//! API keys: a private `keys.json` in the app's data dir, with env-var fallbacks.

use crate::settings::{dirs, LlmProvider, Settings, TtsProvider};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// API keys. Stored in `keys.json` inside the app's data dir (mode 0600) — not the keychain: an
/// ad-hoc-signed app changes identity on every build, so the keychain would prompt every launch.
/// Env vars `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `ELEVENLABS_API_KEY` fill in blanks.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Keys {
    pub openai: String,
    pub anthropic: String,
    pub elevenlabs: String,
    /// Key for a custom LLM endpoint (`LlmSettings::base_url`); falls back to the provider key.
    pub llm: String,
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
            let k = Keys { openai: get("openai"), anthropic: get("anthropic"), elevenlabs: get("elevenlabs"), llm: String::new() };
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
            "llm" => k.llm = value.to_string(),
            other => anyhow::bail!("unknown key account {other}"),
        }
        k.save()
    }

    /// The key the chat/judge LLM authenticates with: the custom-endpoint key when a
    /// `base_url` is set and one is stored, else the provider's own key.
    pub fn llm_key(&self, s: &Settings) -> &str {
        if !s.llm.base_url.is_empty() && !self.llm.is_empty() {
            return &self.llm;
        }
        match s.llm.provider {
            LlmProvider::OpenAi => &self.openai,
            LlmProvider::Anthropic => &self.anthropic,
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
        if self.llm_key(s).is_empty() {
            let what = match (s.llm.base_url.is_empty(), s.llm.provider) {
                (false, _) => "LLM endpoint",
                (true, LlmProvider::OpenAi) => "OpenAI (LLM)",
                (true, LlmProvider::Anthropic) => "Anthropic (LLM)",
            };
            // Don't list OpenAI twice when it's already missing for TTS.
            if !(what == "OpenAI (LLM)" && out.contains(&"OpenAI (TTS)")) {
                out.push(what);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys() {
        let k = Keys::default();
        let s = Settings::default();
        assert_eq!(k.missing(&s), vec!["ElevenLabs (STT)", "OpenAI (LLM)"]);
    }

    #[test]
    fn custom_endpoint_key() {
        let mut s = Settings::default();
        s.llm.base_url = "https://api.deepseek.com".into();
        let mut k = Keys { elevenlabs: "e".into(), ..Default::default() };
        assert_eq!(k.missing(&s), vec!["LLM endpoint"]);
        k.openai = "o".into();
        assert_eq!(k.llm_key(&s), "o"); // provider key still works as a fallback
        k.llm = "d".into();
        assert_eq!(k.llm_key(&s), "d");
        s.llm.base_url.clear();
        assert_eq!(k.llm_key(&s), "o");
    }
}
