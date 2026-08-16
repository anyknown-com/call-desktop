//! Real-time audio for the voice desktop app: device I/O (cpal), resampling, WebRTC APM
//! (AEC/NS/AGC), the ordered playback sink with far-end tap, the PCM ring buffer and the
//! Media-mode turn controller. Everything model-related lives in `voice-ml`; everything
//! decision-related lives in `voice-core`.

pub mod apm;
pub mod engine;
pub mod media_turn;
pub mod resample;
pub mod ring;
pub mod sink;

/// Internal processing rate: everything from the devices is brought to 48 kHz mono for the APM,
/// then down to 16 kHz for VAD / STT / speaker models.
pub const APM_RATE: u32 = 48_000;
pub const MODEL_RATE: u32 = 16_000;
/// 10 ms at 48 kHz — the APM frame.
pub const APM_FRAME: usize = 480;
