//! Silero VAD v5 + the frame-processor state machine from `@ricky0123/vad-web` 0.0.30.
//!
//! Model I/O replicates `vad-web/dist/models/v5.js` exactly: `input` float32 `[1, 512]` (the raw
//! 512-sample 16 kHz window, no context prepended — the v5 ONNX keeps its own 64-sample context
//! inside `state`), `state` float32 `[2, 1, 128]`, `sr` int64 scalar 16000; outputs `output`
//! (speech probability, `[1, 1]`) and `stateN` `[2, 1, 128]`.
//! Detector semantics replicate `vad-web/dist/frame-processor.js` (`FrameProcessor.process`).

use anyhow::{anyhow, Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;

pub const SAMPLE_RATE: u32 = 16_000;
pub const FRAME_SAMPLES: usize = 512;
/// Milliseconds per 512-sample frame at 16 kHz (`frameSamples / 16` in vad-web).
pub const FRAME_MS: f32 = FRAME_SAMPLES as f32 / 16.0;
const STATE_LEN: usize = 2 * 128;

/// Stateful Silero VAD v5 model. Feed consecutive 512-sample 16 kHz frames.
pub struct SileroVad {
    session: Session,
    state: Vec<f32>,
}

impl SileroVad {
    pub fn new(model_path: impl AsRef<Path>) -> Result<Self> {
        let session = Session::builder()?
            .commit_from_file(model_path.as_ref())
            .with_context(|| format!("loading Silero VAD from {}", model_path.as_ref().display()))?;
        Ok(Self { session, state: vec![0.0; STATE_LEN] })
    }

    /// Speech probability in `[0, 1]` for one 512-sample frame.
    pub fn process(&mut self, frame512: &[f32]) -> Result<f32> {
        if frame512.len() != FRAME_SAMPLES {
            return Err(anyhow!("Silero VAD expects {FRAME_SAMPLES} samples, got {}", frame512.len()));
        }
        let input = Tensor::from_array(([1usize, FRAME_SAMPLES], frame512.to_vec()))?;
        let state = Tensor::from_array(([2usize, 1, 128], std::mem::take(&mut self.state)))?;
        let sr = Tensor::from_array(((), vec![SAMPLE_RATE as i64]))?;
        let mut out = self.session.run(ort::inputs!["input" => input, "state" => state, "sr" => sr])?;
        let (_, next) = out["stateN"].try_extract_tensor::<f32>()?;
        self.state = next.to_vec();
        let prob = out.remove("output").ok_or_else(|| anyhow!("no `output` from Silero VAD"))?;
        let (_, p) = prob.try_extract_tensor::<f32>()?;
        Ok(p[0])
    }

    pub fn reset(&mut self) {
        self.state.fill(0.0);
    }
}

/// vad-web `FrameProcessorOptions`. Defaults are the voice app's (`src/audio/vad-source.ts`),
/// not vad-web's own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadConfig {
    pub positive_speech_threshold: f32,
    pub negative_speech_threshold: f32,
    pub redemption_ms: u32,
    pub pre_speech_pad_ms: u32,
    pub min_speech_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            positive_speech_threshold: 0.5,
            negative_speech_threshold: 0.35,
            redemption_ms: 700,
            pre_speech_pad_ms: 300,
            min_speech_ms: 250,
        }
    }
}

impl VadConfig {
    /// `Math.floor(ms / msPerFrame)`, as in vad-web `calculateFrameParams`.
    fn frames(ms: u32) -> usize {
        (ms as f32 / FRAME_MS) as usize
    }
    pub fn redemption_frames(&self) -> usize {
        Self::frames(self.redemption_ms)
    }
    pub fn pre_speech_pad_frames(&self) -> usize {
        Self::frames(self.pre_speech_pad_ms)
    }
    pub fn min_speech_frames(&self) -> usize {
        Self::frames(self.min_speech_ms)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum VadEvent {
    /// First frame with `prob >= positive_speech_threshold` (vad-web `onSpeechStart`).
    Start,
    /// The `min_speech_frames`-th speech frame of this segment (vad-web `onSpeechRealStart`).
    RealStart,
    /// Segment ended with fewer than `min_speech_frames` speech frames (vad-web `onVADMisfire`).
    Misfire,
    /// Segment ended; `audio` is 16 kHz PCM including the pre-speech pad and the redemption tail.
    End { audio: Vec<f32> },
}

/// Port of vad-web's `FrameProcessor` on top of [`SileroVad`].
pub struct VadDetector {
    model: SileroVad,
    config: VadConfig,
    /// (frame, is_speech) — vad-web `audioBuffer`.
    buffer: Vec<(Vec<f32>, bool)>,
    speaking: bool,
    redemption_counter: usize,
    speech_frame_count: usize,
    real_start_fired: bool,
}

impl VadDetector {
    pub fn new(model: SileroVad, config: VadConfig) -> Self {
        Self {
            model,
            config,
            buffer: Vec::new(),
            speaking: false,
            redemption_counter: 0,
            speech_frame_count: 0,
            real_start_fired: false,
        }
    }

    pub fn config(&self) -> &VadConfig {
        &self.config
    }

    /// vad-web `FrameProcessor.reset`: drops buffered audio and model state.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.speaking = false;
        self.redemption_counter = 0;
        self.speech_frame_count = 0;
        self.real_start_fired = false;
        self.model.reset();
    }

    /// Process one 512-sample frame. Returns the speech probability and any events, in order.
    pub fn process(&mut self, frame512: &[f32]) -> Result<(f32, Vec<VadEvent>)> {
        let prob = self.model.process(frame512)?;
        let mut events = Vec::new();
        let is_speech = prob >= self.config.positive_speech_threshold;
        self.buffer.push((frame512.to_vec(), is_speech));

        if is_speech {
            self.speech_frame_count += 1;
            self.redemption_counter = 0;
        }
        if is_speech && !self.speaking {
            self.speaking = true;
            events.push(VadEvent::Start);
        }
        if self.speaking && self.speech_frame_count == self.config.min_speech_frames() && !self.real_start_fired {
            self.real_start_fired = true;
            events.push(VadEvent::RealStart);
        }
        if prob < self.config.negative_speech_threshold && self.speaking {
            self.redemption_counter += 1;
            if self.redemption_counter >= self.config.redemption_frames() {
                self.redemption_counter = 0;
                self.speech_frame_count = 0;
                self.speaking = false;
                self.real_start_fired = false;
                let buffer = std::mem::take(&mut self.buffer);
                let speech_frames = buffer.iter().filter(|(_, s)| *s).count();
                if speech_frames >= self.config.min_speech_frames() {
                    let audio = buffer.into_iter().flat_map(|(f, _)| f).collect();
                    events.push(VadEvent::End { audio });
                } else {
                    events.push(VadEvent::Misfire);
                }
            }
        }
        if !self.speaking {
            let pad = self.config.pre_speech_pad_frames();
            if self.buffer.len() > pad {
                self.buffer.drain(..self.buffer.len() - pad);
            }
            self.speech_frame_count = 0;
        }
        Ok((prob, events))
    }
}
