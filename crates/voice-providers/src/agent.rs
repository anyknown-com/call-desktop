//! Port of `agent-ai-sdk.ts`: the main conversational LLM, streaming.

use async_trait::async_trait;
use voice_core::call_machine::ChatMessage;

use crate::llm::Llm;
use crate::{AgentClient, AgentConfig, Result, TextStream};

/// Appended so replies suit being read aloud sentence by sentence.
pub const SPOKEN_STYLE_HINT: &str =
    "You are talking to the user over a live voice call. Reply the way a person speaks: short, natural sentences, \
     no markdown, no lists, no headings, no code blocks. Answer in the language the user speaks.";

/// OpenAI Chat Completions or Anthropic Messages, streaming text deltas.
pub struct Agent {
    llm: Llm,
    instructions: String,
}

impl Agent {
    pub fn new(cfg: AgentConfig) -> Self {
        let instructions = [cfg.system_prompt.trim(), SPOKEN_STYLE_HINT]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n\n");
        Self {
            llm: Llm::new(cfg.provider, cfg.model, cfg.api_key, cfg.effort),
            instructions,
        }
    }

    /// Override the API origin (e.g. `http://127.0.0.1:1234`) — for tests and proxies.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.llm.base_url = base_url.into();
        self
    }

    /// The system prompt actually sent (system prompt + spoken-style hint).
    pub fn instructions(&self) -> &str {
        &self.instructions
    }
}

#[async_trait]
impl AgentClient for Agent {
    async fn run(&self, messages: Vec<ChatMessage>) -> Result<TextStream> {
        self.llm.stream_text(&self.instructions, &messages).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Effort, LlmProvider};

    fn cfg(system_prompt: &str) -> AgentConfig {
        AgentConfig {
            provider: LlmProvider::OpenAi,
            model: "m".into(),
            api_key: "k".into(),
            system_prompt: system_prompt.into(),
            effort: Effort::Unset,
        }
    }

    #[test]
    fn instructions_join_prompt_and_hint() {
        assert_eq!(
            Agent::new(cfg("  Be nice.  ")).instructions(),
            format!("Be nice.\n\n{SPOKEN_STYLE_HINT}")
        );
        assert_eq!(Agent::new(cfg("   ")).instructions(), SPOKEN_STYLE_HINT);
    }
}
