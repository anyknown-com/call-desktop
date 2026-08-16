//! Media-mode orchestration: continuous 16 kHz mic PCM + generic VAD events in; playback-cut /
//! verified-turn events out. Owns the 4 s ring, the 1.6 s / 320 ms scoring cadence, committed-turn
//! collection and the full-turn verification pass. Decisions live in `voice_core::media_gate`;
//! inference lives behind [`SpeakerScorer`]. Timestamps are stream time (ms of audio pushed), so
//! behavior is replayable. Port of media-turn-controller.ts, callbacks → returned events.

use crate::ring::PcmRingBuffer;
use voice_core::media_gate::{GateAction, GateEvent, GateState, MediaGate, MediaGateConfig, RejectReason};
use voice_core::speaker_profile::{score_embedding, SpeakerProfile};

/// Speaker embedding inference (CAM++). Blocking; called on the pipeline thread.
pub trait SpeakerScorer {
    fn embed(&mut self, pcm16k: &[f32]) -> anyhow::Result<Vec<f32>>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum MediaTurnEvent {
    /// Confirmed target speaker: cut AI playback now.
    CutPlayback,
    /// Verified turn: send to STT (16 kHz mono).
    TurnAccepted(Vec<f32>),
    /// Turn rejected after playback was already cut: show a subtle hint.
    TurnRejected(RejectReason),
    StateChanged(GateState),
    Error(String),
}

pub struct MediaTurnOptions {
    pub window_ms: f64,
    pub hop_ms: f64,
    pub min_voiced_ms: f64,
    pub ring_ms: f64,
}

impl Default for MediaTurnOptions {
    fn default() -> Self {
        Self { window_ms: 1600.0, hop_ms: 320.0, min_voiced_ms: 1200.0, ring_ms: 4000.0 }
    }
}

struct Turn {
    start_ms: f64,
    chunks: Vec<f32>,
    collected_from_ms: f64,
    voiced_ms: f64,
}

pub struct MediaTurnController<S: SpeakerScorer> {
    profile: SpeakerProfile,
    scorer: S,
    gate: MediaGate,
    ring: PcmRingBuffer,
    sample_rate: u32,
    opts: MediaTurnOptions,
    turn_cap_ms: f64,
    vad_active: bool,
    voiced_since_candidate_ms: f64,
    last_score_at_ms: f64,
    turn: Option<Turn>,
}

const SR: u32 = 16_000;

impl<S: SpeakerScorer> MediaTurnController<S> {
    pub fn new(profile: SpeakerProfile, scorer: S, opts: MediaTurnOptions, gate_cfg: Option<MediaGateConfig>) -> Self {
        let cfg = gate_cfg.unwrap_or_else(|| MediaGateConfig::default_for(profile.thresholds.into()));
        Self {
            ring: PcmRingBuffer::new(opts.ring_ms, SR),
            gate: MediaGate::new(cfg),
            turn_cap_ms: cfg.turn_cap_ms,
            profile,
            scorer,
            sample_rate: SR,
            opts,
            vad_active: false,
            voiced_since_candidate_ms: 0.0,
            last_score_at_ms: f64::NEG_INFINITY,
            turn: None,
        }
    }

    pub fn state(&self) -> GateState {
        self.gate.state()
    }
    /// Current stream time in ms (total audio pushed).
    pub fn now_ms(&self) -> f64 {
        self.ring.end_ms()
    }

    /// Generic VAD speech start. Never touches playback by itself.
    pub fn vad_start(&mut self) -> Vec<MediaTurnEvent> {
        if self.vad_active {
            return vec![];
        }
        self.vad_active = true;
        self.voiced_since_candidate_ms = 0.0;
        let actions = self.gate.handle(GateEvent::Vad { active: true, t: self.now_ms() });
        self.apply(actions)
    }

    /// Generic VAD silence end (after the VAD's own redemption window).
    pub fn vad_end(&mut self) -> Vec<MediaTurnEvent> {
        if !self.vad_active {
            return vec![];
        }
        self.vad_active = false;
        let actions = self.gate.handle(GateEvent::Vad { active: false, t: self.now_ms() });
        self.apply(actions)
    }

    /// Continuous mono PCM at 16 kHz.
    pub fn push_frame(&mut self, frame: &[f32]) -> Vec<MediaTurnEvent> {
        self.ring.write(frame);
        let frame_ms = frame.len() as f64 / self.sample_rate as f64 * 1000.0;
        if self.vad_active {
            self.voiced_since_candidate_ms += frame_ms;
            if let Some(t) = &mut self.turn {
                t.voiced_ms += frame_ms;
            }
        }
        let now = self.now_ms();
        if let Some(t) = &mut self.turn {
            if now - t.start_ms <= self.turn_cap_ms + 1000.0 {
                t.chunks.extend_from_slice(frame);
            }
        }
        self.maybe_score()
    }

