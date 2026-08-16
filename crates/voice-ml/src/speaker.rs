//! CAM++ speaker embeddings (3D-Speaker `speech_campplus_sv_zh_en_16k-common_advanced`,
//! sherpa-onnx export). Input `x` float32 `[1, T, 80]` mean-subtracted Kaldi fbank
//! (`voice_core::fbank::compute_features`), output `embedding` `[1, 192]`.

use anyhow::{anyhow, Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use sha2::{Digest, Sha256};
use std::path::Path;
use voice_core::cosine::l2_normalize;
use voice_core::fbank::{compute_features, NUM_BINS};

/// Pinned model hash; see models/campplus/MODEL_CARD.md and scripts/fetch-models.sh.
pub const CAMPPLUS_SHA256: &str = "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2";
pub const EMBEDDING_DIM: usize = 192;

pub struct SpeakerEmbedder {
    session: Session,
}

impl SpeakerEmbedder {
    /// Loads the model, refusing any file whose sha256 differs from [`CAMPPLUS_SHA256`].
    pub fn new(model_path: impl AsRef<Path>) -> Result<Self> {
        let path = model_path.as_ref();
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let got = hex(&Sha256::digest(&bytes));
        if got != CAMPPLUS_SHA256 {
            return Err(anyhow!("{}: sha256 {got} != pinned {CAMPPLUS_SHA256}", path.display()));
        }
        let session = Session::builder()?
            .commit_from_memory(&bytes)
            .with_context(|| format!("loading CAM++ from {}", path.display()))?;
        Ok(Self { session })
    }

    /// 16 kHz mono PCM in `[-1, 1]` → L2-normalized 192-d embedding.
    pub fn embed(&mut self, pcm16k: &[f32]) -> Result<Vec<f32>> {
        let (feats, t) = compute_features(pcm16k);
        if t == 0 {
            return Err(anyhow!("audio too short for a single fbank frame"));
        }
        let x = Tensor::from_array(([1usize, t, NUM_BINS], feats))?;
        let out = self.session.run(ort::inputs!["x" => x])?;
        let (_, e) = out["embedding"].try_extract_tensor::<f32>()?;
        if e.len() != EMBEDDING_DIM {
            return Err(anyhow!("unexpected embedding size {}", e.len()));
        }
        Ok(l2_normalize(e))
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
