//! Shared chat-completion plumbing for OpenAI (Chat Completions) and Anthropic (Messages):
//! request bodies, SSE parsing, streaming and non-streaming calls.

use std::collections::VecDeque;
use std::pin::Pin;

use bytes::Bytes;
use futures::{stream, Stream, StreamExt};
use serde_json::{json, Value};
use voice_core::call_machine::{ChatMessage, Role};

use crate::http::check_status;
use crate::sse::{SseDecoder, SseEvent};
use crate::{Effort, Error, LlmProvider, Result, TextStream};

pub(crate) const OPENAI_BASE: &str = "https://api.openai.com";
pub(crate) const ANTHROPIC_BASE: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Anthropic requires `max_tokens`; the TS version (AI SDK) defaults to 4096.
const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 4096;

/// Per-call sampling knobs. `None` = provider default.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Sampling {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

/// One configured LLM endpoint (provider + model + key + effort).
#[derive(Debug, Clone)]
pub(crate) struct Llm {
    pub http: reqwest::Client,
    pub provider: LlmProvider,
    pub model: String,
    pub api_key: String,
    pub effort: Effort,
    pub base_url: String,
}

impl Llm {
    pub fn new(provider: LlmProvider, model: String, api_key: String, effort: Effort) -> Self {
        let base_url = match provider {
            LlmProvider::OpenAi => OPENAI_BASE,
            LlmProvider::Anthropic => ANTHROPIC_BASE,
        }
        .to_string();
        Self {
            http: reqwest::Client::new(),
            provider,
            model,
            api_key,
            effort,
            base_url,
        }
    }

    pub fn name(&self) -> &'static str {
        provider_name(self.provider)
    }

    fn body(
        &self,
        system: &str,
        messages: &[ChatMessage],
        sampling: Sampling,
        stream: bool,
    ) -> Value {
        match self.provider {
            LlmProvider::OpenAi => {
                openai_body(&self.model, self.effort, system, messages, sampling, stream)
            }
            LlmProvider::Anthropic => {
                anthropic_body(&self.model, self.effort, system, messages, sampling, stream)
            }
        }
    }

    async fn send(&self, body: Value) -> Result<reqwest::Response> {
        let req = match self.provider {
            LlmProvider::OpenAi => self
                .http
                .post(format!("{}/v1/chat/completions", self.base_url))
                .bearer_auth(&self.api_key),
            LlmProvider::Anthropic => self
                .http
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION),
        };
        let res = req
            .json(&body)
            .send()
            .await
            .map_err(Error::transport(self.name()))?;
        check_status(self.name(), res).await
    }

    /// Streaming completion: yields text deltas.
    pub async fn stream_text(&self, system: &str, messages: &[ChatMessage]) -> Result<TextStream> {
        let res = self
            .send(self.body(system, messages, Sampling::default(), true))
            .await?;
        Ok(sse_text_stream(self.provider, res.bytes_stream()))
    }

    /// Non-streaming completion: returns the full text.
    pub async fn generate_text(
        &self,
        system: &str,
        messages: &[ChatMessage],
        sampling: Sampling,
    ) -> Result<String> {
        let res = self
            .send(self.body(system, messages, sampling, false))
            .await?;
        let json: Value = res.json().await.map_err(Error::transport(self.name()))?;
        Ok(extract_text(self.provider, &json))
    }
}

pub(crate) fn provider_name(p: LlmProvider) -> &'static str {
    match p {
        LlmProvider::OpenAi => "openai",
        LlmProvider::Anthropic => "anthropic",
    }
}

fn role_str(r: &Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn message_values(messages: &[ChatMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| json!({ "role": role_str(&m.role), "content": m.content }))
        .collect()
}

/// OpenAI reasoning models reject `temperature` (mirrors the AI SDK, which strips it there).
fn openai_is_reasoning_model(model: &str) -> bool {
    model.starts_with('o') || (model.starts_with("gpt-5") && !model.starts_with("gpt-5-chat"))
}

pub(crate) fn openai_body(
    model: &str,
    effort: Effort,
    system: &str,
    messages: &[ChatMessage],
    sampling: Sampling,
    stream: bool,
) -> Value {
    let mut msgs = Vec::with_capacity(messages.len() + 1);
    if !system.is_empty() {
        msgs.push(json!({ "role": "system", "content": system }));
    }
    msgs.extend(message_values(messages));
    let mut body = json!({ "model": model, "messages": msgs, "stream": stream });
    if effort != Effort::Unset {
        body["reasoning_effort"] = json!(effort.as_str());
    }
    if let Some(n) = sampling.max_tokens {
        body["max_completion_tokens"] = json!(n);
    }
    if let Some(t) = sampling.temperature {
        if !openai_is_reasoning_model(model) {
            body["temperature"] = json!(t);
        }
    }
    body
}

/// Anthropic's `output_config.effort` value for our `Effort`, or `None` to omit it.
/// `none`/`minimal` are OpenAI-only and map to `low`.
pub(crate) fn anthropic_effort(effort: Effort) -> Option<&'static str> {
    match effort {
        Effort::Unset => None,
        Effort::None | Effort::Minimal | Effort::Low => Some("low"),
        e => Some(e.as_str()),
    }
}

