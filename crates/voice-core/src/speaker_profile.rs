//! Enrollment quality checks and speaker-profile construction (FINAL v1).
//! Port of voice/src/core/speaker/speaker-profile.ts.

use crate::cosine::{cosine, l2_normalize};
use crate::thresholds::{
    held_out_passes, resolve_thresholds, GlobalThresholds, ResolvedThresholds, GLOBAL_THRESHOLDS_V1, THRESHOLD_POLICY_VERSION,
};
use serde::{Deserialize, Serialize};

pub const PROFILE_SCHEMA_VERSION: u32 = 1;
pub const ENROLLMENT_CLIPS: usize = 6;
/// Minimum voiced audio per enrollment clip (ms).
pub const MIN_VOICED_MS: f64 = 2000.0;
/// Reject clips with more than 0.1% clipped samples.
pub const MAX_CLIPPED_FRACTION: f64 = 0.001;
/// Reject clips whose RMS is below this (weak level).
pub const MIN_RMS: f64 = 0.008;
/// Reject a clip whose embedding cosine vs the mean of the others is below this.
pub const MIN_CONSISTENCY: f64 = 0.5;
/// Near-duplicate detection.
pub const DUPLICATE_COSINE: f64 = 0.995;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerProfile {
    pub schema_version: u32,
    pub model_sha256: String,
    pub frontend_version: u32,
    pub created_at: f64,
    pub centroid: Vec<f32>,
    pub enrollment: Vec<Vec<f32>>,
    pub held_out: Vec<f32>,
    pub held_out_score: f64,
    pub max_local_negative: Option<f64>,
    pub thresholds: ProfileThresholds,
    pub threshold_policy_version: u32,
}

