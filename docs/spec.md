# voice-desktop — Rust + GPUI rewrite spec (v0, 2026-08-16)

Open-source native desktop version of `@anyknown/voice`: a cascaded, BYOK, no-server voice
call app (mic → VAD → batch STT → LLM → TTS → speaker) with barge-in, semantic end-of-turn,
speaker-gated Media mode, and — the reason this exists — **OS-level media ducking while the AI
speaks**, which a browser cannot do.

Decisions settled with the owner (2026-08-16):

1. **macOS first.** Windows/Linux are Phase 3; nothing in Phases 1–2 may hard-block them.
2. **AEC = WebRTC APM** (`webrtc-audio-processing` crate) on all platforms. Apple
   voice-processing IO is *not* used, so the echo path is one implementation everywhere.
3. **The web repo (`voice/`) is frozen** as reference + fixture generator; it becomes the landing
   page. Later: an MCP server so Claude Code / Codex can drive the app (see §7).

Everything the web version already settled (see `voice/docs/speaker-gate-spec.md`,
`voice/README.md`) carries over unchanged unless stated here: cascaded only, no realtime/streaming
STT, ElevenLabs `scribe_v2` + `eleven_v3` default, OpenAI `tts-1` fallback, OpenAI/Anthropic
chat, 700 ms VAD silence, semantic end-of-turn with fast-model judge, interjection handling,
Media-mode speaker gate with the FINAL v1 parameters.

---

## 1. Workspace layout

```
crates/
  voice-core/       pure logic, no I/O, no async runtime. Ported from voice/src/core.
                    fbank, cosine, thresholds, speaker profile, media-gate FSM,
                    segmenter, turn heuristics, tts queue FSM, echo filter, call-session FSM.
                    Golden fixtures + ported vitest cases are the acceptance tests.
  voice-audio/      cpal I/O, resampler, WebRTC APM (AEC/NS/AGC), Silero VAD + CAM++ via `ort`,
                    PCM ring buffer, playback sink. Owns the real-time thread.
  voice-providers/  reqwest: ElevenLabs STT/TTS, OpenAI chat+TTS, Anthropic chat. Hand-rolled SSE.
  voice-os/         per-OS: media ducking, keychain for API keys, global hotkey, now-playing.
  voice-cli/        headless call runner (Phase 1 deliverable). Also the e2e harness:
                    `--mic-wav`, `--speaker-wav`, `--mock-providers`.
  voice-app/        GPUI shell (Phase 2).
fixtures/speaker/   copied from voice/test/fixtures/speaker (golden fbank/embeddings, corpus manifest).
docs/
```

Rules:
- `voice-core` compiles with `no_std`-ish discipline in spirit: `std` allowed, but no threads,
  no clocks, no I/O. Time is always a `u64`/`f64` ms parameter passed in. This is what made
  the TS version testable; keep it.
- Cross-crate messages are plain enums, not trait objects, so the CLI and the GPUI app share
  one `Event`/`Command` vocabulary.

## 2. Audio pipeline (voice-audio)

```
cpal input (device rate) ─► resample 48k ─► APM.process_capture ─► split
                                              ▲                     ├─► resample 16k ─► Silero VAD (ort)
                                              │                     ├─► 16k ring buffer (4 s) ─► CAM++ scoring (media mode)
                                              │                     └─► 16k turn buffer ─► STT
                                   APM.process_render ◄─ playback mix (TTS PCM) ─► cpal output
```

- **APM runs at 48 kHz, 10 ms frames**, mono capture, mono render. Both cpal streams are
  resampled to 48 k first; device sample rates vary.
- **Far-end alignment is the correctness-critical piece.** The render frame handed to
  `process_render` must be the *same* 10 ms of PCM that is being written to the output stream.
  Implementation: the output callback pulls 10 ms blocks from the TTS mix ring, and for each block
  it (a) copies it to the device buffer and (b) pushes a copy into a lock-free SPSC queue that the
  capture callback drains into `process_render` before `process_capture`. Measure residual echo
  with the CLI harness (`--speaker-wav` loops a known TTS clip; assert VAD does not fire on it).
- APM config: AEC on (mobile mode off), NS moderate, AGC2 adaptive digital, HPF on. Expose the
  three toggles in settings; keep AEC always on.
- Silero VAD: same ONNX as `@ricky0123/vad-web` (v5), 16 k, 512-sample windows, thresholds
  ported from the web config (`positiveSpeechThreshold`, `redemptionFrames` = 700 ms).
