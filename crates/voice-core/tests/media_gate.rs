//! 1:1 port of voice/test/media-gate.test.ts.
use voice_core::media_gate::*;
use voice_core::thresholds::ResolvedThresholds;

const THRESHOLDS: ResolvedThresholds = ResolvedThresholds { streaming_high: 0.62, streaming_low: 0.5, full_turn: 0.55 };
const HIGH: f64 = 0.8;
const GREY: f64 = 0.55; // between θ_low and θ_high
const LOW: f64 = 0.2;

fn gate() -> MediaGate {
    MediaGate::new(MediaGateConfig::default_for(THRESHOLDS))
}
fn vad(g: &mut MediaGate, active: bool, t: f64) -> Vec<GateAction> {
    g.handle(GateEvent::Vad { active, t })
}
fn score(g: &mut MediaGate, value: f64, t: f64) -> Vec<GateAction> {
    g.handle(GateEvent::Score { value, t, window_start: t - 1600.0, window_end: t })
}
/// Drive a fresh gate to COMMITTED: VAD onset at 0, highs at 1200 and 1520.
fn committed() -> (MediaGate, Vec<GateAction>) {
    let mut g = gate();
    vad(&mut g, true, 0.0);
    score(&mut g, HIGH, 1200.0);
    let a = score(&mut g, HIGH, 1520.0);
    (g, a)
}

// MONITORING
#[test]
fn vad_onset_enters_candidate_without_action() {
    let mut g = gate();
    assert!(vad(&mut g, true, 0.0).is_empty());
    assert_eq!(g.state(), GateState::Candidate);
}
#[test]
fn ignores_stale_scores() {
    let mut g = gate();
    assert!(score(&mut g, HIGH, 100.0).is_empty());
    assert_eq!(g.state(), GateState::Monitoring);
}

// CANDIDATE
#[test]
fn first_high_moves_to_confirming() {
    let mut g = gate();
    vad(&mut g, true, 0.0);
    assert!(score(&mut g, HIGH, 1200.0).is_empty());
    assert_eq!(g.state(), GateState::Confirming);
}
#[test]
fn low_and_grey_keep_candidate() {
    let mut g = gate();
    vad(&mut g, true, 0.0);
    score(&mut g, LOW, 1200.0);
    assert_eq!(g.state(), GateState::Candidate);
    score(&mut g, GREY, 1520.0);
    assert_eq!(g.state(), GateState::Candidate);
}
#[test]
fn vad_silence_returns_to_monitoring() {
    let mut g = gate();
    vad(&mut g, true, 0.0);
    score(&mut g, HIGH, 1200.0);
    assert!(vad(&mut g, false, 2000.0).is_empty());
    assert_eq!(g.state(), GateState::Monitoring);
}
#[test]
fn epoch_rollover_resets_confirmation_but_keeps_listening() {
    let mut g = gate();
    vad(&mut g, true, 0.0);
    score(&mut g, HIGH, 19_900.0);
    assert_eq!(g.state(), GateState::Confirming);
    score(&mut g, HIGH, 20_220.0); // epoch expired: counters reset BEFORE scoring
    assert_eq!(g.state(), GateState::Confirming); // this high became the new first high
    let a = score(&mut g, HIGH, 20_540.0);
    assert!(matches!(a[0], GateAction::Commit { .. }));
}

// CONFIRMING
#[test]
fn two_highs_commit() {
    let (g, a) = committed();
    assert_eq!(g.state(), GateState::Committed);
    assert_eq!(a.len(), 1);
    assert!(matches!(a[0], GateAction::Commit { .. }));
}
#[test]
fn uses_recent_vad_onset_minus_200() {
    let (_, a) = committed();
    assert_eq!(a[0], GateAction::Commit { t: 1520.0, turn_start: -200.0 });
}
#[test]
fn falls_back_to_first_high_window_when_onset_stale() {
    let mut g = gate();
    vad(&mut g, true, 0.0);
    let mut t = 1200.0;
    while t < 8000.0 {
        score(&mut g, LOW, t);
        t += 320.0;
    }
    score(&mut g, HIGH, 8200.0);
    let a = score(&mut g, HIGH, 8520.0);
    assert_eq!(a[0], GateAction::Commit { t: 8520.0, turn_start: 8200.0 - 1600.0 });
}
#[test]
fn grey_resets_confirmation() {
    let mut g = gate();
    vad(&mut g, true, 0.0);
    score(&mut g, HIGH, 1200.0);
    score(&mut g, GREY, 1520.0);
    assert_eq!(g.state(), GateState::Candidate);
    score(&mut g, HIGH, 1840.0);
    assert!(matches!(score(&mut g, HIGH, 2160.0)[0], GateAction::Commit { .. }));
}
#[test]
fn vad_silence_returns_to_monitoring_without_commit() {
    let mut g = gate();
    vad(&mut g, true, 0.0);
    score(&mut g, HIGH, 1200.0);
    vad(&mut g, false, 1900.0);
    assert_eq!(g.state(), GateState::Monitoring);
    vad(&mut g, true, 3000.0);
    score(&mut g, HIGH, 4200.0);
    assert_eq!(g.state(), GateState::Confirming);
}

