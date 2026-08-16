//! The running call: executes `voice_core::call_machine` commands with tokio (STT/LLM/TTS
//! tasks, timers), drives the audio engine + pipeline thread, ducks other apps' audio while the
//! assistant speaks, and persists transcripts. The CLI, the GPUI app and the MCP server are thin
//! front-ends over [`Runtime`].

mod driver;
mod ducking;
pub mod enroll;
pub mod keys;
pub mod mock;
pub mod pipeline;
mod proactive;
mod providers;
pub mod settings;
pub mod transcript;

use anyhow::Result;
use driver::Driver;
use keys::Keys;
use settings::Settings;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use voice_core::call_machine::CallState;
use voice_core::media_gate::GateState;
use voice_core::speaker_profile::SpeakerProfile;

#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    State(CallState),
    /// VAD speech probability of the latest frame (for a level meter).
    Level(f32),
    GateState(GateState),
    /// Subtle UI hint (e.g. "voice not verified").
    Hint(String),
    Error(String),
    Ducked(bool),
    /// Transcript saved at hangup.
    Saved(std::path::PathBuf),
}

#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    Start,
    Hangup,
    Interrupt,
    /// Mute the microphone: speech events are dropped until unmuted.
    SetMicMuted(bool),
    Shutdown,
}

pub struct RuntimeOptions {
    pub settings: Settings,
    pub keys: Keys,
    /// Required for media mode.
    pub profile: Option<SpeakerProfile>,
    pub models_dir: std::path::PathBuf,
    /// Use offline mock providers (e2e harness).
    pub mock: bool,
    /// Feed this WAV as the microphone instead of a device (e2e harness).
    pub input_wav: Option<std::path::PathBuf>,
}

pub struct Runtime {
    pub commands: UnboundedSender<RuntimeCommand>,
    pub events: UnboundedReceiver<RuntimeEvent>,
    task: tokio::task::JoinHandle<()>,
}

impl Runtime {
    /// Build providers, start audio, spawn the driver.
    pub fn start(opts: RuntimeOptions) -> Result<Runtime> {
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, ev_rx) = unbounded_channel();
        let driver = Driver::new(opts, ev_tx)?;
        let task = tokio::spawn(driver.run(cmd_rx));
        Ok(Runtime { commands: cmd_tx, events: ev_rx, task })
    }

    pub async fn join(self) {
        let _ = self.task.await;
    }
}