    fn maybe_score(&mut self) -> Vec<MediaTurnEvent> {
        let st = self.gate.state();
        let now = self.now_ms();
        if now < self.opts.window_ms || now - self.last_score_at_ms < self.opts.hop_ms {
            return vec![];
        }
        let candidate = matches!(st, GateState::Candidate | GateState::Confirming) && self.voiced_since_candidate_ms >= self.opts.min_voiced_ms;
        let committed = st == GateState::Committed;
        if !candidate && !committed {
            return vec![];
        }
        self.last_score_at_ms = now;
        let window_start = now - self.opts.window_ms;
        let pcm = self.ring.slice(window_start, now);
        match self.scorer.embed(&pcm) {
            Ok(emb) => {
                let value = score_embedding(&self.profile, &emb);
                let actions = self.gate.handle(GateEvent::Score { value, t: now, window_start, window_end: now });
                self.apply(actions)
            }
            Err(e) => vec![MediaTurnEvent::Error(e.to_string())],
        }
    }

    fn apply(&mut self, actions: Vec<GateAction>) -> Vec<MediaTurnEvent> {
        let mut out = vec![];
        for a in actions {
            match a {
                GateAction::Commit { turn_start, .. } => {
                    // Backfill [turn_start, now] from the ring; later frames are appended live.
                    let start_ms = turn_start.max(self.ring.start_ms());
                    let now = self.now_ms();
                    self.turn = Some(Turn {
                        start_ms,
                        chunks: self.ring.slice(start_ms, now),
                        collected_from_ms: start_ms,
                        voiced_ms: now - start_ms, // VAD was active throughout confirmation
                    });
                    out.push(MediaTurnEvent::CutPlayback);
                }
                GateAction::Finalize { turn_start, turn_end, .. } => {
                    out.extend(self.verify(turn_start, turn_end));
                }
                GateAction::Accept { turn_start, turn_end, .. } => {
                    let audio = self.take_turn_audio(turn_start, turn_end, false);
                    out.push(MediaTurnEvent::TurnAccepted(audio));
                }
                GateAction::Reject { reason, .. } => {
                    self.turn = None;
                    out.push(MediaTurnEvent::TurnRejected(reason));
                }
            }
        }
        out.push(MediaTurnEvent::StateChanged(self.gate.state()));
        out
    }

    fn verify(&mut self, turn_start: f64, turn_end: f64) -> Vec<MediaTurnEvent> {
        let audio = self.take_turn_audio(turn_start, turn_end, true);
        let min_samples = (self.opts.min_voiced_ms / 1000.0 * self.sample_rate as f64) as usize;
        let too_short = self.turn.as_ref().is_none_or(|t| t.voiced_ms < self.opts.min_voiced_ms) || audio.len() < min_samples;
        let now = self.now_ms();
        if too_short {
            let actions = self.gate.resolve_verification(false, now, RejectReason::TooShort);
            return self.apply(actions);
        }
        match self.scorer.embed(&audio) {
            Ok(emb) => {
                let score = score_embedding(&self.profile, &emb);
                let ok = score >= self.profile.thresholds.full_turn;
                let actions = self.gate.resolve_verification(ok, now, RejectReason::BelowThreshold);
                self.apply(actions)
            }
            Err(e) => {
                let mut out = vec![MediaTurnEvent::Error(e.to_string())];
                let actions = self.gate.resolve_verification(false, now, RejectReason::BelowThreshold);
                out.extend(self.apply(actions));
                out
            }
        }
    }

