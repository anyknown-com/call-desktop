//! Sentence → TTS → ordered playback bookkeeping for one assistant turn.

use super::types::SegmentId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SegState {
    Pending,
    Synthesizing,
    Ready,
    Playing,
    Done,
}

#[derive(Debug)]
pub(super) struct Segment {
    pub(super) id: SegmentId,
    pub(super) text: String,
    pub(super) state: SegState,
    pub(super) ended_at: Option<f64>,
}

/// Sentence → TTS → ordered playback, one per assistant turn.
#[derive(Debug)]
pub(super) struct TtsQueue {
    pub(super) segments: Vec<Segment>,
    pub(super) interjections: Vec<SegmentId>,
    pub(super) finished: bool,
    pub(super) drained: bool,
}

impl TtsQueue {
    pub(super) fn new() -> Self {
        Self { segments: vec![], interjections: vec![], finished: false, drained: false }
    }
    pub(super) fn spoken_text(&self) -> String {
        self.segments
            .iter()
            .filter(|s| matches!(s.state, SegState::Playing | SegState::Done))
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
    pub(super) fn echo_horizon_text(&self, now: f64, horizon_ms: f64) -> String {
        self.segments
            .iter()
            .filter(|s| match s.state {
                SegState::Playing => true,
                SegState::Done => s.ended_at.is_some_and(|e| now - e <= horizon_ms),
                _ => false,
            })
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
    pub(super) fn all_done(&self) -> bool {
        self.segments.iter().all(|s| s.state == SegState::Done)
    }
    /// Segments being synthesized or ready-but-not-yet-played count against the lookahead; the
    /// playing one does not. Returns segments to start synthesizing now.
    pub(super) fn pump(&mut self, lookahead: usize) -> Vec<(SegmentId, String)> {
        let in_flight = self.segments.iter().filter(|s| matches!(s.state, SegState::Synthesizing | SegState::Ready)).count();
        let mut budget = (lookahead + 1).saturating_sub(in_flight);
        let mut out = vec![];
        for s in self.segments.iter_mut() {
            if budget == 0 {
                break;
            }
            if s.state != SegState::Pending {
                continue;
            }
            budget -= 1;
            s.state = SegState::Synthesizing;
            out.push((s.id, s.text.clone()));
        }
        out
    }
}
