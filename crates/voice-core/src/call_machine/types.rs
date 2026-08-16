//! Public vocabulary of the call machine: ids, state snapshots, [`Input`]s, [`Command`]s and
//! [`CallConfig`].

use crate::echo_filter::DEFAULT_ECHO_THRESHOLD;
use crate::segmenter::SegmenterOptions;
use crate::turn_heuristics::TurnVerdict;
use serde::{Deserialize, Serialize};

// ---------- ids ----------

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub u64);
    };
}
id_type!(TurnId);
id_type!(SegmentId);
id_type!(ReqId);
id_type!(TimerId);

// ---------- public data ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallStatus {
    Idle,
    Listening,
    UserSpeaking,
    Transcribing,
    /// User paused but seems mid-thought — waiting for them to continue.
    Holding,
    Thinking,
    Speaking,
    /// Output paused; deciding whether the user's speech was echo, a remark, or a real interruption.
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TurnKind {
    /// A remark the user made while the assistant was speaking.
    Interjection,
    /// The assistant's brief reaction to an interjection.
    Reaction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    pub role: Role,
    pub text: String,
    pub at: f64,
    /// Assistant turn cut short by the user.
    pub interrupted: bool,
    pub kind: Option<TurnKind>,
    /// False while an assistant turn is still streaming.
    pub is_final: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallState {
    pub active: bool,
    pub status: CallStatus,
    pub turns: Vec<Turn>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InterjectionDecision {
    Ignore,
    React { reaction: String },
    Stop,
}

/// Outcome of an external request. `Aborted` means we cancelled it (or it timed out): stay quiet.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T> {
    Ok(T),
    Aborted,
    Failed(String),
}

// ---------- inputs & commands ----------

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Start,
    Hangup,
    /// Hard interruption from outside the VAD path (hotkey / button / media-mode speaker gate).
    Interrupt,
    /// VAD: speech may have started (not yet confirmed).
    SpeechStart,
    /// VAD: speech sustained long enough to be real → the assistant yields.
    SpeechRealStart,
    /// VAD: brief noise, not speech.
    SpeechMisfire,
    /// VAD: an utterance finished; mono float PCM at `sample_rate`.
    SpeechEnd { audio: Vec<f32>, sample_rate: u32 },
    SttResult { req: ReqId, outcome: Outcome<String> },
    /// Ask the assistant to speak up on its own (greeting, idle nudge). Ignored unless the call
    /// is quietly listening: no assistant turn, no pending user text, nobody speaking.
    Proactive { instruction: String },
    AgentDelta { turn: TurnId, delta: String },
    /// Agent stream ended. `error` = Some when it failed (not when we cancelled it).
    AgentFinished { turn: TurnId, error: Option<String> },
    /// TTS synthesis for a segment ended (all audio has been handed to the sink).
    SynthFinished { seg: SegmentId, error: Option<String> },
    /// The sink started playing a segment.
    SegmentStarted { seg: SegmentId },
    /// The sink finished playing a segment.
    SegmentEnded { seg: SegmentId },
    JudgeResult { req: ReqId, outcome: Outcome<TurnVerdict> },
    DecisionResult { req: ReqId, outcome: Outcome<InterjectionDecision> },
    Timer { id: TimerId },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Transcribe { req: ReqId, audio: Vec<f32>, sample_rate: u32, timeout_ms: f64 },
    CancelTranscribe { req: ReqId },
    /// `nudge` = a one-off instruction for a proactive turn (not part of the history).
    RunAgent { turn: TurnId, history: Vec<ChatMessage>, nudge: Option<String> },
    CancelAgent { turn: TurnId },
    /// Declare a playback segment (in play order; `priority` puts it in front of the paused/current
    /// one), synthesize `text`, stream PCM into the sink for `seg`, then end the segment. Report
    /// completion with `Input::SynthFinished` even on error/timeout.
    Synthesize { seg: SegmentId, text: String, priority: bool, timeout_ms: f64 },
    CancelSynthesize { seg: SegmentId },
    SinkPause,
    SinkResume,
    /// Drop everything, including the segment currently playing, and un-pause.
    SinkClear,
    Judge { req: ReqId, history: Vec<ChatMessage>, utterance: String },
    CancelJudge { req: ReqId },
    Decide { req: ReqId, history: Vec<ChatMessage>, spoken_so_far: String, playing: String, interjection: String },
    CancelDecide { req: ReqId },
    SetTimer { id: TimerId, ms: f64 },
    CancelTimer { id: TimerId },
}

#[derive(Debug, Clone)]
pub struct CallConfig {
    /// After an "incomplete" verdict, respond anyway once the user has been silent this long.
    pub hold_ms: f64,
    /// Speech sustained this long while output is muted = definite interruption (no STT needed).
    pub commit_ms: f64,
    /// Mute output as soon as speech *might* have started (before it's confirmed).
    pub mute_on_speech_start: bool,
    pub stt_timeout_ms: f64,
    pub synth_timeout_ms: f64,
    /// How many segments may be synthesizing beyond the one being played.
    pub lookahead: usize,
    /// How far back finished sentences still count as "what we were saying".
    pub echo_horizon_ms: f64,
    pub echo_threshold: f64,
    /// Whether the runtime provides a semantic end-of-turn judge (`Command::Judge`).
    pub has_turn_detector: bool,
    /// Whether the runtime provides an interjection handler (`Command::Decide`).
    pub has_interjection_handler: bool,
    pub segmenter: SegmenterOptions,
}

impl Default for CallConfig {
    fn default() -> Self {
        Self {
            hold_ms: 6_000.0,
            commit_ms: 1_200.0,
            mute_on_speech_start: true,
            stt_timeout_ms: 45_000.0,
            synth_timeout_ms: 30_000.0,
            lookahead: 1,
            echo_horizon_ms: 2_000.0,
            echo_threshold: DEFAULT_ECHO_THRESHOLD,
            has_turn_detector: false,
            has_interjection_handler: false,
            segmenter: SegmenterOptions::default(),
        }
    }
}
