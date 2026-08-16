//! Enrollment session shared by the CLI and the app: six accepted ~3 s clips (≥2 s voiced) plus
//! one held-out clip → speaker profile. Blocking; drive it from a background thread.

use crate::settings::Settings;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};
use voice_audio::engine::{AudioEngine, EngineConfig};
use voice_core::speaker_profile::{
    build_profile, check_clip_embedding, check_clip_signal, ClipRejection, ProfileBuildError, ProfileBuildInput, SpeakerProfile, ENROLLMENT_CLIPS,
};
use voice_ml::{SileroVad, SpeakerEmbedder, VadConfig, VadDetector};

pub const CLIP_SECS: f64 = 3.5;

pub fn profile_path() -> Option<PathBuf> {
    crate::settings::dirs().map(|d| d.config_dir().join("speaker-profile.json"))
}

pub fn load_profile() -> Option<SpeakerProfile> {
    let p = profile_path()?;
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

pub fn delete_profile() -> Result<()> {
    if let Some(p) = profile_path() {
        if p.exists() {
            std::fs::remove_file(p)?;
        }
    }
    Ok(())
}

pub fn describe_rejection(r: ClipRejection) -> &'static str {
    match r {
        ClipRejection::InsufficientVoiced => "not enough speech (need ≥2 s) — keep talking for the whole clip",
        ClipRejection::Clipping => "clipping — move back from the mic or lower input gain",
        ClipRejection::LowLevel => "too quiet — speak up or move closer",
        ClipRejection::InconsistentEmbedding => "sounded different from your other clips — same voice, same distance please",
        ClipRejection::Duplicate => "looks like a duplicate capture — try again",
    }
}

pub enum FinishError {
    Clip(ClipRejection),
    HeldOutBelowThreshold { held_out_score: f64 },
}

pub struct Enroller {
    _engine: AudioEngine,
    cap_rx: Receiver<Vec<f32>>,
    vad: VadDetector,
    embedder: SpeakerEmbedder,
    accepted: Vec<Vec<f32>>,
}

impl Enroller {
    pub fn new(settings: &Settings, models_dir: &Path) -> Result<Self> {
        let embedder = SpeakerEmbedder::new(models_dir.join(voice_ml::models::CAMPPLUS_FILE)).context("load CAM++")?;
        let vad = VadDetector::new(SileroVad::new(models_dir.join(voice_ml::models::SILERO_VAD_V5_FILE))?, VadConfig::default());
        let (cap_tx, cap_rx) = channel::<Vec<f32>>();
        let (sink_tx, _sink_rx) = channel();
        let engine = AudioEngine::start(
            EngineConfig {
                input_device: settings.audio.input_device.clone(),
                output_device: settings.audio.output_device.clone(),
                apm: Default::default(),
                input_wav: None,
            },
            cap_tx,
            sink_tx,
        )?;
        Ok(Self { _engine: engine, cap_rx, vad, embedder, accepted: vec![] })
    }

    pub fn accepted(&self) -> usize {
        self.accepted.len()
    }
    pub fn needed(&self) -> usize {
        ENROLLMENT_CLIPS
    }
    pub fn complete(&self) -> bool {
        self.accepted.len() >= ENROLLMENT_CLIPS
    }

    /// Blocking: record `secs` of mic audio (16 kHz mono after AEC/NS).
    pub fn record(&mut self, secs: f64) -> Vec<f32> {
        while self.cap_rx.try_recv().is_ok() {} // drop what queued while idle
        let mut pcm = Vec::with_capacity((secs * 16000.0) as usize);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs_f64(secs) {
            if let Ok(chunk) = self.cap_rx.recv_timeout(Duration::from_millis(200)) {
                pcm.extend(chunk);
            }
        }
        pcm
    }

    fn voiced_ms(&mut self, pcm: &[f32]) -> Result<f64> {
        self.vad.reset();
        let mut n = 0usize;
        for f in pcm.chunks_exact(512) {
            let (prob, _) = self.vad.process(f)?;
            if prob >= self.vad.config().positive_speech_threshold {
                n += 1;
            }
        }
        Ok(n as f64 * 32.0)
    }

    /// Check an enrollment clip and keep it if it passes. Returns the accepted count.
    pub fn submit_clip(&mut self, pcm: &[f32]) -> Result<std::result::Result<usize, ClipRejection>> {
        let vm = self.voiced_ms(pcm)?;
        if let Some(r) = check_clip_signal(pcm, vm) {
            return Ok(Err(r));
        }
        let e = self.embedder.embed(pcm)?;
        if let Some(r) = check_clip_embedding(&e, &self.accepted) {
            return Ok(Err(r));
        }
        self.accepted.push(e);
        Ok(Ok(self.accepted.len()))
    }

    /// Check the held-out clip, build and save the profile.
    pub fn finish(&mut self, held_pcm: &[f32]) -> Result<std::result::Result<SpeakerProfile, FinishError>> {
        let vm = self.voiced_ms(held_pcm)?;
        if let Some(r) = check_clip_signal(held_pcm, vm) {
            return Ok(Err(FinishError::Clip(r)));
        }
        let held = self.embedder.embed(held_pcm)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as f64;
        match build_profile(ProfileBuildInput {
            enrollment: &self.accepted,
            held_out: &held,
            max_local_negative: None,
            model_sha256: voice_ml::CAMPPLUS_SHA256,
            frontend_version: voice_core::fbank::FRONTEND_VERSION,
            now,
            global: None,
        }) {
            Ok(profile) => {
                let p = profile_path().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
                std::fs::create_dir_all(p.parent().unwrap())?;
                std::fs::write(&p, serde_json::to_vec_pretty(&profile)?)?;
                Ok(Ok(profile))
            }
            Err(ProfileBuildError::HeldOutBelowThreshold { held_out_score }) => Ok(Err(FinishError::HeldOutBelowThreshold { held_out_score })),
            Err(ProfileBuildError::WrongClipCount) => Err(anyhow::anyhow!("enrollment incomplete")),
        }
    }
}
