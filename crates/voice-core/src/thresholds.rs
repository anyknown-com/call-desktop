//! Threshold policy (FINAL v1): exactly two calibrated thresholds — the fixed 1.6 s streaming
//! commit threshold and the full-turn acceptance threshold — plus a hysteresis gap that derives
//! θ_low from θ_high. Global values are provisional pending Phase 3 live calibration.

pub const THRESHOLD_POLICY_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlobalThresholds {
    /// Streaming 1.6 s-window commit threshold θ_high.
    pub streaming_high: f64,
    /// Hysteresis gap Δ; θ_low = θ_high − Δ.
    pub hysteresis_gap: f64,
    /// Full-turn acceptance threshold (VERIFYING).
    pub full_turn: f64,
}

pub const GLOBAL_THRESHOLDS_V1: GlobalThresholds =
    GlobalThresholds { streaming_high: 0.62, hysteresis_gap: 0.12, full_turn: 0.55 };

/// Margin the local-negative max must be cleared by when raising θ_high.
pub const LOCAL_NEGATIVE_MARGIN: f64 = 0.04;
/// Margin the held-out enrollment clip must clear above the resulting threshold.
pub const HELD_OUT_MARGIN: f64 = 0.02;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedThresholds {
    pub streaming_high: f64,
    pub streaming_low: f64,
    pub full_turn: f64,
}

/// Local media calibration may RAISE θ_high to max(globalHigh, maxLocalNegative + 0.04) but
/// never lower it.
pub fn resolve_thresholds(global: GlobalThresholds, max_local_negative: Option<f64>) -> ResolvedThresholds {
    let streaming_high = match max_local_negative {
        None => global.streaming_high,
        Some(neg) => global.streaming_high.max(neg + LOCAL_NEGATIVE_MARGIN),
    };
    ResolvedThresholds {
        streaming_high,
        streaming_low: streaming_high - global.hysteresis_gap,
        full_turn: global.full_turn,
    }
}

/// Media mode may only be enabled when the held-out clip clears both thresholds by ≥ 0.02.
pub fn held_out_passes(held_out_score: f64, t: &ResolvedThresholds) -> bool {
    held_out_score >= t.streaming_high.max(t.full_turn) + HELD_OUT_MARGIN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_without_calibration() {
        let t = resolve_thresholds(GLOBAL_THRESHOLDS_V1, None);
        assert_eq!(t, ResolvedThresholds { streaming_high: 0.62, streaming_low: 0.5, full_turn: 0.55 });
    }

    #[test]
    fn local_negative_only_raises() {
        let t = resolve_thresholds(GLOBAL_THRESHOLDS_V1, Some(0.3));
        assert_eq!(t.streaming_high, 0.62);
        let t = resolve_thresholds(GLOBAL_THRESHOLDS_V1, Some(0.7));
        assert!((t.streaming_high - 0.74).abs() < 1e-6);
        assert!((t.streaming_low - 0.62).abs() < 1e-6);
    }

    #[test]
    fn held_out_margin() {
        let t = resolve_thresholds(GLOBAL_THRESHOLDS_V1, None);
        let bar = t.streaming_high.max(t.full_turn);
        assert!(held_out_passes(bar + 0.021, &t));
        assert!(!held_out_passes(bar + 0.019, &t));
        assert!(!held_out_passes(bar - 0.1, &t));
    }

    #[test]
    fn streaming_stricter_than_full_turn_and_hysteresis_holds() {
        assert!(GLOBAL_THRESHOLDS_V1.streaming_high > GLOBAL_THRESHOLDS_V1.full_turn);
        let t = resolve_thresholds(GLOBAL_THRESHOLDS_V1, Some(0.9));
        assert!(t.streaming_low < t.streaming_high);
    }
}
