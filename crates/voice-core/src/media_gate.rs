//! Media-mode streaming speaker-verification gate (FINAL v1).
//!
//! Pure state machine — no I/O, no clock, no ONNX. Driven by timestamped generic-VAD events and
//! 1.6 s-window speaker scores; emits actions. Generic VAD NEVER changes playback: only two
//! consecutive high speaker scores commit (cut playback).
//!
//!   MONITORING → CANDIDATE → CONFIRMING → COMMITTED → VERIFYING → MONITORING
//!
//! Port of voice/src/core/speaker/media-gate.ts. Times are ms as f64 (may be negative).

use crate::thresholds::ResolvedThresholds;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateState {
    Monitoring,
    Candidate,
    Confirming,
    Committed,
    Verifying,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GateEvent {
    Vad { active: bool, t: f64 },
    Score { value: f64, t: f64, window_start: f64, window_end: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEndReason {
    Silence,
    LowScores,
    TurnCap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    BelowThreshold,
    TooShort,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GateAction {
    /// Cut AI playback and start collecting the committed turn.
    Commit { t: f64, turn_start: f64 },
    /// Turn ended; run full-turn verification on [turn_start, turn_end].
    Finalize { t: f64, turn_start: f64, turn_end: f64, reason: TurnEndReason },
    /// Full-turn verification passed; send audio to STT.
    Accept { t: f64, turn_start: f64, turn_end: f64 },
    /// Full-turn verification failed; show subtle "voice not verified" hint, skip STT.
    Reject { t: f64, reason: RejectReason },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaGateConfig {
    pub thresholds: ResolvedThresholds,
    /// Scoring window length (ms).
    pub window_ms: f64,
    /// Consecutive scores >= θ_high required to commit.
    pub commit_consecutive: u32,
    /// Consecutive scores < θ_low that end a committed turn.
    pub absence_consecutive: u32,
    /// Candidate epoch rollover (ms).
    pub candidate_epoch_ms: f64,
    /// Committed turn cap (ms).
    pub turn_cap_ms: f64,
    /// Tail retained after the last target-positive window (ms).
    pub retained_tail_ms: f64,
    /// Turn start = VAD onset − this pad, when onset is recent enough.
    pub pre_onset_pad_ms: f64,
    /// How recent the VAD onset must be (ms) to be used as turn start.
    pub onset_lookback_ms: f64,
}

impl MediaGateConfig {
    pub fn default_for(thresholds: ResolvedThresholds) -> Self {
        Self {
            thresholds,
            window_ms: 1600.0,
            commit_consecutive: 2,
            absence_consecutive: 4,
            candidate_epoch_ms: 20_000.0,
            turn_cap_ms: 20_000.0,
            retained_tail_ms: 400.0,
            pre_onset_pad_ms: 200.0,
            onset_lookback_ms: 2000.0,
        }
    }
}

#[derive(Debug)]
pub struct MediaGate {
    cfg: MediaGateConfig,
    st: GateState,
    vad_active: bool,
    last_vad_onset: f64,
    epoch_start: f64,
    consecutive_high: u32,
    consecutive_low: u32,
    first_high_window_start: f64,
    turn_start: f64,
    last_evidence_end: f64,
    pending_turn: Option<(f64, f64)>,
}

impl MediaGate {
    pub fn new(cfg: MediaGateConfig) -> Self {
        Self {
            cfg,
            st: GateState::Monitoring,
            vad_active: false,
            last_vad_onset: f64::NEG_INFINITY,
            epoch_start: 0.0,
            consecutive_high: 0,
            consecutive_low: 0,
            first_high_window_start: 0.0,
            turn_start: 0.0,
            last_evidence_end: 0.0,
            pending_turn: None,
        }
    }

    pub fn state(&self) -> GateState {
        self.st
    }

    pub fn handle(&mut self, ev: GateEvent) -> Vec<GateAction> {
        match ev {
            GateEvent::Vad { active, t } => self.handle_vad(active, t),
            GateEvent::Score { value, t, window_start, window_end } => self.handle_score(value, t, window_start, window_end),
        }
    }

    /// Resolve VERIFYING after the full-turn embedding check.
    /// `accepted` = full-turn score passed threshold AND ≥1.2 s voiced audio.
    pub fn resolve_verification(&mut self, accepted: bool, t: f64, reason: RejectReason) -> Vec<GateAction> {
        if self.st != GateState::Verifying {
            return vec![];
        }
        let Some((turn_start, turn_end)) = self.pending_turn.take() else { return vec![] };
        self.st = GateState::Monitoring;
        // If VAD is still active (e.g. media kept talking), re-enter CANDIDATE at once.
        if self.vad_active {
            self.enter_candidate(t);
        }
        if accepted {
            vec![GateAction::Accept { t, turn_start, turn_end }]
        } else {
            vec![GateAction::Reject { t, reason }]
        }
    }

    fn handle_vad(&mut self, active: bool, t: f64) -> Vec<GateAction> {
        self.vad_active = active;
        if active {
            self.last_vad_onset = t;
            if self.st == GateState::Monitoring {
                self.enter_candidate(t);
            }
            return vec![];
        }
        // VAD silence (the adapter reports this after its 700 ms redemption).
        match self.st {
            GateState::Candidate | GateState::Confirming => {
                self.st = GateState::Monitoring;
                self.reset_counters();
                vec![]
            }
            GateState::Committed => self.finalize(t, TurnEndReason::Silence, t),
            _ => vec![],
        }
    }

    fn handle_score(&mut self, value: f64, t: f64, window_start: f64, window_end: f64) -> Vec<GateAction> {
        let ResolvedThresholds { streaming_high, streaming_low, .. } = self.cfg.thresholds;
        match self.st {
            GateState::Candidate | GateState::Confirming => {
                self.rollover_epoch(t);
                if value >= streaming_high {
                    if self.consecutive_high == 0 {
                        self.first_high_window_start = window_start;
                    }
                    self.consecutive_high += 1;
                    if self.consecutive_high >= self.cfg.commit_consecutive {
                        return self.commit(t, window_end);
                    }
                    self.st = GateState::Confirming;
                } else {
                    // Any non-high score (including grey zone) resets confirmation.
                    self.consecutive_high = 0;
                    self.st = GateState::Candidate;
                }
                vec![]
            }
            GateState::Committed => {
                if value >= streaming_high {
                    self.last_evidence_end = window_end;
                    self.consecutive_low = 0;
                } else if value < streaming_low {
                    self.consecutive_low += 1;
                    if self.consecutive_low >= self.cfg.absence_consecutive {
                        let end = (self.last_evidence_end + self.cfg.retained_tail_ms).min(t);
                        return self.finalize(t, TurnEndReason::LowScores, end);
                    }
                } else {
                    // Grey zone: neither evidence nor absence.
                    self.consecutive_low = 0;
                }
                if t - self.turn_start >= self.cfg.turn_cap_ms {
                    let end = (self.last_evidence_end + self.cfg.retained_tail_ms).min(t);
                    return self.finalize(t, TurnEndReason::TurnCap, end);
                }
                vec![]
            }
            // MONITORING (stale score) or VERIFYING: ignore.
            _ => vec![],
        }
    }

    fn commit(&mut self, t: f64, window_end: f64) -> Vec<GateAction> {
        // Recent VAD onset minus pad when available, else start of first high window.
        let onset_recent = t - self.last_vad_onset <= self.cfg.onset_lookback_ms;
        self.turn_start = if onset_recent { self.last_vad_onset - self.cfg.pre_onset_pad_ms } else { self.first_high_window_start };
        self.last_evidence_end = window_end;
        self.consecutive_low = 0;
        self.st = GateState::Committed;
        vec![GateAction::Commit { t, turn_start: self.turn_start }]
    }

    fn finalize(&mut self, t: f64, reason: TurnEndReason, turn_end: f64) -> Vec<GateAction> {
        let turn_start = self.turn_start;
        let turn_end = turn_start.max(turn_end);
        self.pending_turn = Some((turn_start, turn_end));
        self.st = GateState::Verifying;
        self.reset_counters();
        vec![GateAction::Finalize { t, turn_start, turn_end, reason }]
    }

    fn enter_candidate(&mut self, t: f64) {
        self.st = GateState::Candidate;
        self.epoch_start = t;
        self.reset_counters();
    }

    /// At the epoch cap, reset bookkeeping but keep listening without a gap.
    fn rollover_epoch(&mut self, t: f64) {
        if t - self.epoch_start >= self.cfg.candidate_epoch_ms {
            self.epoch_start = t;
            self.consecutive_high = 0;
            self.st = GateState::Candidate;
        }
    }

    fn reset_counters(&mut self) {
        self.consecutive_high = 0;
        self.consecutive_low = 0;
    }
}
