//! Mute other apps' audio while the assistant speaks: duck when playback becomes active,
//! restore (debounced) when it goes quiet, on hangup, and on shutdown.

use crate::RuntimeEvent;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::AbortHandle;
use voice_core::call_machine::{Input, TimerId};

/// Timer id reserved for the ducking debounce (machine ids start at 1 and count up; u64::MAX
/// can never collide).
pub const DUCK_RESTORE_TIMER: TimerId = TimerId(u64::MAX);

pub struct Ducking {
    ducker: Box<dyn voice_os::MediaDucker>,
    restore_handle: Option<AbortHandle>,
    inputs: UnboundedSender<Input>,
    events: UnboundedSender<RuntimeEvent>,
}

impl Ducking {
    /// Pick a backend; reports (and returns `None`) if none is available.
    pub fn open(inputs: UnboundedSender<Input>, events: UnboundedSender<RuntimeEvent>) -> Option<Ducking> {
        match voice_os::create_ducker() {
            Ok(d) => {
                tracing::info!(backend = d.backend_name(), "media ducking enabled");
                Some(Ducking { ducker: d, restore_handle: None, inputs, events })
            }
            Err(e) => {
                let _ = events.send(RuntimeEvent::Error(format!("media ducking unavailable: {e}")));
                None
            }
        }
    }

    /// Playback became active/inactive.
    pub fn on_active(&mut self, on: bool) {
        if let Some(h) = self.restore_handle.take() {
            h.abort();
        }
        if on {
            if !self.ducker.is_ducked() {
                match self.ducker.duck(voice_os::DuckMode::Mute) {
                    Ok(()) => {
                        let _ = self.events.send(RuntimeEvent::Ducked(true));
                    }
                    Err(e) => {
                        let _ = self.events.send(RuntimeEvent::Error(format!("duck: {e}")));
                    }
                }
            }
        } else {
            // Debounce so back-to-back sentences don't flap.
            let tx = self.inputs.clone();
            let h = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let _ = tx.send(Input::Timer { id: DUCK_RESTORE_TIMER });
            });
            self.restore_handle = Some(h.abort_handle());
        }
    }

    /// The debounce timer fired.
    pub fn on_restore_timer(&mut self) {
        self.restore_handle = None;
        self.restore();
    }

    pub fn restore(&mut self) {
        if let Some(h) = self.restore_handle.take() {
            h.abort();
        }
        if self.ducker.is_ducked() {
            if let Err(e) = self.ducker.restore() {
                let _ = self.events.send(RuntimeEvent::Error(format!("restore audio: {e}")));
            }
            let _ = self.events.send(RuntimeEvent::Ducked(false));
        }
    }
}
