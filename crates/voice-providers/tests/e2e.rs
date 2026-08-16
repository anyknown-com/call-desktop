//! Hermetic end-to-end tests against a local `wiremock` server.

use futures::TryStreamExt;
use serde_json::{json, Value};
use voice_core::call_machine::{ChatMessage, InterjectionDecision, Role};
use voice_core::turn_heuristics::TurnVerdict;
use voice_providers::*;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn user(s: &str) -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content: s.into(),
    }
}

#[tokio::test]
async fn anthropic_agent_streams_text_deltas() {
    let server = MockServer::start().await;
    let sse = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(body_partial_json(json!({
            "model": "claude-haiku-4-5",
            "stream": true,
            "max_tokens": 4096,
            "output_config": {"effort": "low"},
            "system": format!("Be brief.\n\n{SPOKEN_STYLE_HINT}"),
            "messages": [{"role": "user", "content": "hi"}],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let agent = Agent::new(AgentConfig {
        provider: LlmProvider::Anthropic,
        model: "claude-haiku-4-5".into(),
        api_key: "sk-test".into(),
        system_prompt: "Be brief.".into(),
        effort: Effort::Minimal,
    })
    .with_base_url(server.uri());
    let deltas: Vec<String> = agent
        .run(vec![user("hi")])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(deltas, vec!["Hel", "lo"]);
}

#[tokio::test]
async fn openai_agent_streams_and_reports_http_errors() {
    let server = MockServer::start().await;
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"A\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"B\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-o"))
        .and(body_partial_json(
            json!({"model": "gpt-4.1-mini", "stream": true, "reasoning_effort": "high"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let cfg = AgentConfig {
        provider: LlmProvider::OpenAi,
        model: "gpt-4.1-mini".into(),
        api_key: "sk-o".into(),
        system_prompt: String::new(),
        effort: Effort::High,
    };
    let agent = Agent::new(cfg.clone()).with_base_url(server.uri());
    let deltas: Vec<String> = agent
        .run(vec![user("hi")])
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(deltas, vec!["A", "B"]);

    let bad = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_string("x".repeat(500)))
        .mount(&bad)
        .await;
    let agent = Agent::new(cfg).with_base_url(bad.uri());
    match agent.run(vec![user("hi")]).await {
        Err(Error::Http {
            provider,
            status,
            body,
        }) => {
            assert_eq!((provider, status, body.len()), ("openai", 429, 300));
        }
        Err(other) => panic!("expected Http error, got {other:?}"),
        Ok(_) => panic!("expected Http error, got a stream"),
    }
}

#[tokio::test]
async fn fast_llm_judges_and_decides() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(
            json!({"stream": false, "max_completion_tokens": 3}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "Incomplete"}}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(json!({"stream": false, "max_completion_tokens": 80})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "{\"action\":\"react\",\"reaction\":\"ha\"}"}}]
        })))
        .mount(&server)
        .await;
    let fast = FastLlm::new(FastLlmConfig {
        provider: LlmProvider::OpenAi,
        model: "gpt-4.1-nano".into(),
        api_key: "k".into(),
        effort: Effort::Unset,
    })
    .with_base_url(server.uri());
    let history = vec![user("so I was thinking\n[interrupted by user]")];
    assert_eq!(
        fast.judge(&history, "and then").await.unwrap(),
        TurnVerdict::Incomplete
    );
    assert_eq!(
        fast.decide(&history, "spoken", "playing", "lol")
            .await
            .unwrap(),
        InterjectionDecision::React {
            reaction: "ha".into()
        }
    );

    // The judge prompt: system = instructions, single user message with the recent() block.
    let reqs = server.received_requests().await.unwrap();
    let judge: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(judge["temperature"], json!(0.0));
    assert_eq!(judge["messages"][0]["role"], "system");
    assert!(judge["messages"][0]["content"]
        .as_str()
        .unwrap()
        .starts_with("You are the turn-taking judge"));
    assert_eq!(
        judge["messages"][1]["content"],
        "Conversation so far:\nUser: so I was thinking\n\nUser's current turn so far:\n\"\"\"and then\"\"\"\n\nOne word:"
    );
    let decide: Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!((decide["temperature"].as_f64().unwrap() - 0.4).abs() < 1e-6);
}