// COMMITTED
#[test]
fn vad_silence_finalizes_with_silence_reason() {
    let (mut g, _) = committed();
    let a = vad(&mut g, false, 4000.0);
    assert_eq!(a, vec![GateAction::Finalize { t: 4000.0, turn_start: -200.0, turn_end: 4000.0, reason: TurnEndReason::Silence }]);
    assert_eq!(g.state(), GateState::Verifying);
}
#[test]
fn four_lows_finalize() {
    let (mut g, _) = committed();
    score(&mut g, LOW, 1840.0);
    score(&mut g, LOW, 2160.0);
    score(&mut g, LOW, 2480.0);
    let a = score(&mut g, LOW, 2800.0);
    assert_eq!(a.len(), 1);
    match a[0] {
        GateAction::Finalize { reason, turn_end, .. } => {
            assert_eq!(reason, TurnEndReason::LowScores);
            assert_eq!(turn_end, 1920.0); // last evidence (1520) + 400 tail
        }
        _ => panic!("expected finalize"),
    }
}
#[test]
fn high_resets_low_counter_and_extends_evidence() {
    let (mut g, _) = committed();
    for t in [1840.0, 2160.0, 2480.0] {
        score(&mut g, LOW, t);
    }
    score(&mut g, HIGH, 2800.0);
    for t in [3120.0, 3440.0, 3760.0] {
        score(&mut g, LOW, t);
    }
    let a = score(&mut g, LOW, 4080.0);
    match a[0] {
        GateAction::Finalize { turn_end, .. } => assert_eq!(turn_end, 2800.0 + 400.0),
        _ => panic!("expected finalize"),
    }
}
#[test]
fn grey_neither_ends_nor_counts() {
    let (mut g, _) = committed();
    let mut t = 1840.0;
    while t < 8000.0 {
        assert!(score(&mut g, GREY, t).is_empty());
        t += 320.0;
    }
    assert_eq!(g.state(), GateState::Committed);
}
#[test]
fn turn_cap_finalizes_even_when_high() {
    let (mut g, _) = committed();
    let mut fin = None;
    let mut t = 1840.0;
    while t < 25_000.0 && fin.is_none() {
        fin = score(&mut g, HIGH, t).first().copied();
        t += 320.0;
    }
    match fin {
        Some(GateAction::Finalize { reason, .. }) => assert_eq!(reason, TurnEndReason::TurnCap),
        other => panic!("expected finalize, got {other:?}"),
    }
    assert_eq!(g.state(), GateState::Verifying);
}

// VERIFYING
#[test]
fn accept_emits_accept_and_returns_to_monitoring() {
    let (mut g, _) = committed();
    vad(&mut g, false, 4000.0);
    let a = g.resolve_verification(true, 4100.0, RejectReason::BelowThreshold);
    assert_eq!(a, vec![GateAction::Accept { t: 4100.0, turn_start: -200.0, turn_end: 4000.0 }]);
    assert_eq!(g.state(), GateState::Monitoring);
}
#[test]
fn reject_emits_reject() {
    let (mut g, _) = committed();
    vad(&mut g, false, 4000.0);
    let a = g.resolve_verification(false, 4100.0, RejectReason::BelowThreshold);
    assert_eq!(a, vec![GateAction::Reject { t: 4100.0, reason: RejectReason::BelowThreshold }]);
    assert_eq!(g.state(), GateState::Monitoring);
}
#[test]
fn too_short_reject_reason() {
    let (mut g, _) = committed();
    vad(&mut g, false, 2000.0);
    let a = g.resolve_verification(false, 2100.0, RejectReason::TooShort);
    assert_eq!(a, vec![GateAction::Reject { t: 2100.0, reason: RejectReason::TooShort }]);
}
#[test]
fn ignores_events_while_verifying_and_resumes_candidate_if_vad_active() {
    let (mut g, _) = committed();
    for t in [1840.0, 2160.0, 2480.0, 2800.0] {
        score(&mut g, LOW, t);
    }
    assert_eq!(g.state(), GateState::Verifying);
    assert!(score(&mut g, HIGH, 3120.0).is_empty());
    g.resolve_verification(false, 3200.0, RejectReason::BelowThreshold);
    assert_eq!(g.state(), GateState::Candidate); // VAD never went silent
}
#[test]
fn double_resolution_is_noop() {
    let (mut g, _) = committed();
    vad(&mut g, false, 4000.0);
    g.resolve_verification(true, 4100.0, RejectReason::BelowThreshold);
    assert!(g.resolve_verification(true, 4200.0, RejectReason::BelowThreshold).is_empty());
}
