//! HTTP providers for the voice desktop app: ElevenLabs STT/TTS, OpenAI chat + TTS,
//! Anthropic chat. Port of `voice/src/providers/*.ts`.
//!
//! # Cancellation and timeouts
//!
//! No call takes a cancellation token. To cancel, drop the future (or the returned
//! [`PcmStream`] / [`TextStream`]): `reqwest` aborts the underlying request when the
//! future/response is dropped. Timeouts are the caller's job (wrap in `tokio::time::timeout`).

mod agent;
mod error;
mod fast_llm;
mod http;
mod llm;
mod pcm;
mod sse;
mod tts;
mod wav;

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use voice_core::call_machine::{ChatMessage, InterjectionDecision};
use voice_core::turn_heuristics::TurnVerdict;

pub use agent::{Agent, SPOKEN_STYLE_HINT};
pub use error::{Error, Result};
pub use fast_llm::{parse_decision, FastLlm};
pub use pcm::{s16le_to_f32, single_chunk_pcm};
pub use sse::{SseDecoder, SseEvent};
pub use tts::{ElevenLabsStt, ElevenLabsTts, OpenAiTts};
pub use wav::encode_wav;

/// Mono f32 PCM in `[-1, 1]`, delivered as chunks as they arrive from the network.
pub struct PcmStream {
    pub sample_rate: u32,
    pub chunks: Pin<Box<dyn Stream<Item = Result<Vec<f32>>> + Send>>,
}

/// Text deltas from a streaming chat completion.
pub type TextStream = Pin<Box<dyn Stream<Item = Result<String>> + Send>>;

/// Batch speech-to-text.
#[async_trait]
pub trait SttClient: Send + Sync {
    async fn transcribe(&self, audio: &[f32], sample_rate: u32) -> Result<String>;
}

/// Text-to-speech returning a PCM stream.
#[async_trait]
pub trait TtsClient: Send + Sync {
    async fn synthesize(&self, text: &str) -> Result<PcmStream>;
}

/// The main conversational LLM.
#[async_trait]
pub trait AgentClient: Send + Sync {
    async fn run(&self, messages: Vec<ChatMessage>) -> Result<TextStream>;
}

/// Semantic end-of-turn judge.
#[async_trait]
pub trait TurnDetector: Send + Sync {
    async fn judge(&self, history: &[ChatMessage], utterance: &str) -> Result<TurnVerdict>;
}

/// Decides what to do with a short remark made while the assistant was speaking.
#[async_trait]
pub trait InterjectionHandler: Send + Sync {
    async fn decide(
        &self,
        history: &[ChatMessage],
        spoken_so_far: &str,
        playing: &str,
        interjection: &str,
    ) -> Result<InterjectionDecision>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    OpenAi,
    Anthropic,
}

/// Reasoning effort. `Default` = provider default (the TS `""`).
/// OpenAI additionally accepts `None`/`Minimal`; for Anthropic those map to `Low`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    #[default]
    #[serde(rename = "")]
    Unset,
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    /// Wire value; `""` for `Unset`.
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Unset => "",
            Effort::None => "none",
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElevenLabsSttConfig {
    pub api_key: String,
    /// e.g. "scribe_v2"
    pub model: String,
    /// ISO code to pin the language; empty = auto-detect.
    pub language_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElevenLabsTtsConfig {
    pub api_key: String,
    /// e.g. "eleven_v3", "eleven_flash_v2_5"
    pub model: String,
    pub voice_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiTtsConfig {
    pub api_key: String,
    /// e.g. "tts-1", "gpt-4o-mini-tts"
    pub model: String,
    pub voice: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    pub provider: LlmProvider,
    pub model: String,
    pub api_key: String,
    pub system_prompt: String,
    pub effort: Effort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastLlmConfig {
    pub provider: LlmProvider,
    pub model: String,
    pub api_key: String,
    pub effort: Effort,
}
