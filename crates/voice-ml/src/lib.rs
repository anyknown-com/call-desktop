//! On-device ML inference for the voice desktop app, all on CPU via ONNX Runtime (`ort`):
//! Silero VAD v5 (same model + frame-processor semantics as `@ricky0123/vad-web`) and the pinned
//! CAM++ speaker-embedding model used by the media-mode speaker gate.

pub mod models;
pub mod speaker;
pub mod vad;

pub use speaker::{SpeakerEmbedder, CAMPPLUS_SHA256};
pub use vad::{SileroVad, VadConfig, VadDetector, VadEvent};
