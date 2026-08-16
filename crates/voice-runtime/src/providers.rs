//! The provider set a call runs against: real HTTP clients built from settings + keys, or the
//! offline mocks for the e2e harness.

use crate::keys::Keys;
use crate::mock;
use crate::settings::{Settings, TtsProvider};
use anyhow::{anyhow, Result};
use std::sync::Arc;
use voice_providers::{
    Agent, AgentClient, AgentConfig, ElevenLabsStt, ElevenLabsSttConfig, ElevenLabsTts, ElevenLabsTtsConfig, FastLlm, FastLlmConfig,
    InterjectionHandler, OpenAiTts, OpenAiTtsConfig, SttClient, TtsClient, TurnDetector,
};

pub struct Providers {
    pub stt: Arc<dyn SttClient>,
    pub tts: Arc<dyn TtsClient>,
    pub agent: Arc<dyn AgentClient>,
    pub judge: Option<Arc<dyn TurnDetector>>,
    pub interjection: Option<Arc<dyn InterjectionHandler>>,
}

pub fn build_providers(s: &Settings, k: &Keys) -> Result<Providers> {
    let missing = k.missing(s);
    if !missing.is_empty() {
        return Err(anyhow!("missing API keys: {}", missing.join(", ")));
    }
    let llm_key = match s.llm.provider {
        voice_providers::LlmProvider::OpenAi => k.openai.clone(),
        voice_providers::LlmProvider::Anthropic => k.anthropic.clone(),
    };
    let stt: Arc<dyn SttClient> = Arc::new(ElevenLabsStt::new(ElevenLabsSttConfig {
        api_key: k.elevenlabs.clone(),
        model: s.stt.model.clone(),
        language_code: s.stt.language_code.clone(),
    }));
    let tts: Arc<dyn TtsClient> = match s.tts.provider {
        TtsProvider::ElevenLabs => Arc::new(ElevenLabsTts::new(ElevenLabsTtsConfig {
            api_key: k.elevenlabs.clone(),
            model: s.tts.elevenlabs_model.clone(),
            voice_id: s.tts.elevenlabs_voice_id.clone(),
        })),
        TtsProvider::OpenAi => Arc::new(OpenAiTts::new(OpenAiTtsConfig { api_key: k.openai.clone(), model: s.tts.openai_model.clone(), voice: s.tts.openai_voice.clone() })),
    };
    let agent: Arc<dyn AgentClient> = Arc::new(Agent::new(AgentConfig {
        provider: s.llm.provider,
        model: s.llm.model.clone(),
        api_key: llm_key.clone(),
        system_prompt: s.system_prompt.clone(),
        effort: s.llm.effort,
    }));
    let fast = Arc::new(FastLlm::new(FastLlmConfig { provider: s.llm.provider, model: s.llm.fast_model.clone(), api_key: llm_key, effort: s.llm.fast_effort }));
    Ok(Providers {
        stt,
        tts,
        agent,
        judge: s.turn.semantic.then(|| fast.clone() as Arc<dyn TurnDetector>),
        interjection: s.turn.interjections.then_some(fast as Arc<dyn InterjectionHandler>),
    })
}

pub fn mock_providers(s: &Settings) -> Providers {
    let fast = Arc::new(mock::MockFast);
    Providers {
        stt: Arc::new(mock::MockStt::new()),
        tts: Arc::new(mock::MockTts),
        agent: Arc::new(mock::MockAgent),
        judge: s.turn.semantic.then(|| fast.clone() as Arc<dyn TurnDetector>),
        interjection: s.turn.interjections.then_some(fast as Arc<dyn InterjectionHandler>),
    }
}
