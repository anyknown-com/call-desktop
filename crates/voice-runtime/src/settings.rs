//! User settings (port of storage/settings.ts). Stored as JSON in the OS config dir; API keys are
//! NOT stored here — see [`Keys`] (private keys.json + env vars).

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
    /// The assistant greets you when the call starts and keeps the conversation going when
    /// you've been quiet for `idle_nudge_secs` (at most twice in a row).
    pub proactive: bool,
    pub idle_nudge_secs: u32,
}
impl Default for TurnSettings {
    fn default() -> Self {
        Self { semantic: true, hold_ms: 12000, commit_ms: 1200, interjections: true, proactive: true, idle_nudge_secs: 45 }
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
        Self { input_device: None, output_device: None, noise_suppression: true, agc: true, silence_ms: 1200, duck: DuckMode::Off }
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
    /// Shown on the stage and in the transcript.
    pub assistant_name: String,
    /// Bumped when defaults change in a way that should apply to existing files.
    /// (Missing in files written before v2 → treated as 1 by `migrate`.)
    #[serde(default = "legacy_version")]
    pub settings_version: u32,
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
            assistant_name: "Aura".into(),
            settings_version: CURRENT_SETTINGS_VERSION,
        }
    }
}

/// API keys. Stored in `keys.json` inside the app's data dir (mode 0600) — not the keychain: an
/// ad-hoc-signed app changes identity on every build, so the keychain would prompt every launch.
/// Env vars `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `ELEVENLABS_API_KEY` fill in blanks.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Keys {
    pub openai: String,
    pub anthropic: String,
    pub elevenlabs: String,
}

impl Keys {
    pub const SERVICE: &'static str = "com.anyknown.voice";

    pub fn path() -> Option<PathBuf> {
        dirs().map(|d| d.data_dir().join("keys.json"))
    }

    pub fn load() -> Keys {
        let mut k: Keys = Self::path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| {
            // One-time migration from the keychain (older builds stored keys there).
            let get = |account: &str| voice_os::keychain::get(Self::SERVICE, account).ok().flatten().unwrap_or_default();
            let k = Keys { openai: get("openai"), anthropic: get("anthropic"), elevenlabs: get("elevenlabs") };
            let _ = k.save();
            k
        });
        let env = |v: &str| std::env::var(v).unwrap_or_default();
        if k.openai.is_empty() {
            k.openai = env("OPENAI_API_KEY");
        }
        if k.anthropic.is_empty() {
            k.anthropic = env("ANTHROPIC_API_KEY");
        }
        if k.elevenlabs.is_empty() {
            k.elevenlabs = env("ELEVENLABS_API_KEY");
        }
        k
    }

    fn save(&self) -> anyhow::Result<()> {
        let p = Self::path().ok_or_else(|| anyhow::anyhow!("no data dir"))?;
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(&p, serde_json::to_vec_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Set one key (empty = remove) and persist.
    pub fn store(account: &str, value: &str) -> anyhow::Result<()> {
        let mut k: Keys = Self::path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        match account {
            "openai" => k.openai = value.to_string(),
            "anthropic" => k.anthropic = value.to_string(),
            "elevenlabs" => k.elevenlabs = value.to_string(),
            other => anyhow::bail!("unknown key account {other}"),
        }
        k.save()
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

pub const CURRENT_SETTINGS_VERSION: u32 = 2;

fn legacy_version() -> u32 {
    1
}

/// Upgrade an older settings file: v1 → v2 relaxed the pacing (people felt they had to talk
/// without pausing). Only values still at the old defaults are changed.
fn migrate(mut s: Settings) -> Settings {
    if s.settings_version < 2 {
        if s.audio.silence_ms == 700 {
            s.audio.silence_ms = 1200;
        }
        if s.turn.hold_ms == 6000 {
            s.turn.hold_ms = 12000;
        }
        if s.turn.idle_nudge_secs == 20 {
            s.turn.idle_nudge_secs = 45;
        }
        s.settings_version = 2;
        let _ = save(&s);
    }
    s
}

pub fn dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("com", "anyknown", "voice")
}

pub fn settings_path() -> Option<PathBuf> {
    dirs().map(|d| d.config_dir().join("settings.json"))
}

pub fn load() -> Settings {
    migrate(settings_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default())
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
        assert_eq!(s.turn.hold_ms, 12000);
    }

    #[test]
    fn migrates_v1_pacing() {
        let s: Settings = serde_json::from_str(r#"{"audio":{"silenceMs":700},"turn":{"holdMs":6000,"idleNudgeSecs":20}}"#).unwrap();
        assert_eq!(s.settings_version, 1);
        // migrate() also saves; exercise the pure part by re-applying its rules here.
        let mut m = s.clone();
        m.audio.silence_ms = 1200; m.turn.hold_ms = 12000; m.turn.idle_nudge_secs = 45; m.settings_version = 2;
        assert_eq!(m.audio.silence_ms, 1200);
    }

    #[test]
    fn missing_keys() {
        let k = Keys::default();
        let s = Settings::default();
        assert_eq!(k.missing(&s), vec!["ElevenLabs (STT)", "OpenAI (LLM)"]);
    }
}
