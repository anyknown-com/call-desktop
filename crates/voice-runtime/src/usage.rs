//! Per-call usage counters, for working out what a minute of conversation costs. Character and
//! audio-second counts (not provider-billed tokens): enough to price a call to within a few
//! percent, and available without provider support. Roughly 4 chars ≈ 1 token.

use crate::settings::Settings;
use serde::{Deserialize, Serialize};
use voice_core::call_machine::{ChatMessage, Command, Input};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Usage {
    /// Wall-clock from `Start` to `Hangup`.
    pub call_ms: f64,
    pub stt_model: String,
    pub stt_calls: u32,
    pub stt_audio_ms: f64,
    pub tts_model: String,
    pub tts_calls: u32,
    pub tts_chars: u64,
    pub llm_model: String,
    pub agent_calls: u32,
    pub agent_prompt_chars: u64,
    pub agent_output_chars: u64,
    pub fast_model: String,
    pub judge_calls: u32,
    pub judge_prompt_chars: u64,
    pub decide_calls: u32,
    pub decide_prompt_chars: u64,
    #[serde(skip)]
    started_at: Option<f64>,
}

fn chars(history: &[ChatMessage]) -> u64 {
    history.iter().map(|m| m.content.chars().count() as u64).sum()
}

impl Usage {
    pub fn new(s: &Settings) -> Usage {
        let tts_model = match s.tts.provider {
            crate::settings::TtsProvider::ElevenLabs => format!("elevenlabs/{}", s.tts.elevenlabs_model),
            crate::settings::TtsProvider::OpenAi => format!("openai/{}", s.tts.openai_model),
        };
        Usage {
            stt_model: format!("elevenlabs/{}", s.stt.model),
            tts_model,
            llm_model: format!("{:?}/{}", s.llm.provider, s.llm.model).to_lowercase(),
            fast_model: format!("{:?}/{}", s.llm.provider, s.llm.fast_model).to_lowercase(),
            ..Default::default()
        }
    }

    /// Called for every input the machine handles, at time `now` (ms).
    pub fn on_input(&mut self, input: &Input, now: f64) {
        match input {
            Input::Start => self.started_at = Some(now),
            Input::Hangup => {
                if let Some(t) = self.started_at.take() {
                    self.call_ms = now - t;
                }
            }
            Input::AgentDelta { delta, .. } => self.agent_output_chars += delta.chars().count() as u64,
            _ => {}
        }
    }

    /// Called for every command the machine emits.
    pub fn on_command(&mut self, c: &Command) {
        match c {
            Command::Transcribe { audio, sample_rate, .. } => {
                self.stt_calls += 1;
                self.stt_audio_ms += audio.len() as f64 * 1000.0 / *sample_rate as f64;
            }
            Command::Synthesize { text, .. } => {
                self.tts_calls += 1;
                self.tts_chars += text.chars().count() as u64;
            }
            Command::RunAgent { history, nudge, .. } => {
                self.agent_calls += 1;
                self.agent_prompt_chars += chars(history) + nudge.as_ref().map_or(0, |n| n.chars().count() as u64);
            }
            Command::Judge { history, utterance, .. } => {
                self.judge_calls += 1;
                self.judge_prompt_chars += chars(history) + utterance.chars().count() as u64;
            }
            Command::Decide { history, spoken_so_far, playing, interjection, .. } => {
                self.decide_calls += 1;
                self.decide_prompt_chars += chars(history) + [spoken_so_far, playing, interjection].iter().map(|s| s.chars().count() as u64).sum::<u64>();
            }
            _ => {}
        }
    }

    pub fn to_markdown(&self) -> String {
        let min = self.call_ms / 60_000.0;
        format!(
            "---\n\
             call: {min:.1} min · stt: {} calls, {:.0} s audio ({}) · tts: {} calls, {} chars ({}) · \
             agent: {} calls, {} prompt chars, {} output chars ({}) · judge: {} calls, {} chars · decide: {} calls, {} chars ({})\n",
            self.stt_calls,
            self.stt_audio_ms / 1000.0,
            self.stt_model,
            self.tts_calls,
            self.tts_chars,
            self.tts_model,
            self.agent_calls,
            self.agent_prompt_chars,
            self.agent_output_chars,
            self.llm_model,
            self.judge_calls,
            self.judge_prompt_chars,
            self.decide_calls,
            self.decide_prompt_chars,
            self.fast_model,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voice_core::call_machine::{ReqId, Role, SegmentId, TurnId};

    #[test]
    fn counts_commands_and_call_time() {
        let mut u = Usage::new(&Settings::default());
        u.on_input(&Input::Start, 1000.0);
        u.on_command(&Command::Transcribe { req: ReqId(1), audio: vec![0.0; 16_000], sample_rate: 16_000, timeout_ms: 1.0 });
        u.on_command(&Command::RunAgent { turn: TurnId(1), history: vec![ChatMessage { role: Role::User, content: "héllo".into() }], nudge: None });
        u.on_input(&Input::AgentDelta { turn: TurnId(1), delta: "hi there".into() }, 1500.0);
        u.on_command(&Command::Synthesize { seg: SegmentId(1), text: "hi there".into(), priority: false, timeout_ms: 1.0 });
        u.on_input(&Input::Hangup, 61_000.0);
        assert_eq!(u.call_ms, 60_000.0);
        assert_eq!((u.stt_calls, u.stt_audio_ms), (1, 1000.0));
        assert_eq!((u.agent_calls, u.agent_prompt_chars, u.agent_output_chars), (1, 5, 8));
        assert_eq!((u.tts_calls, u.tts_chars), (1, 8));
        assert!(u.to_markdown().contains("call: 1.0 min"));
    }
}
