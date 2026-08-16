#![allow(dead_code)]
use std::path::{Path, PathBuf};

pub fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/speaker/golden")
}

/// 16 kHz mono WAV → f32 in [-1, 1] (int16 / 32768).
pub fn read_wav(p: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let spec = r.spec();
    assert_eq!(spec.channels, 1, "{}", p.display());
    assert_eq!(spec.sample_rate, 16_000, "{}", p.display());
    match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.unwrap() as f32 / scale).collect()
        }
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
    }
}
