//! Port of `fast-llm.ts`: small, fast LLM calls that shape turn-taking:
//! - has the user finished their thought, or just paused?
//! - what to do with a remark made while the assistant was speaking?

use async_trait::async_trait;
use voice_core::call_machine::{ChatMessage, InterjectionDecision, Role};
use voice_core::turn_heuristics::TurnVerdict;

use crate::llm::{Llm, Sampling};
use crate::{FastLlmConfig, InterjectionHandler, Result, TurnDetector};

const TURN_INSTRUCTIONS: &str =
    "You are the turn-taking judge in a spoken conversation. The user speaks slowly and pauses often \
     mid-thought; a pause is NOT the end of their turn. Given the conversation and everything the user \
     has said in the current turn (fragments joined), decide whether they have finished and are now \
     expecting the assistant to answer, or whether they are still mid-thought and would continue if given \
     a moment. Trailing conjunctions, unfinished clauses, list starts (\"first…\"), self-corrections and \
     \"let me think\" mean incomplete. A question, a request, or a rounded-off statement means complete. \
     Answer with exactly one word: complete or incomplete.";

const INTERJECTION_INSTRUCTIONS: &str =
    "You are the assistant in a live voice call. You were mid-answer when the user said something short. \
     Decide what to do and answer ONLY with JSON on one line:\n\
     {\"action\":\"ignore\"} — a mere backchannel or noise; keep talking.\n\
     {\"action\":\"react\",\"reaction\":\"<one short spoken sentence>\"} — a comment, joke, quip or aside worth a \
     quick natural reaction (agree, laugh, tease back, one-line answer) before you carry on with what you \
     were saying. Reaction max ~12 words, in the user's language, plain spoken text, no markdown.\n\
     {\"action\":\"stop\"} — they want you to stop, change topic, disagree substantively, or ask something \
     that needs a real answer; you will yield the floor.";

/// Non-streaming judge/decider on either provider. Implements both [`TurnDetector`]
/// and [`InterjectionHandler`].
pub struct FastLlm {
    llm: Llm,
}

impl FastLlm {
    pub fn new(cfg: FastLlmConfig) -> Self {
        Self {
            llm: Llm::new(cfg.provider, cfg.model, cfg.api_key, cfg.effort),
        }
    }

    /// Override the API origin (e.g. `http://127.0.0.1:1234`) — for tests and proxies.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.llm.base_url = base_url.into();
        self
    }
}