/// Serde-friendly mirror of [`ResolvedThresholds`] (same field names as the web profile JSON).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileThresholds {
    pub streaming_high: f64,
    pub streaming_low: f64,
    pub full_turn: f64,
}
impl From<ResolvedThresholds> for ProfileThresholds {
    fn from(t: ResolvedThresholds) -> Self {
        Self { streaming_high: t.streaming_high, streaming_low: t.streaming_low, full_turn: t.full_turn }
    }
}
impl From<ProfileThresholds> for ResolvedThresholds {
    fn from(t: ProfileThresholds) -> Self {
        Self { streaming_high: t.streaming_high, streaming_low: t.streaming_low, full_turn: t.full_turn }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipRejection {
    InsufficientVoiced,
    Clipping,
    LowLevel,
    InconsistentEmbedding,
    Duplicate,
}

/// Signal-level checks that need no embedding.
pub fn check_clip_signal(pcm: &[f32], voiced_ms: f64) -> Option<ClipRejection> {
    if voiced_ms < MIN_VOICED_MS {
        return Some(ClipRejection::InsufficientVoiced);
    }
    let mut clipped = 0usize;
    let mut sum_sq = 0f64;
    for &v in pcm {
        if v.abs() >= 0.999 {
            clipped += 1;
        }
        sum_sq += (v as f64) * (v as f64);
    }
    if !pcm.is_empty() && clipped as f64 / pcm.len() as f64 > MAX_CLIPPED_FRACTION {
        return Some(ClipRejection::Clipping);
    }
    if (sum_sq / pcm.len().max(1) as f64).sqrt() < MIN_RMS {
        return Some(ClipRejection::LowLevel);
    }
    None
}

/// Embedding-level checks for a candidate clip against previously accepted ones.
pub fn check_clip_embedding(candidate: &[f32], accepted: &[Vec<f32>]) -> Option<ClipRejection> {
    if accepted.is_empty() {
        return None;
    }
    if accepted.iter().any(|e| cosine(candidate, e) > DUPLICATE_COSINE) {
        return Some(ClipRejection::Duplicate);
    }
    if accepted.len() >= 2 && cosine(candidate, &build_centroid(accepted)) < MIN_CONSISTENCY {
        return Some(ClipRejection::InconsistentEmbedding);
    }
    None
}

/// L2-normalize each embedding, average, renormalize.
pub fn build_centroid(embeddings: &[Vec<f32>]) -> Vec<f32> {
    assert!(!embeddings.is_empty(), "no embeddings");
    let dim = embeddings[0].len();
    let mut sum = vec![0f64; dim];
    for e in embeddings {
        for (s, v) in sum.iter_mut().zip(l2_normalize(e)) {
            *s += v as f64;
        }
    }
    let mean: Vec<f32> = sum.iter().map(|s| (s / embeddings.len() as f64) as f32).collect();
    l2_normalize(&mean)
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProfileBuildError {
    WrongClipCount,
    HeldOutBelowThreshold { held_out_score: f64 },
}

pub struct ProfileBuildInput<'a> {
    pub enrollment: &'a [Vec<f32>],
    pub held_out: &'a [f32],
    pub max_local_negative: Option<f64>,
    pub model_sha256: &'a str,
    pub frontend_version: u32,
    pub now: f64,
    pub global: Option<GlobalThresholds>,
}

pub fn build_profile(input: ProfileBuildInput<'_>) -> Result<SpeakerProfile, ProfileBuildError> {
    if input.enrollment.len() != ENROLLMENT_CLIPS {
        return Err(ProfileBuildError::WrongClipCount);
    }
    let centroid = build_centroid(input.enrollment);
    let thresholds = resolve_thresholds(input.global.unwrap_or(GLOBAL_THRESHOLDS_V1), input.max_local_negative);
    let held_out_norm = l2_normalize(input.held_out);
    let held_out_score = cosine(&held_out_norm, &centroid);
    if !held_out_passes(held_out_score, &thresholds) {
        return Err(ProfileBuildError::HeldOutBelowThreshold { held_out_score });
    }
    Ok(SpeakerProfile {
        schema_version: PROFILE_SCHEMA_VERSION,
        model_sha256: input.model_sha256.to_string(),
        frontend_version: input.frontend_version,
        created_at: input.now,
        centroid,
        enrollment: input.enrollment.iter().map(|e| l2_normalize(e)).collect(),
        held_out: held_out_norm,
        held_out_score,
        max_local_negative: input.max_local_negative,
        thresholds: thresholds.into(),
        threshold_policy_version: THRESHOLD_POLICY_VERSION,
    })
}

/// Cosine of an (unnormalized) embedding against the profile centroid.
pub fn score_embedding(profile: &SpeakerProfile, embedding: &[f32]) -> f64 {
    cosine(embedding, &profile.centroid)
}

/// A profile is only usable with the exact model + frontend + schema it was built with.
pub fn profile_compatible(profile: &SpeakerProfile, model_sha256: &str, frontend_version: u32) -> bool {
    profile.schema_version == PROFILE_SCHEMA_VERSION
        && profile.model_sha256 == model_sha256
        && profile.frontend_version == frontend_version
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(dir: usize, dim: usize) -> Vec<f32> {
        let mut v = vec![0f32; dim];
        v[dir] = 1.0;
        v
    }
    fn near(base: &[f32], eps: f32, k: usize) -> Vec<f32> {
        let mut v = base.to_vec();
        v[k] += eps;
        v
    }

    #[test]
    fn signal_checks() {
        let ok: Vec<f32> = (0..16000).map(|i| 0.1 * ((i as f32) * 0.05).sin()).collect();
        assert_eq!(check_clip_signal(&ok, 2500.0), None);
        assert_eq!(check_clip_signal(&ok, 1000.0), Some(ClipRejection::InsufficientVoiced));
        let quiet: Vec<f32> = ok.iter().map(|v| v * 0.01).collect();
        assert_eq!(check_clip_signal(&quiet, 2500.0), Some(ClipRejection::LowLevel));
        let clipped: Vec<f32> = ok.iter().map(|v| if v.abs() > 0.05 { v.signum() } else { *v }).collect();
        assert_eq!(check_clip_signal(&clipped, 2500.0), Some(ClipRejection::Clipping));
    }

    #[test]
    fn embedding_checks() {
        let a = unit(0, 4);
        assert_eq!(check_clip_embedding(&a, &[]), None);
        assert_eq!(check_clip_embedding(&near(&a, 0.001, 1), std::slice::from_ref(&a)), Some(ClipRejection::Duplicate));
        let b = near(&a, 0.3, 1);
        let acc = vec![a.clone(), b];
        assert_eq!(check_clip_embedding(&unit(3, 4), &acc), Some(ClipRejection::InconsistentEmbedding));
        assert_eq!(check_clip_embedding(&near(&a, 0.2, 2), &acc), None);
    }

    #[test]
    fn centroid_is_normalized() {
        let c = build_centroid(&[unit(0, 3), unit(1, 3)]);
        let n: f32 = c.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-6);
        assert!((c[0] - c[1]).abs() < 1e-6);
    }

    #[test]
    fn build_profile_paths() {
        let base = unit(0, 8);
        let enrollment: Vec<Vec<f32>> = (1..=6).map(|k| near(&base, 0.05, k)).collect();
        let bad = build_profile(ProfileBuildInput { enrollment: &enrollment[..5], held_out: &base, max_local_negative: None, model_sha256: "m", frontend_version: 1, now: 0.0, global: None });
        assert_eq!(bad, Err(ProfileBuildError::WrongClipCount));
        let ok = build_profile(ProfileBuildInput { enrollment: &enrollment, held_out: &near(&base, 0.05, 7), max_local_negative: None, model_sha256: "m", frontend_version: 1, now: 5.0, global: None }).unwrap();
        assert!(ok.held_out_score > 0.9);
        assert!(profile_compatible(&ok, "m", 1));
        assert!(!profile_compatible(&ok, "other", 1));
        let far = build_profile(ProfileBuildInput { enrollment: &enrollment, held_out: &unit(7, 8), max_local_negative: None, model_sha256: "m", frontend_version: 1, now: 0.0, global: None });
        assert!(matches!(far, Err(ProfileBuildError::HeldOutBelowThreshold { .. })));
        // Local negative raises the bar: a held-out at 0.70 fails when media scored 0.68.
        let mid = build_profile(ProfileBuildInput { enrollment: &enrollment, held_out: &near(&base, 0.05, 7), max_local_negative: Some(0.99), model_sha256: "m", frontend_version: 1, now: 0.0, global: None });
        assert!(matches!(mid, Err(ProfileBuildError::HeldOutBelowThreshold { .. })));
        // Round-trips through JSON with the web field names.
        let json = serde_json::to_string(&ok).unwrap();
        assert!(json.contains("\"heldOutScore\""));
        let back: SpeakerProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.centroid, ok.centroid);
        assert!((back.held_out_score - ok.held_out_score).abs() < 1e-12);
    }
}
