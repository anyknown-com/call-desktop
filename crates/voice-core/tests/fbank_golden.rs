//! Port of voice/test/kaldi-fbank.test.ts against the copied golden fixtures.
use std::collections::BTreeMap;
use std::path::PathBuf;
use voice_core::fbank::{compute_fbank, NUM_BINS};

#[derive(serde::Deserialize)]
struct Manifest {
    tolerances: Tolerances,
    clips: BTreeMap<String, Clip>,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tolerances {
    fbank_max_abs_err: f64,
    fbank_mean_abs_err: f64,
}
#[derive(serde::Deserialize)]
struct Clip {
    samples: usize,
    frames: usize,
    wav: String,
    fbank: String,
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/speaker/golden")
}

fn read_wav(p: &PathBuf) -> (Vec<f32>, u32) {
    let mut r = hound::WavReader::open(p).unwrap();
    let spec = r.spec();
    assert_eq!(spec.channels, 1);
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.unwrap() as f32 / scale).collect()
        }
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
    };
    (samples, spec.sample_rate)
}

fn read_f32(p: &PathBuf) -> Vec<f32> {
    std::fs::read(p).unwrap().chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

#[test]
fn matches_golden_fixtures() {
    let dir = golden_dir();
    let m: Manifest = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    for (name, clip) in &m.clips {
        let (pcm, sr) = read_wav(&dir.join(&clip.wav));
        assert_eq!(sr, 16000, "{name}");
        assert_eq!(pcm.len(), clip.samples, "{name}");
        let golden = read_f32(&dir.join(&clip.fbank));
        let (frames, t) = compute_fbank(&pcm);
        assert_eq!(t, clip.frames, "{name}");
        assert_eq!(golden.len(), t * NUM_BINS, "{name}");
        // Same tolerance split as the TS test: bins near the float-epsilon log floor
        // (golden <= -12) get 5e-3 because the reference's float32 KissFFT noise is
        // amplified by the log; everywhere else the full 1e-3 applies.
        let (mut max_err, mut max_err_floor, mut sum) = (0f64, 0f64, 0f64);
        for (a, g) in frames.iter().zip(&golden) {
            let e = (*a as f64 - *g as f64).abs();
            if *g <= -12.0 {
                max_err_floor = max_err_floor.max(e);
            } else {
                max_err = max_err.max(e);
            }
            sum += e;
        }
        assert!(max_err < m.tolerances.fbank_max_abs_err, "{name}: max err {max_err}");
        assert!(max_err_floor < 5e-3, "{name}: floor err {max_err_floor}");
        let mean = sum / golden.len() as f64;
        assert!(mean < m.tolerances.fbank_mean_abs_err, "{name}: mean err {mean}");
    }
}
