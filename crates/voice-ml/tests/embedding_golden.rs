//! CAM++ golden test: our fbank + ort embedding vs the Python/sherpa-onnx reference fixtures.
mod common;

use common::{golden_dir, read_wav};
use std::collections::BTreeMap;
use std::time::Instant;
use voice_core::cosine::cosine;
use voice_ml::{models, SpeakerEmbedder};

#[derive(serde::Deserialize)]
struct Manifest {
    tolerances: Tolerances,
    clips: BTreeMap<String, Clip>,
    #[serde(rename = "pairwiseCosine")]
    pairwise_cosine: BTreeMap<String, f64>,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tolerances {
    embedding_cosine_min: f64,
    pairwise_cosine_abs_err: f64,
}
#[derive(serde::Deserialize)]
struct Clip {
    samples: usize,
    wav: String,
    embedding: String,
}

fn manifest() -> Manifest {
    serde_json::from_slice(&std::fs::read(golden_dir().join("manifest.json")).unwrap()).unwrap()
}

#[test]
fn matches_golden_embeddings_and_pairwise_scores() {
    let dir = golden_dir();
    let m = manifest();
    let mut emb = SpeakerEmbedder::new(models::campplus()).unwrap();
    let mut ours: BTreeMap<String, Vec<f32>> = BTreeMap::new();
    let mut min_cos = 1.0f64;
    for (name, clip) in &m.clips {
        let pcm = read_wav(&dir.join(&clip.wav));
        assert_eq!(pcm.len(), clip.samples, "{name}");
        let golden: Vec<f32> = serde_json::from_slice(&std::fs::read(dir.join(&clip.embedding)).unwrap()).unwrap();
        assert_eq!(golden.len(), 192, "{name}");
        let e = emb.embed(&pcm).unwrap();
        assert_eq!(e.len(), 192);
        let c = cosine(&e, &golden);
        assert!(c >= m.tolerances.embedding_cosine_min, "{name}: cosine {c} < {}", m.tolerances.embedding_cosine_min);
        min_cos = min_cos.min(c);
        ours.insert(name.clone(), e);
    }
    let mut max_err = 0.0f64;
    for (pair, golden) in &m.pairwise_cosine {
        let (a, b) = pair.split_once('|').unwrap();
        let c = cosine(&ours[a], &ours[b]);
        let err = (c - golden).abs();
        assert!(err < m.tolerances.pairwise_cosine_abs_err, "{pair}: ours {c} vs golden {golden}");
        max_err = max_err.max(err);
    }
    println!("min embedding cosine {min_cos:.8}, max pairwise abs err {max_err:.2e}");
}

#[test]
fn rejects_wrong_sha256() {
    let Err(err) = SpeakerEmbedder::new(models::silero_vad_v5()) else { panic!("loaded a non-pinned file") };
    assert!(err.to_string().contains("sha256"), "{err}");
}

/// `cargo test -p voice-ml --release --test embedding_golden -- --ignored --nocapture`
#[test]
#[ignore]
fn embed_timing_1p6s_window() {
    let pcm = read_wav(&golden_dir().join("short_1p6s.wav"));
    assert_eq!(pcm.len(), 25_600);
    let mut emb = SpeakerEmbedder::new(models::campplus()).unwrap();
    for _ in 0..5 {
        emb.embed(&pcm).unwrap();
    }
    let mut ms: Vec<f64> = (0..50)
        .map(|_| {
            let t = Instant::now();
            emb.embed(&pcm).unwrap();
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    ms.sort_by(|a, b| a.total_cmp(b));
    let (p50, p95) = (ms[ms.len() / 2], ms[ms.len() * 95 / 100]);
    println!("embed(1.6 s): p50 {p50:.1} ms, p95 {p95:.1} ms (n={})", ms.len());
    assert!(p95 < 100.0, "p95 {p95:.1} ms >= 100 ms");
}
