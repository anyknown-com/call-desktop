//! Transcript persistence: one JSON + one Markdown file per call under the OS data dir.

use crate::settings::dirs;
use crate::usage::Usage;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use voice_core::call_machine::{Role, Turn, TurnKind};

/// What a `<stamp>.json` holds. Files written before `usage` existed are a bare `Vec<Turn>`;
/// [`CallRecord::from_json`] reads both.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CallRecord {
    pub turns: Vec<Turn>,
    pub usage: Usage,
}

impl CallRecord {
    pub fn from_json(bytes: &[u8]) -> Result<CallRecord> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Record(CallRecord),
            Legacy(Vec<Turn>),
        }
        Ok(match serde_json::from_slice::<Either>(bytes)? {
            Either::Record(r) => r,
            Either::Legacy(turns) => CallRecord { turns, ..Default::default() },
        })
    }
}

pub fn calls_dir() -> Option<PathBuf> {
    dirs().map(|d| d.data_dir().join("calls"))
}

/// Returns the JSON path, or None when there was nothing to save.
pub fn save(turns: &[Turn], usage: &Usage) -> Result<Option<PathBuf>> {
    if turns.is_empty() {
        return Ok(None);
    }
    let dir = calls_dir().ok_or_else(|| anyhow::anyhow!("no data dir"))?;
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let json = dir.join(format!("{stamp}.json"));
    let record = CallRecord { turns: turns.to_vec(), usage: usage.clone() };
    std::fs::write(&json, serde_json::to_vec_pretty(&record)?)?;
    std::fs::write(dir.join(format!("{stamp}.md")), to_markdown(turns) + &usage.to_markdown())?;
    Ok(Some(json))
}

pub fn to_markdown(turns: &[Turn]) -> String {
    let mut out = String::new();
    for t in turns {
        let who = match (&t.role, t.kind) {
            (Role::User, Some(TurnKind::Interjection)) => "**User** (aside)",
            (Role::User, _) => "**User**",
            (Role::Assistant, Some(TurnKind::Reaction)) => "**Assistant** (reaction)",
            (Role::Assistant, _) => "**Assistant**",
        };
        let flag = if t.interrupted { " _(interrupted)_" } else { "" };
        out.push_str(&format!("{who}{flag}: {}\n\n", t.text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use voice_core::call_machine::TurnId;

    #[test]
    fn markdown_shape() {
        let turns = vec![
            Turn { id: TurnId(1), role: Role::User, text: "hi".into(), at: 0.0, interrupted: false, kind: None, is_final: true },
            Turn { id: TurnId(2), role: Role::Assistant, text: "hello".into(), at: 0.0, interrupted: true, kind: None, is_final: true },
        ];
        let md = to_markdown(&turns);
        assert_eq!(md, "**User**: hi\n\n**Assistant** _(interrupted)_: hello\n\n");
    }

    #[test]
    fn reads_legacy_and_current_json() {
        let legacy = br#"[{"id":1,"role":"user","text":"hi","at":0.0,"interrupted":false,"kind":null,"is_final":true}]"#;
        let r = CallRecord::from_json(legacy).unwrap();
        assert_eq!(r.turns.len(), 1);
        assert_eq!(r.usage, Usage::default());
        let current = serde_json::to_vec(&CallRecord { turns: r.turns.clone(), usage: { let mut u = Usage::default(); u.tts_chars = 7; u } }).unwrap();
        let r2 = CallRecord::from_json(&current).unwrap();
        assert_eq!((r2.turns.len(), r2.usage.tts_chars), (1, 7));
    }
}
