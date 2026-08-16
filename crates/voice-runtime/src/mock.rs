//! Mock providers for the e2e harness (`voice call --mock`): no network, deterministic.
//! STT returns a canned transcript, the agent echoes it in two sentences, TTS produces silence.

use async_trait::async_trait;
use futures::{stream, StreamExt};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use voice_core::call_machine::{ChatMessage, InterjectionDecision};
use voice_core::turn_heuristics::TurnVerdict;
use voice_providers::{AgentClient, InterjectionHandler, PcmStream, Result, SttClient, TextStream, TtsClient, TurnDetector};

pub struct MockStt {
    n: AtomicUsize,
}
impl MockStt {
    pub fn new() -> Self {
        Self { n: AtomicUsize::new(0) }
    }
}
impl Default for MockStt {
    fn default() -> Self {
        Self::new()
    }
}
#[async_trait]
impl SttClient for MockStt {
    async fn transcribe(&self, audio: &[f32], sample_rate: u32) -> Result<String> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let n = self.n.fetch_add(1, Ordering::Relaxed) + 1;
        let secs = audio.len() as f64 / sample_rate as f64;
        Ok(format!("mock utterance {n} ({secs:.1} s)."))
    }
}

pub struct MockAgent;
#[async_trait]
impl AgentClient for MockAgent {
    async fn run(&self, messages: Vec<ChatMessage>) -> Result<TextStream> {
        let last = messages.last().map(|m| m.content.clone()).unwrap_or_default();
        let deltas = vec![
            "You said: ".to_string(),
            last,
            " That was your message. ".to_string(),
            "This is a second sentence so playback lasts a while. ".to_string(),
            "And a third one to make interruption easy to test.".to_string(),
        ];
        let s = stream::iter(deltas.into_iter().map(Ok)).then(|d| async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            d
        });
        Ok(Box::pin(s))
    }
}

pub struct MockTts;
#[async_trait]
impl TtsClient for MockTts {
    async fn synthesize(&self, text: &str) -> Result<PcmStream> {
        // Silence (never a tone — it is unpleasant): ~55 ms per character, 24 kHz, streamed in
        // 100 ms chunks after a 200 ms "network" delay, so timing/segment bookkeeping is exercised.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let secs = (text.chars().count() as f64 * 0.055).clamp(0.6, 6.0);
        let total = (secs * 24000.0) as usize;
        let chunks: Vec<Vec<f32>> = (0..total.div_ceil(2400)).map(|i| vec![0f32; 2400.min(total - i * 2400)]).collect();
        let s = stream::iter(chunks.into_iter().map(Ok)).then(|c| async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c
        });
        Ok(PcmStream { sample_rate: 24000, chunks: Box::pin(s) })
    }
}

pub struct MockFast;
#[async_trait]
impl TurnDetector for MockFast {
    async fn judge(&self, _history: &[ChatMessage], _utterance: &str) -> Result<TurnVerdict> {
        Ok(TurnVerdict::Complete)
    }
}
#[async_trait]
impl InterjectionHandler for MockFast {
    async fn decide(&self, _history: &[ChatMessage], _spoken: &str, _playing: &str, _interjection: &str) -> Result<InterjectionDecision> {
        Ok(InterjectionDecision::Stop)
    }
}