pub(crate) fn anthropic_body(
    model: &str,
    effort: Effort,
    system: &str,
    messages: &[ChatMessage],
    sampling: Sampling,
    stream: bool,
) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": sampling.max_tokens.unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS),
        "messages": message_values(messages),
        "stream": stream,
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if let Some(e) = anthropic_effort(effort) {
        body["output_config"] = json!({ "effort": e });
    }
    // `temperature` is deliberately not sent: Claude 4.7+ rejects sampling parameters.
    body
}

/// What one SSE event means for the text stream.
#[derive(Debug, PartialEq)]
pub(crate) enum StreamItem {
    Text(String),
    Done,
    Skip,
}

pub(crate) fn parse_event(provider: LlmProvider, ev: &SseEvent) -> Result<StreamItem> {
    let name = provider_name(provider);
    if provider == LlmProvider::OpenAi && ev.data.trim() == "[DONE]" {
        return Ok(StreamItem::Done);
    }
    let v: Value = serde_json::from_str(&ev.data)
        .map_err(|e| Error::protocol(name, format!("bad SSE JSON: {e}: {}", ev.data)))?;
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(Error::protocol(name, format!("stream error: {msg}")));
    }
    match provider {
        LlmProvider::OpenAi => Ok(v
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| StreamItem::Text(s.to_string()))
            .unwrap_or(StreamItem::Skip)),
        LlmProvider::Anthropic => match v.get("type").and_then(Value::as_str) {
            Some("content_block_delta")
                if v.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta") =>
            {
                Ok(v.pointer("/delta/text")
                    .and_then(Value::as_str)
                    .map(|s| StreamItem::Text(s.to_string()))
                    .unwrap_or(StreamItem::Skip))
            }
            Some("message_stop") => Ok(StreamItem::Done),
            _ => Ok(StreamItem::Skip),
        },
    }
}

/// Text of a non-streaming completion response.
pub(crate) fn extract_text(provider: LlmProvider, json: &Value) -> String {
    match provider {
        LlmProvider::OpenAi => json
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        LlmProvider::Anthropic => json
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<String>()
            })
            .unwrap_or_default(),
    }
}