/// Last `n` messages as "User:/Assistant:" lines, with `\n[...]` annotations stripped.
fn recent(history: &[ChatMessage], n: usize) -> String {
    let start = history.len().saturating_sub(n);
    history[start..]
        .iter()
        .map(|m| {
            let who = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
            };
            format!("{who}: {}", strip_annotations(&m.content))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Port of `.replace(/\n\[.*?\]/g, "")`: remove every `\n[` … `]` (non-greedy, no newlines inside).
fn strip_annotations(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("\n[") {
        let after = &rest[i + 2..];
        match after.find([']', '\n']) {
            Some(j) if after.as_bytes()[j] == b']' => {
                out.push_str(&rest[..i]);
                rest = &after[j + 1..];
            }
            _ => {
                out.push_str(&rest[..i + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn conversation(history: &[ChatMessage]) -> String {
    let r = recent(history, 4);
    if r.is_empty() {
        "(start)".to_string()
    } else {
        r
    }
}

fn user(prompt: String) -> Vec<ChatMessage> {
    vec![ChatMessage {
        role: Role::User,
        content: prompt,
    }]
}

#[async_trait]
impl TurnDetector for FastLlm {
    async fn judge(&self, history: &[ChatMessage], utterance: &str) -> Result<TurnVerdict> {
        let prompt = format!(
            "Conversation so far:\n{}\n\nUser's current turn so far:\n\"\"\"{utterance}\"\"\"\n\nOne word:",
            conversation(history)
        );
        let text = self
            .llm
            .generate_text(
                TURN_INSTRUCTIONS,
                &user(prompt),
                Sampling {
                    max_tokens: Some(3),
                    temperature: Some(0.0),
                },
            )
            .await?;
        Ok(if text.to_lowercase().contains("incomplete") {
            TurnVerdict::Incomplete
        } else {
            TurnVerdict::Complete
        })
    }
}

#[async_trait]
impl InterjectionHandler for FastLlm {
    async fn decide(
        &self,
        history: &[ChatMessage],
        spoken_so_far: &str,
        playing: &str,
        interjection: &str,
    ) -> Result<InterjectionDecision> {
        let prompt = format!(
            "Conversation:\n{}\n\n\
             What you have said in this answer so far:\n\"\"\"{spoken_so_far}\"\"\"\n\n\
             You were in the middle of saying:\n\"\"\"{playing}\"\"\"\n\n\
             The user just said:\n\"\"\"{interjection}\"\"\"\n\nJSON:",
            conversation(history)
        );
        let text = self
            .llm
            .generate_text(
                INTERJECTION_INSTRUCTIONS,
                &user(prompt),
                Sampling {
                    max_tokens: Some(80),
                    temperature: Some(0.4),
                },
            )
            .await?;
        Ok(parse_decision(&text))
    }
}

/// Port of `parseDecision`: find the first `{ … }` span, parse it, default to `Ignore`.
pub fn parse_decision(text: &str) -> InterjectionDecision {
    let Some(start) = text.find('{') else {
        return InterjectionDecision::Ignore;
    };
    let Some(end) = text.rfind('}') else {
        return InterjectionDecision::Ignore;
    };
    if end < start {
        return InterjectionDecision::Ignore;
    }
    let Ok(j) = serde_json::from_str::<serde_json::Value>(&text[start..=end]) else {
        return InterjectionDecision::Ignore;
    };
    match j.get("action").and_then(|a| a.as_str()) {
        Some("stop") => InterjectionDecision::Stop,
        Some("react") => InterjectionDecision::React {
            reaction: j
                .get("reaction")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string(),
        },
        _ => InterjectionDecision::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_actions_and_tolerates_junk() {
        assert_eq!(
            parse_decision(r#"{"action":"stop"}"#),
            InterjectionDecision::Stop
        );
        assert_eq!(
            parse_decision(r#"Sure: {"action":"react","reaction":"哈哈對啊"}"#),
            InterjectionDecision::React {
                reaction: "哈哈對啊".into()
            }
        );
        assert_eq!(
            parse_decision(r#"{"action":"ignore"}"#),
            InterjectionDecision::Ignore
        );
        assert_eq!(parse_decision("nonsense"), InterjectionDecision::Ignore);
        assert_eq!(
            parse_decision(r#"{"action":"react"}"#),
            InterjectionDecision::React {
                reaction: "".into()
            }
        );
        assert_eq!(parse_decision("} {"), InterjectionDecision::Ignore);
        assert_eq!(
            parse_decision(r#"{"action":"react","reaction":5}"#),
            InterjectionDecision::React {
                reaction: "".into()
            }
        );
    }

    #[test]
    fn recent_formats_last_four_and_strips_annotations() {
        let m = |role: Role, s: &str| ChatMessage {
            role,
            content: s.into(),
        };
        let history = vec![
            m(Role::User, "zero"),
            m(Role::Assistant, "one"),
            m(Role::User, "two"),
            m(Role::Assistant, "I was saying that\n[interrupted by user]"),
            m(Role::User, "four [not stripped]\nkeep\n[a]\n[b] tail"),
        ];
        assert_eq!(
            recent(&history, 4),
            "Assistant: one\nUser: two\nAssistant: I was saying that\nUser: four [not stripped]\nkeep tail"
        );
        assert_eq!(recent(&[], 4), "");
        assert_eq!(conversation(&[]), "(start)");
        // Unterminated bracket is left alone, like the non-greedy regex.
        assert_eq!(strip_annotations("a\n[open"), "a\n[open");
        assert_eq!(strip_annotations("a\n[x\ny]"), "a\n[x\ny]");
    }
}
