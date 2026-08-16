# voice-desktop

Open-source native desktop version of [`@anyknown/voice`](../voice): a cascaded, bring-your-own-key
voice call app (mic → VAD → STT → LLM → TTS) with barge-in, a speaker-verified Media mode, and
**OS-level ducking of other apps' audio while the AI speaks** — the thing a browser can't do.

Rust + [GPUI](https://www.gpui.rs). macOS first; Windows/Linux later.

Status: Phase 0 — `voice-core` port in progress. See [`docs/spec.md`](docs/spec.md).

```
cargo test
```