- CAM++: identical model file (pin sha `aa3cfc16…ceba2`), `ort` CPU EP; CoreML EP later if p95
  inference > 100 ms.
- Playback sink: single mixer, TTS PCM at provider rate resampled to 48 k. `TtsQueue` FSM in
  `voice-core` decides pause/resume/abort; the sink just obeys.

## 3. Media ducking (voice-os) — the headline feature

Toggle: **"Duck other audio while the AI speaks"** (default off; on when enabled by the user).
Two levels the user can pick: *mute* (0) or *duck* (−20 dB). Restore on TTS end, on call end, and
on crash (write a sentinel; on next launch, restore anything left ducked).

| OS | Mechanism | Scope | Phase |
|---|---|---|---|
| macOS 14.2+ | Core Audio **Process Tap** (`AudioHardwareCreateProcessTap` / aggregate device) — tap every process except ours, apply gain in the tap callback | every app incl. browsers | 1 |
| macOS < 14.2 fallback | AppleScript `tell application "Spotify"/"Music" to pause` + `MPNowPlayingInfoCenter` playing state so well-behaved apps yield | media apps only | 1 (fallback) |
| Windows | `IAudioSessionManager2` → enumerate sessions, `ISimpleAudioVolume::SetMasterVolume` per session ≠ our PID | every app | 3 |
| Linux | PipeWire/PulseAudio: set volume on every sink-input ≠ ours; MPRIS `Pause` as courtesy | every app | 3 |

Trigger points (from `voice-core`'s `TtsQueue`): `Speaking` → duck, `Idle`/`Paused` (barge-in)
→ restore. Debounce restore by 250 ms so back-to-back sentences don't flap.

Interaction with Media mode: when ducking is on, the media the speaker gate exists to reject is
already attenuated during TTS, so false cuts during playback drop; the gate still matters when
the AI is silent. No logic change; note it in the UI copy.

## 4. Providers (voice-providers)

- All HTTP via `reqwest` + `rustls`. Streaming chat via a small SSE decoder (no AI-SDK
  equivalent needed; the web version's `instructions`/tool shapes are trivial to reproduce).
- Same provider matrix and defaults as web. Keys live in the OS keychain (`voice-os`), never in
  plain config files.
- Batch STT sends the 16 k turn WAV exactly as web does; TTS returns PCM/MP3 → decode
  (`symphonia`) → resample → sink.

## 5. voice-core port order and acceptance

Port in dependency order; each module lands with its ported tests.

| Module (TS → Rust) | Acceptance |
|---|---|
| `kaldi-fbank` → `fbank.rs` | golden `.fbank.f32`: max abs err < 1e-3 (bins > −12), < 5e-3 floor bins, mean < 1e-4; frame counts match |
| `cosine`, `thresholds` → same | ported unit tests |
| `media-gate` → `media_gate.rs` | all 24 vitest cases ported 1:1 |
| `speaker-profile` | ported tests; embedding golden via `ort` in voice-audio (cosine > 0.9995, pairwise diff < 5e-4) |
| `segmenter`, `turn-heuristics`, `echo-filter`, `tts-queue` | ported tests |
| `call-session` | ported tests (460 lines — the biggest, do last) |

## 6. Phases

**Phase 1 — headless call works (voice-cli).** Real mic/speaker on macOS, real providers,
barge-in via APM+VAD, Media mode with an enrolled profile loaded from a JSON file, ducking via
Process Tap. Exit criteria: (a) 10-minute conversation with no echo-triggered barge-in;
(b) 2-hour media-only loop, zero false cuts (spec Phase 3 test) — run *with* ducking off;
(c) ducking restores correctly on ctrl-c.

**Phase 2 — GPUI app.** Call view (transcript, state pill, interrupt button), settings
(providers/keys/effort/ducking/media mode), enrollment wizard, history (sqlite) + export.
macOS bundle, signed + notarized, Sparkle-style updates later.

**Phase 3 — Windows/Linux** ducking backends, cpal edge cases, packaging.

## 7. Future: MCP server

The app exposes a local MCP server (stdio launcher + optional localhost HTTP) so Claude Code /
Codex can: start/stop a call, inject a system instruction, read the live transcript, speak a
string via TTS, and subscribe to turn events. Architectural consequence *now*: everything the UI
can do goes through the same `Command`/`Event` enums the CLI uses; the MCP server is just a third
front-end. Do not put logic in GPUI views.

## 8. Non-goals (v1)

Realtime/streaming STT or speech-to-speech models, server components, telemetry, mobile,
Apple voice-processing AEC, per-app ducking UI (all-or-nothing except our own process).
