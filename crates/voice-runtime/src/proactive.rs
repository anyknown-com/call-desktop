//! Proactive conversation: a greeting when the call starts, then up to two gentle nudges when
//! the user has been quiet for a while.

use std::time::Instant;
use voice_core::call_machine::{CallState, CallStatus, Role};

pub const GREETING: &str = "The call just started. Greet the user warmly in one short sentence and ask one open, friendly question to get the conversation going. Use the language of your instructions.";
const NUDGE_1: &str = "The user has been quiet for a while. Gently keep the conversation going: pick up the last topic with a short, specific follow-up question, or offer one interesting related thought. One or two spoken sentences.";
const NUDGE_2: &str = "Still quiet. Say one short, light sentence to check in — no pressure, no repetition of what you already said.";

pub struct Proactivity {
    idle_secs: u32,
    /// When the call last became quietly-listening.
    listening_since: Option<Instant>,
    /// Consecutive nudges without the user saying anything.
    nudges: u32,
    user_turns: usize,
}

impl Proactivity {
    pub fn new(idle_secs: u32) -> Self {
        Self { idle_secs: idle_secs.max(5), listening_since: None, nudges: 0, user_turns: 0 }
    }

    /// Bookkeeping on every state change: reset the nudge count when the user actually says
    /// something; time the quiet stretch from the moment we return to Listening.
    pub fn observe(&mut self, st: &CallState) {
        let user_turns = st.turns.iter().filter(|t| t.role == Role::User).count();
        if user_turns != self.user_turns {
            self.user_turns = user_turns;
            self.nudges = 0;
        }
        if st.status == CallStatus::Listening {
            if self.listening_since.is_none() {
                self.listening_since = Some(Instant::now());
            }
        } else {
            self.listening_since = None;
        }
    }

    /// The nudge instruction to send now, if the user has been quiet long enough.
    pub fn due(&mut self) -> Option<&'static str> {
        let since = self.listening_since?;
        if self.nudges >= 2 || since.elapsed().as_secs() < self.idle_secs as u64 {
            return None;
        }
        let instruction = if self.nudges == 0 { NUDGE_1 } else { NUDGE_2 };
        self.nudges += 1;
        self.listening_since = None;
        Some(instruction)
    }
}
