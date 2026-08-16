//! OS integration for voice-desktop: media ducking, crash-safe restore and keychain access.
//!
//! # Media ducking
//!
//! [`create_ducker`] picks the best available backend:
//!
//! | OS | Backend | Scope | Notes |
//! |---|---|---|---|
//! | macOS 14.2+ | Core Audio process tap (`CATapMuted`) | every other process | needs the **System Audio Recording** permission (TCC prompt on first use; enable under *System Settings › Privacy & Security › Screen & System Audio Recording* if denied). Only `Mute` is supported; `Duck` behaves as `Mute`. |
//! | macOS (fallback) | AppleScript `pause`/`play` on Spotify and Music | those two apps only | prompts for Automation permission the first time. |
//! | Linux / Windows | none yet | | `create_ducker` returns [`Error::Unsupported`]. |
//!
//! Every backend restores on [`MediaDucker::restore`] and on `Drop`. Call [`recover_after_crash`]
//! at startup to undo a duck left over by a previous crash.

use std::path::PathBuf;

pub mod keychain;
#[cfg(target_os = "macos")]
mod macos;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported on this platform: {0}")]
    Unsupported(String),
    #[error("{call} failed with OSStatus {status} ('{fourcc}'){hint}")]
    CoreAudio {
        call: &'static str,
        status: i32,
        fourcc: String,
        hint: &'static str,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("keychain: {0}")]
    Keychain(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DuckMode {
    /// Silence other apps completely.
    Mute,
    /// Attenuate other apps by `gain_db` (negative, e.g. -20.0). Backends that cannot attenuate
    /// treat this as `Mute`; see [`MediaDucker::backend_name`] docs of each backend.
    Duck { gain_db: f32 },
}

pub trait MediaDucker: Send {
    /// Duck other apps' audio. Calling it while already ducked is a no-op.
    fn duck(&mut self, mode: DuckMode) -> Result<()>;
    /// Undo [`duck`](Self::duck). Idempotent.
    fn restore(&mut self) -> Result<()>;
    fn is_ducked(&self) -> bool;
    /// Short human-readable backend identifier, e.g. `"macos-process-tap"`.
    fn backend_name(&self) -> &'static str;
}

/// Create the best ducker for this OS. On macOS, tries the process-tap backend first and falls
/// back to AppleScript if that fails (older macOS, permission denied, ...).
pub fn create_ducker() -> Result<Box<dyn MediaDucker>> {
    #[cfg(target_os = "macos")]
    {
        Ok(select_backend(macos::tap::ProcessTapDucker::new))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(Error::Unsupported(
            "media ducking is not implemented for this OS yet".into(),
        ))
    }
}

/// Backend selection: preferred backend if it initialises, otherwise the AppleScript fallback.
#[cfg(target_os = "macos")]
fn select_backend<T: MediaDucker + 'static>(
    preferred: impl FnOnce() -> Result<T>,
) -> Box<dyn MediaDucker> {
    match preferred() {
        Ok(d) => Box::new(d),
        Err(e) => {
            tracing::warn!("process-tap ducker unavailable ({e}); falling back to AppleScript");
            Box::new(macos::applescript::AppleScriptDucker::new())
        }
    }
}

/// Path of the crash sentinel: `<temp>/voice-desktop/duck.lock`. Exists while the AppleScript
/// backend has media paused; its content lists the apps it paused, one per line.
pub fn sentinel_path() -> PathBuf {
    std::env::temp_dir().join("voice-desktop").join("duck.lock")
}

/// Undo a duck left behind by a crashed previous run. Process taps die with their process, so
/// only the AppleScript backend needs recovery: resume whatever the sentinel says was paused.
pub fn recover_after_crash() {
    let path = sentinel_path();
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    #[cfg(target_os = "macos")]
    for app in contents.lines().filter(|l| !l.is_empty()) {
        macos::applescript::resume(app);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = contents;
    let _ = std::fs::remove_file(&path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_lives_in_temp_dir() {
        let p = sentinel_path();
        assert!(p.starts_with(std::env::temp_dir()));
        assert!(p.ends_with("voice-desktop/duck.lock"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn falls_back_to_applescript_when_tap_fails() {
        let d = select_backend(|| -> Result<macos::tap::ProcessTapDucker> {
            Err(Error::Unsupported("fake tap failure".into()))
        });
        assert_eq!(d.backend_name(), "macos-applescript");
        assert!(!d.is_ducked());
    }
}