#[tokio::test]
async fn elevenlabs_stt_posts_wav_multipart() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/speech-to-text"))
        .and(header("xi-api-key", "xi"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "  hello there \n"})))
        .mount(&server)
        .await;
    let stt = ElevenLabsStt::new(ElevenLabsSttConfig {
        api_key: "xi".into(),
        model: "scribe_v2".into(),
        language_code: "zh".into(),
    })
    .with_base_url(server.uri());
    assert_eq!(
        stt.transcribe(&[0.0, 0.5, -0.5], 16_000).await.unwrap(),
        "hello there"
    );

    let req = &server.received_requests().await.unwrap()[0];
    let ct = req
        .headers
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.starts_with("multipart/form-data; boundary="), "{ct}");
    let body = String::from_utf8_lossy(&req.body).into_owned();
    for needle in [
        "name=\"model_id\"\r\n\r\nscribe_v2",
        "name=\"file\"; filename=\"utterance.wav\"",
        "content-type: audio/wav",
        "name=\"tag_audio_events\"\r\n\r\nfalse",
        "name=\"language_code\"\r\n\r\nzh",
        "RIFF",
    ] {
        assert!(
            body.to_lowercase().contains(&needle.to_lowercase()),
            "missing {needle:?} in body"
        );
    }
}

#[tokio::test]
async fn elevenlabs_tts_pcm_then_mp3_fallback() {
    let server = MockServer::start().await;
    // Odd-length body: the last byte of sample 2 arrives with a later flush; still one stream.
    Mock::given(method("POST"))
        .and(path("/v1/text-to-speech/voice%201/stream"))
        .and(query_param("output_format", "pcm_24000"))
        .and(header("xi-api-key", "xi"))
        .and(body_partial_json(
            json!({"text": "hi", "model_id": "eleven_v3"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0xff, 0x7f, 0x00, 0x80]))
        .mount(&server)
        .await;
    let tts = ElevenLabsTts::new(ElevenLabsTtsConfig {
        api_key: "xi".into(),
        model: "eleven_v3".into(),
        voice_id: "voice 1".into(),
    })
    .with_base_url(server.uri());
    let pcm = tts.synthesize("hi").await.unwrap();
    assert_eq!(pcm.sample_rate, 24_000);
    let samples: Vec<f32> = pcm.chunks.try_collect::<Vec<_>>().await.unwrap().concat();
    assert_eq!(samples, vec![32767.0 / 32768.0, -1.0]);
    assert!(!tts.pcm_refused());

    // A tier that refuses PCM: 403 → remembered → mp3 requested. Garbage mp3 surfaces as a decode error.
    let refusing = MockServer::start().await;
    Mock::given(query_param("output_format", "pcm_24000"))
        .respond_with(ResponseTemplate::new(403).set_body_string("pcm not allowed"))
        .expect(1)
        .mount(&refusing)
        .await;
    Mock::given(query_param("output_format", "mp3_44100_128"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1, 2, 3, 4]))
        .expect(2)
        .mount(&refusing)
        .await;
    let tts = ElevenLabsTts::new(ElevenLabsTtsConfig {
        api_key: "xi".into(),
        model: "eleven_v3".into(),
        voice_id: "v".into(),
    })
    .with_base_url(refusing.uri());
    assert!(matches!(
        tts.synthesize("a").await,
        Err(Error::Protocol {
            provider: "elevenlabs-tts",
            ..
        })
    ));
    assert!(tts.pcm_refused());
    // Second call goes straight to mp3 (the pcm mock's expect(1) enforces this on drop).
    assert!(matches!(
        tts.synthesize("b").await,
        Err(Error::Protocol { .. })
    ));

    // Any other failure status is a plain HTTP error.
    let broken = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("nope"))
        .mount(&broken)
        .await;
    let tts = ElevenLabsTts::new(ElevenLabsTtsConfig {
        api_key: "xi".into(),
        model: "m".into(),
        voice_id: "v".into(),
    })
    .with_base_url(broken.uri());
    assert!(matches!(
        tts.synthesize("a").await,
        Err(Error::Http { status: 500, .. })
    ));
    assert!(!tts.pcm_refused());
}

#[tokio::test]
async fn openai_tts_streams_pcm() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .and(header("authorization", "Bearer sk"))
        .and(body_partial_json(
            json!({"model": "tts-1", "input": "hey", "voice": "alloy", "response_format": "pcm"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0x00, 0x40, 0x00, 0xc0]))
        .mount(&server)
        .await;
    let tts = OpenAiTts::new(OpenAiTtsConfig {
        api_key: "sk".into(),
        model: "tts-1".into(),
        voice: "alloy".into(),
    })
    .with_base_url(server.uri());
    let pcm = tts.synthesize("hey").await.unwrap();
    assert_eq!(pcm.sample_rate, 24_000);
    let samples: Vec<f32> = pcm.chunks.try_collect::<Vec<_>>().await.unwrap().concat();
    assert_eq!(samples, vec![0.5, -0.5]);
}
