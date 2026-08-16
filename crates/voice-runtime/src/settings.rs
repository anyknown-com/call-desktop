//! User settings (port of storage/settings.ts). Stored as JSON in the OS config dir; API keys are
//! NOT stored here — they live in the keychain (see `voice_os::keychain`) or env vars.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use voice_providers::{Effort, LlmProvider};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TtsProvider {
    #[default]
    ElevenLabs,
    OpenAi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DuckMode {
    /// Don't touch other apps' audio.
    #[default]
    Off,
    /// Mute everything else while the assistant speaks.
    Mute,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LlmSettings {
    pub provider: LlmProvider,
    pub model: String,
    /// Unset = provider default; none/minimal/low/medium/high/xhigh/max.
    pub effort: Effort,
    pub fast_model: String,
    pub fast_effort: Effort,
}
impl Default for LlmSettings {
    fn default() -> Self {
        Self { provider: LlmProvider::OpenAi, model: "gpt-4o-mini".into(), effort: Effort::Unset, fast_model: "gpt-4o-mini".into(), fast_effort: Effort::Unset }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SttSettings {
    pub model: String,
    pub language_code: String,
}
impl Default for SttSettings {
    fn default() -> Self {
        Self { model: "scribe_v2".into(), language_code: String::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TtsSettings {
    pub provider: TtsProvider,
    pub elevenlabs_model: String,
    pub elevenlabs_voice_id: String,
    pub openai_model: String,
    pub openai_voice: String,
}
impl Default for TtsSettings {
    fn default() -> Self {
        Self {
            provider: TtsProvider::ElevenLabs,
            elevenlabs_model: "eleven_v3".into(),
            elevenlabs_voice_id: "21m00Tcm4TlvDq8ikWAM".into(),
            openai_model: "tts-1".into(),
            openai_voice: "alloy".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TurnSettings {
    /// Ask the fast model whether the user has finished (else heuristics + silence only).
    pub semantic: bool,
    /// Max silence to wait after an "incomplete" verdict before answering anyway.
    pub hold_ms: u32,
    /// Sustained speech while muted that counts as a definite interruption.
    pub commit_ms: u32,
    /// Let the fast model decide how to handle short remarks (react / ignore / stop).
    pub interjections: bool,
}
impl Default for TurnSettings {
    fn default() -> Self {
        Self { semantic: true, hold_ms: 6000, commit_ms: 1200, interjections: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioSettings {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub noise_suppression: bool,
    pub agc: bool,
    /// VAD silence before an utterance ends (ms).
    pub silence_ms: u32,
    /// Duck other apps' audio while the assistant speaks.
    pub duck: DuckMode,
}
impl Default for AudioSettings {
    fn default() -> Self {
        Self { input_device: None, output_device: None, noise_suppression: true, agc: true, silence_ms: 700, duck: DuckMode::Off }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub llm: LlmSettings,
    pub stt: SttSettings,
    pub tts: TtsSettings,
    pub turn: TurnSettings,
    pub audio: AudioSettings,
    /// Media mode: generic VAD never mutes/cuts the assistant; only the enrolled speaker's
    /// verified voice does (needs a voice profile).
    pub media_mode: bool,
    pub system_prompt: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            llm: Default::default(),
            stt: Default::default(),
            tts: Default::default(),
            turn: Default::default(),
            audio: Default::default(),
            media_mode: false,
            system_prompt: "你是一個友善、簡潔的語音助理。".into(),
        }
    }
}

/// API keys, resolved from the keychain with env-var fallback. Never serialized to disk here.
#[derive(Debug, Clone, Default)]
pub struct Keys {
    pub openai: String,
    pub anthropic: String,
    pub elevenlabs: String,
}

impl Keys {
    pub const SERVICE: &'static str = "com.anyknown.voice";

    /// Keychain first, then `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `ELEVENLABS_API_KEY`.
    pub fn load() -> Keys {
        let get = |account: &str, env: &str| {
            voice_os::keychain::get(Self::SERVICE, account).ok().flatten().filter(|s| !s.is_empty()).or_else(|| std::env::var(env).ok()).unwrap_or_default()
        };
        Keys { openai: get("openai", "OPENAI_API_KEY"), anthropic: get("anthropic", "ANTHROPIC_API_KEY"), elevenlabs: get("elevenlabs", "ELEVENLABS_API_KEY") }
    }

    pub fn store(account: &str, value: &str) -> anyhow::Result<()> {
        if value.is_empty() {
            Ok(voice_os::keychain::delete(Self::SERVICE, account)?)
        } else {
            Ok(voice_os::keychain::set(Self::SERVICE, account, value)?)
        }
    }

    /// Which keys are missing for the current configuration (mirrors settings.ts missingKeys).
    pub fn missing(&self, s: &Settings) -> Vec<&'static str> {
        let mut out = vec![];
        if self.elevenlabs.is_empty() {
            out.push("ElevenLabs (STT)");
        }
        if s.tts.provider == TtsProvider::OpenAi && self.openai.is_empty() {
            out.push("OpenAI (TTS)");
        }
        if s.llm.provider == LlmProvider::OpenAi && self.openai.is_empty() && !out.contains(&"OpenAI (TTS)") {
            out.push("OpenAI (LLM)");
        }
        if s.llm.provider == LlmProvider::Anthropic && self.anthropic.is_empty() {
            out.push("Anthropic (LLM)");
        }
        out
    }
}

pub fn dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("com", "anyknown", "voice")
}

pub fn settings_path() -> Option<PathBuf> {
    dirs().map(|d| d.config_dir().join("settings.json"))
}

pub fn load() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(s: &Settings) -> anyhow::Result<()> {
    let p = settings_path().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(p, serde_json::to_vec_pretty(s)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_json_merges_over_defaults() {
        let s: Settings = serde_json::from_str(r#"{"llm":{"provider":"anthropic"},"audio":{"duck":"mute"}}"#).unwrap();
        assert_eq!(s.llm.provider, LlmProvider::Anthropic);
        assert_eq!(s.llm.model, "gpt-4o-mini");
        assert_eq!(s.audio.duck, DuckMode::Mute);
        assert_eq!(s.turn.hold_ms, 6000);
    }

    #[test]
    fn missing_keys() {
        let k = Keys::default();
        let s = Settings::default();
        assert_eq!(k.missing(&s), vec!["ElevenLabs (STT)", "OpenAI (LLM)"]);
    }
}
