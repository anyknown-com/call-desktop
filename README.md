# voice-desktop

Open-source native desktop version of [Anyknown Call](../call) (`@anyknown/voice`): a cascaded, bring-your-own-key
voice call app (mic → VAD → STT → LLM → TTS) with barge-in, semantic turn-taking, a
speaker-verified Media mode, and **OS-level muting of other apps' audio while the AI speaks** —
the thing a browser can't do.

Rust + [GPUI](https://www.gpui.rs). macOS first; Windows/Linux later. See [`docs/spec.md`](docs/spec.md).

## Status

- **Phase 1 (headless) — done.** `voice call` runs a full call on macOS: WebRTC AEC/NS/AGC,
  Silero VAD, ElevenLabs Scribe STT, OpenAI/Anthropic streaming LLM, ElevenLabs/OpenAI TTS,
  barge-in, hold-for-mid-thought, interjection handling, Media mode (CAM++ speaker gate),
  media ducking via Core Audio process taps, transcripts saved to disk.
- **Phase 2 (GPUI app) — in progress.**

## Build

Prereqs (macOS): Rust stable, `brew install cmake meson ninja` (the bundled WebRTC audio
processing library is built from source on first `cargo build`), Xcode command line tools.

```sh
./scripts/fetch-models.sh     # Silero VAD v5 + CAM++ (sha-pinned) into ./models
cargo build --release
cargo test --workspace
```

## Use (CLI)

```sh
voice keys set elevenlabs      # stored in a private keys.json (also: openai, anthropic, llm)
voice keys set openai
voice settings                 # prints/creates ~/Library/Application Support/com.anyknown.voice/settings.json
voice devices
voice call                     # speak; `i`⏎ interrupts, `q`⏎ hangs up
voice call --duck              # mute every other app while the assistant speaks (macOS 14.2+,
                               #   needs System Audio Recording permission)
voice enroll                   # 6 clips + 1 check clip → speaker profile
voice call --media             # Media mode: only your verified voice interrupts
voice duck-test                # mute other apps for 5 s, then restore
voice call --mock --mic-wav some.wav --seconds 30   # offline e2e harness (no keys)
```

Env fallbacks for keys: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `ELEVENLABS_API_KEY`.
Any OpenAI- or Anthropic-compatible endpoint (DeepSeek, Cloudflare AI Gateway, a local server…) works:
set `llm.baseUrl` in settings (or Settings › Language model › Base URL) and `voice keys set llm`.
`VOICE_MODELS_DIR` overrides the models directory.

## Layout

```
crates/voice-core       pure logic (fbank, speaker gate FSM, sans-IO CallMachine…), golden-fixture tests
crates/voice-audio      cpal I/O, resampling, WebRTC APM, ordered playback sink, media-turn controller
crates/voice-ml         ort: Silero VAD v5, CAM++ embedder (cosine 1.0 vs reference)
crates/voice-providers  ElevenLabs STT/TTS, OpenAI/Anthropic chat (SSE), OpenAI TTS, fast-LLM judges
crates/voice-os         media ducking (Core Audio process tap / AppleScript), keychain
crates/voice-runtime    tokio driver: executes CallMachine commands, pipeline thread, ducking, transcripts
crates/voice-cli        `voice` binary
crates/voice-app        GPUI desktop app
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option. Bundled models and fonts carry their own licenses (see `models/*/MODEL_CARD.md`,
`crates/voice-app/fonts/LICENSE.txt`).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