/// Decode an SSE byte stream into text deltas. Ends at `[DONE]` / `message_stop`, at
/// end of body, or after the first transport error.
pub(crate) fn sse_text_stream<S>(provider: LlmProvider, body: S) -> TextStream
where
    S: Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
{
    struct State {
        body: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
        dec: SseDecoder,
        pending: VecDeque<Result<String>>,
        done: bool,
    }
    let name = provider_name(provider);
    let st = State {
        body: Box::pin(body),
        dec: SseDecoder::new(),
        pending: VecDeque::new(),
        done: false,
    };
    Box::pin(stream::unfold(st, move |mut st| async move {
        loop {
            if let Some(item) = st.pending.pop_front() {
                return Some((item, st));
            }
            if st.done {
                return None;
            }
            let events = match st.body.next().await {
                None => {
                    st.done = true;
                    st.dec.finish().into_iter().collect()
                }
                Some(Err(e)) => {
                    st.done = true;
                    return Some((Err(Error::transport(name)(e)), st));
                }
                Some(Ok(bytes)) => st.dec.push(&bytes),
            };
            for ev in events {
                match parse_event(provider, &ev) {
                    Ok(StreamItem::Text(t)) => st.pending.push_back(Ok(t)),
                    Ok(StreamItem::Skip) => {}
                    Ok(StreamItem::Done) => {
                        st.done = true;
                        break;
                    }
                    Err(e) => {
                        st.pending.push_back(Err(e));
                        st.done = true;
                        break;
                    }
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;

    fn msgs() -> Vec<ChatMessage> {
        vec![
            ChatMessage {
                role: Role::User,
                content: "hi".into(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "hello".into(),
            },
        ]
    }

    #[test]
    fn openai_body_shape() {
        let b = openai_body(
            "gpt-4.1-mini",
            Effort::Low,
            "SYS",
            &msgs(),
            Sampling::default(),
            true,
        );
        assert_eq!(
            b,
            json!({
                "model": "gpt-4.1-mini",
                "messages": [
                    {"role": "system", "content": "SYS"},
                    {"role": "user", "content": "hi"},
                    {"role": "assistant", "content": "hello"},
                ],
                "stream": true,
                "reasoning_effort": "low",
            })
        );
        let b = openai_body(
            "gpt-4.1-mini",
            Effort::Unset,
            "",
            &msgs()[..1],
            Sampling {
                max_tokens: Some(3),
                temperature: Some(0.0),
            },
            false,
        );
        assert_eq!(b["stream"], json!(false));
        assert!(b.get("reasoning_effort").is_none());
        assert_eq!(b["messages"].as_array().unwrap().len(), 1);
        assert_eq!(b["max_completion_tokens"], json!(3));
        assert_eq!(b["temperature"], json!(0.0));
        // Reasoning models: temperature is dropped, effort still passes through.
        let b = openai_body(
            "gpt-5-mini",
            Effort::Minimal,
            "",
            &msgs(),
            Sampling {
                max_tokens: None,
                temperature: Some(0.4),
            },
            false,
        );
        assert!(b.get("temperature").is_none());
        assert_eq!(b["reasoning_effort"], json!("minimal"));
    }

    #[test]
    fn anthropic_body_shape() {
        let b = anthropic_body(
            "claude-haiku-4-5",
            Effort::Minimal,
            "SYS",
            &msgs(),
            Sampling::default(),
            true,
        );
        assert_eq!(
            b,
            json!({
                "model": "claude-haiku-4-5",
                "max_tokens": 4096,
                "system": "SYS",
                "messages": [
                    {"role": "user", "content": "hi"},
                    {"role": "assistant", "content": "hello"},
                ],
                "stream": true,
                "output_config": {"effort": "low"},
            })
        );
        let b = anthropic_body(
            "claude-haiku-4-5",
            Effort::Unset,
            "",
            &msgs(),
            Sampling {
                max_tokens: Some(80),
                temperature: Some(0.4),
            },
            false,
        );
        assert_eq!(b["max_tokens"], json!(80));
        assert!(b.get("system").is_none());
        assert!(b.get("output_config").is_none());
        assert!(b.get("temperature").is_none());
        assert_eq!(anthropic_effort(Effort::None), Some("low"));
        assert_eq!(anthropic_effort(Effort::Xhigh), Some("xhigh"));
        assert_eq!(anthropic_effort(Effort::Max), Some("max"));
    }

    #[test]
    fn parses_openai_events() {
        let ev = |d: &str| SseEvent {
            event: None,
            data: d.to_string(),
        };
        assert_eq!(
            parse_event(
                LlmProvider::OpenAi,
                &ev(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#)
            )
            .unwrap(),
            StreamItem::Text("Hel".into())
        );
        assert_eq!(
            parse_event(
                LlmProvider::OpenAi,
                &ev(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#)
            )
            .unwrap(),
            StreamItem::Skip
        );
        assert_eq!(
            parse_event(LlmProvider::OpenAi, &ev("[DONE]")).unwrap(),
            StreamItem::Done
        );
        assert!(parse_event(LlmProvider::OpenAi, &ev(r#"{"error":{"message":"boom"}}"#)).is_err());
        assert!(parse_event(LlmProvider::OpenAi, &ev("not json")).is_err());
    }

    #[test]
    fn parses_anthropic_events() {
        let ev = |e: &str, d: &str| SseEvent {
            event: Some(e.into()),
            data: d.to_string(),
        };
        assert_eq!(
            parse_event(
                LlmProvider::Anthropic,
                &ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#)
            )
            .unwrap(),
            StreamItem::Text("Hi".into())
        );
        assert_eq!(
            parse_event(LlmProvider::Anthropic, &ev("content_block_delta", r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"..."}}"#)).unwrap(),
            StreamItem::Skip
        );
        assert_eq!(
            parse_event(
                LlmProvider::Anthropic,
                &ev("message_start", r#"{"type":"message_start"}"#)
            )
            .unwrap(),
            StreamItem::Skip
        );
        assert_eq!(
            parse_event(
                LlmProvider::Anthropic,
                &ev("message_stop", r#"{"type":"message_stop"}"#)
            )
            .unwrap(),
            StreamItem::Done
        );
        assert!(parse_event(
            LlmProvider::Anthropic,
            &ev(
                "error",
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            )
        )
        .is_err());
    }

    #[test]
    fn extracts_non_streaming_text() {
        assert_eq!(
            extract_text(
                LlmProvider::OpenAi,
                &json!({"choices":[{"message":{"content":"complete"}}]})
            ),
            "complete"
        );
        assert_eq!(
            extract_text(
                LlmProvider::OpenAi,
                &json!({"choices":[{"message":{"content":null}}]})
            ),
            ""
        );
        assert_eq!(
            extract_text(
                LlmProvider::Anthropic,
                &json!({"content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"in"},{"type":"text","text":"complete"}]})
            ),
            "incomplete"
        );
    }

    #[tokio::test]
    async fn text_stream_ends_at_done_and_carries_split_events() {
        let chunks: Vec<reqwest::Result<Bytes>> = vec![
            Ok(Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"A\"}}]}\n\ndata: {\"choi")),
            Ok(Bytes::from_static(b"ces\":[{\"delta\":{\"content\":\"B\"}}]}\n\ndata: [DONE]\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"ignored\"}}]}\n\n")),
        ];
        let out: Vec<String> = sse_text_stream(LlmProvider::OpenAi, stream::iter(chunks))
            .try_collect()
            .await
            .unwrap();
        assert_eq!(out, vec!["A", "B"]);
    }
}
