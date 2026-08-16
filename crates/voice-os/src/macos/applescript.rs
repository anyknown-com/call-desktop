//! Fallback ducker: pause Spotify / Music via `osascript`, resume on restore.
//! Both `Duck` and `Mute` pause playback (there is no attenuation).
//! The first run prompts for Automation permission ("… wants to control Spotify").

use std::process::Command;

use crate::{sentinel_path, DuckMode, MediaDucker, Result};

const APPS: [&str; 2] = ["Spotify", "Music"];

fn osascript(script: &str) -> Option<String> {
    let out = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !out.status.success() {
        tracing::debug!(
            "osascript failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Pause `app` if it is running and playing. Returns true if we paused it.
fn pause(app: &str) -> bool {
    let script = format!(
        "if application \"{app}\" is running then\n\
           tell application \"{app}\"\n\
             if player state is playing then\n pause\n return \"paused\"\n end if\n\
           end tell\n\
         end if\n\
         return \"no\""
    );
    osascript(&script).as_deref() == Some("paused")
}

pub fn resume(app: &str) {
    let script =
        format!("if application \"{app}\" is running then tell application \"{app}\" to play");
    osascript(&script);
}

pub struct AppleScriptDucker {
    paused: Vec<&'static str>,
    ducked: bool,
}

impl AppleScriptDucker {
    pub fn new() -> Self {
        Self {
            paused: Vec::new(),
            ducked: false,
        }
    }
}

impl Default for AppleScriptDucker {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaDucker for AppleScriptDucker {
    fn duck(&mut self, _mode: DuckMode) -> Result<()> {
        if self.ducked {
            return Ok(());
        }
        self.paused = APPS.into_iter().filter(|app| pause(app)).collect();
        if !self.paused.is_empty() {
            let path = sentinel_path();
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, self.paused.join("\n"))?;
        }
        self.ducked = true;
        Ok(())
    }

    fn restore(&mut self) -> Result<()> {
        if !self.ducked {
            return Ok(());
        }
        for app in self.paused.drain(..) {
            resume(app);
        }
        let _ = std::fs::remove_file(sentinel_path());
        self.ducked = false;
        Ok(())
    }

    fn is_ducked(&self) -> bool {
        self.ducked
    }

    fn backend_name(&self) -> &'static str {
        "macos-applescript"
    }
}

impl Drop for AppleScriptDucker {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
