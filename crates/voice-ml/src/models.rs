//! Model file locations.
use std::path::PathBuf;

pub const SILERO_VAD_V5_FILE: &str = "silero_vad_v5.onnx";
pub const CAMPPLUS_FILE: &str = "campplus.onnx";

/// `$VOICE_MODELS_DIR` if set, else `<workspace>/models` (relative to this crate's manifest,
/// which is where `scripts/fetch-models.sh` puts the files). Callers may always pass explicit paths.
pub fn default_dir() -> PathBuf {
    match std::env::var_os("VOICE_MODELS_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models"),
    }
}

pub fn silero_vad_v5() -> PathBuf {
    default_dir().join(SILERO_VAD_V5_FILE)
}

pub fn campplus() -> PathBuf {
    default_dir().join(CAMPPLUS_FILE)
}
