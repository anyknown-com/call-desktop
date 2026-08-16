//! Enrollment: six accepted ~3 s clips (≥2 s voiced) plus one held-out clip → speaker profile.
//! Uses the audio engine directly (mic → AEC/NS → 16 kHz), Silero VAD for voiced-ms, CAM++.

use anyhow::{anyhow, Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};
use voice_audio::engine::{AudioEngine, EngineConfig};
use voice_core::speaker_profile::{
    build_profile, check_clip_embedding, check_clip_signal, ClipRejection, ProfileBuildError, ProfileBuildInput, ENROLLMENT_CLIPS,
};
use voice_ml::{SileroVad, SpeakerEmbedder, VadConfig, VadDetector, VadEvent};

const CLIP_SECS: f64 = 3.5;

pub async fn run(models_dir: PathBuf, out: PathBuf) -> Result<()> {
    let s = voice_runtime::settings::load();
    let mut embedder = SpeakerEmbedder::new(models_dir.join(voice_ml::models::CAMPPLUS_FILE)).context("load CAM++")?;
    let mut vad = VadDetector::new(SileroVad::new(models_dir.join(voice_ml::models::SILERO_VAD_V5_FILE))?, VadConfig::default());
    let (cap_tx, cap_rx) = channel::<Vec<f32>>();
    let (sink_tx, _sink_rx) = channel();
    let _engine = AudioEngine::start(
        EngineConfig { input_device: s.audio.input_device.clone(), output_device: s.audio.output_device.clone(), apm: Default::default(), input_wav: None },
        cap_tx,
        sink_tx,
    )?;
    println!("Voice enrollment: {ENROLLMENT_CLIPS} clips of ~3 s each, then one extra check clip.");
    println!("Speak naturally (any language), e.g. read a sentence from a book. Press Enter to record each clip.\n");

    let mut accepted: Vec<Vec<f32>> = vec![];
    let mut n = 1;
    while accepted.len() < ENROLLMENT_CLIPS {
        wait_enter(&format!("Clip {n}/{ENROLLMENT_CLIPS} — press Enter, then speak for 3 seconds"))?;
        let pcm = record(&cap_rx, CLIP_SECS);
        match check(&pcm, &mut vad, &mut embedder, &accepted)? {
            Ok(emb) => {
                accepted.push(emb);
                println!("  ✓ accepted\n");
                n += 1;
            }
            Err(r) => println!("  ✗ {}\n", reason(r)),
        }
    }
    let held = loop {
        wait_enter("Check clip — press Enter, then speak for 3 seconds")?;
        let pcm = record(&cap_rx, CLIP_SECS);
        // Held-out only needs signal checks (it is scored against the centroid below).
        match check_clip_signal(&pcm, voiced_ms(&pcm, &mut vad)?) {
            Some(r) => println!("  ✗ {}\n", reason(r)),
            None => break embedder.embed(&pcm)?,
        }
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as f64;
    match build_profile(ProfileBuildInput {
        enrollment: &accepted,
        held_out: &held,
        max_local_negative: None,
        model_sha256: voice_ml::CAMPPLUS_SHA256,
        frontend_version: voice_core::fbank::FRONTEND_VERSION,
        now,
        global: None,
    }) {
        Ok(profile) => {
            std::fs::create_dir_all(out.parent().unwrap())?;
            std::fs::write(&out, serde_json::to_vec_pretty(&profile)?)?;
            println!("✓ profile saved: {} (held-out score {:.3}, θ_high {:.2}, full-turn {:.2})", out.display(), profile.held_out_score, profile.thresholds.streaming_high, profile.thresholds.full_turn);
            println!("Media mode is now available: `voice call --media`.");
            Ok(())
        }
        Err(ProfileBuildError::HeldOutBelowThreshold { held_out_score }) => Err(anyhow!(
            "held-out clip scored {held_out_score:.3}, below the required margin — media mode would be unreliable with this mic/room. Try again closer to the mic, in a quieter spot."
        )),
        Err(ProfileBuildError::WrongClipCount) => unreachable!(),
    }
}

fn wait_enter(prompt: &str) -> Result<()> {
    print!("{prompt} ");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(())
}

fn record(rx: &std::sync::mpsc::Receiver<Vec<f32>>, secs: f64) -> Vec<f32> {
    // Drop whatever queued up while the user was reading the prompt.
    while rx.try_recv().is_ok() {}
    print!("  ● recording… ");
    let _ = std::io::stdout().flush();
    let mut pcm = Vec::with_capacity((secs * 16000.0) as usize);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs_f64(secs) {
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            pcm.extend(chunk);
        }
    }
    println!("done");
    pcm
}

fn voiced_ms(pcm: &[f32], vad: &mut VadDetector) -> Result<f64> {
    vad.reset();
    let mut speech_frames = 0usize;
    for f in pcm.chunks_exact(512) {
        let (prob, _ev) = vad.process(f)?;
        if prob >= vad.config().positive_speech_threshold {
            speech_frames += 1;
        }
    }
    let _ = VadEvent::Start; // (events unused here; we count frames)
    Ok(speech_frames as f64 * 32.0)
}

fn check(pcm: &[f32], vad: &mut VadDetector, emb: &mut SpeakerEmbedder, accepted: &[Vec<f32>]) -> Result<std::result::Result<Vec<f32>, ClipRejection>> {
    let vm = voiced_ms(pcm, vad)?;
    if let Some(r) = check_clip_signal(pcm, vm) {
        return Ok(Err(r));
    }
    let e = emb.embed(pcm)?;
    if let Some(r) = check_clip_embedding(&e, accepted) {
        return Ok(Err(r));
    }
    Ok(Ok(e))
}

fn reason(r: ClipRejection) -> &'static str {
    match r {
        ClipRejection::InsufficientVoiced => "not enough speech (need ≥2 s) — keep talking for the whole 3 s",
        ClipRejection::Clipping => "clipping — move back from the mic or lower input gain",
        ClipRejection::LowLevel => "too quiet — speak up or move closer",
        ClipRejection::InconsistentEmbedding => "sounded different from your other clips — same voice, same distance please",
        ClipRejection::Duplicate => "looks like a duplicate capture — try again",
    }
}