    /// Concatenated collected audio trimmed to [turn_start, turn_end].
    fn take_turn_audio(&mut self, turn_start: f64, turn_end: f64, keep: bool) -> Vec<f32> {
        let Some(turn) = &self.turn else { return vec![] };
        let total = turn.chunks.len();
        let sr = self.sample_rate as f64;
        let from = (((turn_start - turn.collected_from_ms) / 1000.0 * sr).round().max(0.0)) as usize;
        let to = ((((turn_end - turn.collected_from_ms) / 1000.0 * sr).round()) as usize).min(total);
        let out = turn.chunks[from.min(total)..to.max(from.min(total))].to_vec();
        if !keep {
            self.turn = None;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voice_core::speaker_profile::ProfileThresholds;

    const DIM: usize = 192;
    const FRAME: usize = 512;

    fn profile() -> SpeakerProfile {
        let mut centroid = vec![0f32; DIM];
        centroid[0] = 1.0;
        SpeakerProfile {
            schema_version: 1,
            model_sha256: "sha".into(),
            frontend_version: 1,
            created_at: 0.0,
            centroid: centroid.clone(),
            enrollment: vec![centroid.clone()],
            held_out: centroid,
            held_out_score: 0.9,
            max_local_negative: None,
            thresholds: ProfileThresholds { streaming_high: 0.62, streaming_low: 0.5, full_turn: 0.55 },
            threshold_policy_version: 1,
        }
    }

    /// Label-encoded scorer: user samples +0.5, media −0.5. score = 0.2 + 0.65·(user fraction).
    struct LabelScorer {
        calls: usize,
    }
    impl SpeakerScorer for LabelScorer {
        fn embed(&mut self, pcm: &[f32]) -> anyhow::Result<Vec<f32>> {
            self.calls += 1;
            let user = pcm.iter().filter(|v| **v > 0.25).count();
            let score = 0.2 + 0.65 * if pcm.is_empty() { 0.0 } else { user as f64 / pcm.len() as f64 };
            let mut e = vec![0f32; DIM];
            e[0] = score as f32;
            e[1] = (1.0 - score * score).max(0.0).sqrt() as f32;
            Ok(e)
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        User,
        Media,
        Silence,
    }

    struct H {
        ctl: MediaTurnController<LabelScorer>,
        vad_active: bool,
        cuts: Vec<f64>,
        accepted: Vec<Vec<f32>>,
        rejected: Vec<RejectReason>,
    }
    impl H {
        fn new() -> H {
            H { ctl: MediaTurnController::new(profile(), LabelScorer { calls: 0 }, MediaTurnOptions::default(), None), vad_active: false, cuts: vec![], accepted: vec![], rejected: vec![] }
        }
        fn collect(&mut self, evs: Vec<MediaTurnEvent>) {
            for e in evs {
                match e {
                    MediaTurnEvent::CutPlayback => self.cuts.push(self.ctl.now_ms()),
                    MediaTurnEvent::TurnAccepted(a) => self.accepted.push(a),
                    MediaTurnEvent::TurnRejected(r) => self.rejected.push(r),
                    _ => {}
                }
            }
        }
        fn run(&mut self, kind: Kind, ms: usize) {
            let active = kind != Kind::Silence;
            if active && !self.vad_active {
                let e = self.ctl.vad_start();
                self.collect(e);
            }
            if !active && self.vad_active {
                let e = self.ctl.vad_end();
                self.collect(e);
            }
            self.vad_active = active;
            let v = match kind {
                Kind::User => 0.5,
                Kind::Media => -0.5,
                Kind::Silence => 0.0,
            };
            let frame = vec![v; FRAME];
            let mut t = 0;
            while t < ms {
                let e = self.ctl.push_frame(&frame);
                self.collect(e);
                t += 32;
            }
        }
    }

    #[test]
    fn clear_user_turn_commits_cuts_once_and_is_accepted() {
        let mut h = H::new();
        h.run(Kind::Silence, 2000);
        h.run(Kind::User, 3000);
        h.run(Kind::Silence, 1000);
        assert_eq!(h.cuts.len(), 1);
        let lat = h.cuts[0] - 2000.0;
        assert!((1200.0..=2200.0).contains(&lat), "latency {lat}");
        assert_eq!(h.accepted.len(), 1);
        assert!(h.rejected.is_empty());
        let sec = h.accepted[0].len() as f64 / 16000.0;
        assert!(sec > 2.5 && sec < 4.0, "{sec}");
    }

    #[test]
    fn media_alone_never_commits() {
        let mut h = H::new();
        h.run(Kind::Media, 30_000);
        h.run(Kind::Silence, 1000);
        assert!(h.cuts.is_empty() && h.accepted.is_empty() && h.rejected.is_empty());
        assert!(h.ctl.scorer.calls > 10);
    }

    #[test]
    fn short_speech_cannot_interrupt() {
        let mut h = H::new();
        h.run(Kind::Silence, 1000);
        h.run(Kind::User, 800);
        h.run(Kind::Silence, 2000);
        assert!(h.cuts.is_empty() && h.accepted.is_empty());
    }

    #[test]
    fn user_during_media_commits_and_ends_on_low_scores() {
        let mut h = H::new();
        h.run(Kind::Media, 8000);
        h.run(Kind::User, 4000);
        let cuts_after_user = h.cuts.len();
        h.run(Kind::Media, 8000);
        h.run(Kind::Silence, 500);
        assert_eq!(cuts_after_user, 1);
        assert_eq!(h.cuts.len(), 1);
        assert_eq!(h.accepted.len(), 1);
    }

    #[test]
    fn turn_cap_finalizes_endless_monologue() {
        let mut h = H::new();
        h.run(Kind::User, 26_000);
        assert_eq!(h.accepted.len(), 1);
        assert_eq!(h.cuts.len(), 2);
        let sec = h.accepted[0].len() as f64 / 16000.0;
        assert!(sec > 15.0 && sec <= 21.0, "{sec}");
    }
}
