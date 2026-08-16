//! The call state machine, sans-IO. Port of voice/src/core/call-session.ts + tts-queue.ts.
//!
//! The runtime (CLI, GPUI app, MCP server) feeds [`Input`]s with the current time and executes
//! the returned [`Command`]s: start/cancel STT, agent, TTS synthesis, turn judge and interjection
//! decisions; pause/resume/clear the ordered playback sink; set/cancel timers. Results come back
//! as further `Input`s. Nothing in here touches a clock, thread or socket.
//!
//! Turn-taking (unchanged from the web version):
//! - The user's utterances are transcribed and accumulated; a heuristic + optional LLM judge
//!   decides whether they've finished or are mid-thought. Mid-thought → hold (`Holding`) until they
//!   continue or `hold_ms` of silence passes.
//! - Output mutes the moment the user *might* be speaking. Speech sustained past `commit_ms`
//!   while muted is a definite interruption — the assistant is cut without waiting for STT.
//!   Shorter speech is transcribed and classified: echo/backchannel → resume; otherwise the
//!   interjection handler decides to ignore, react briefly then resume, or stop and yield.
//! - Utterance results are processed strictly in the order they were spoken.

mod tts_queue;
mod types;

pub use types::*;

use crate::echo_filter::{classify_interruption, is_stop_command, InterruptionKind};
use crate::segmenter::{strip_markdown, SentenceSegmenter};
use crate::turn_heuristics::{heuristic_turn_verdict, TurnVerdict};
use std::collections::VecDeque;
use tts_queue::{SegState, Segment, TtsQueue};

// ---------- internals ----------

struct AssistantTurn {
    id: TurnId,
    turn_index: usize,
    queue: TtsQueue,
    segmenter: SentenceSegmenter,
    /// Remarks made during this turn, for the history entry.
    interjections: Vec<(String, Option<String>)>,
    /// An interjection decision in flight.
    busy: bool,
    /// The remark being decided on (needed once the decision arrives).
    pending_interjection_text: String,
}

#[derive(Debug, Clone)]
struct Interruption {
    gen: u64,
    /// What the speaker was saying when the user started talking.
    horizon: String,
}

struct PendingUser {
    /// Fragments joined — everything the user has said in this turn so far.
    text: String,
    hold: Option<TimerId>,
    judge: Option<ReqId>,
}

struct SttItem {
    req: ReqId,
    interruption: Option<Interruption>,
    outcome: Option<Outcome<String>>,
}

/// Why the in-order utterance chain is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainWait {
    Decide { req: ReqId, turn: TurnId },
    Reaction { seg: SegmentId, turn: TurnId },
}

pub struct CallMachine {
    cfg: CallConfig,
    active: bool,
    history: Vec<ChatMessage>,
    turns: Vec<Turn>,
    next_id: u64,
    user_speaking: bool,
    stt_queue: VecDeque<SttItem>,
    current: Option<AssistantTurn>,
    interruption: Option<Interruption>,
    interruption_gen: u64,
    tentative_mute: bool,
    commit_timer: Option<TimerId>,
    pending: Option<PendingUser>,
    chain_wait: Option<ChainWait>,
    error: Option<String>,
    /// Time of the input being handled (ms).
    now: f64,
}

impl CallMachine {
    pub fn new(cfg: CallConfig) -> Self {
        Self {
            cfg,
            active: false,
            history: vec![],
            turns: vec![],
            next_id: 1,
            user_speaking: false,
            stt_queue: VecDeque::new(),
            current: None,
            interruption: None,
            interruption_gen: 0,
            tentative_mute: false,
            commit_timer: None,
            pending: None,
            chain_wait: None,
            error: None,
            now: 0.0,
        }
    }

    pub fn state(&self) -> CallState {
        CallState { active: self.active, status: self.status(), turns: self.turns.clone(), error: self.error.clone() }
    }
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }
    pub fn history(&self) -> &[ChatMessage] {
        &self.history
    }
    pub fn status(&self) -> CallStatus {
        if !self.active {
            return CallStatus::Idle;
        }
        if self.interruption.is_some() || self.current.as_ref().is_some_and(|c| c.busy) {
            return CallStatus::Interrupted;
        }
        if self.user_speaking {
            return CallStatus::UserSpeaking;
        }
        if !self.stt_queue.is_empty() {
            return CallStatus::Transcribing;
        }
        if let Some(cur) = &self.current {
            return if cur.queue.spoken_text().is_empty() { CallStatus::Thinking } else { CallStatus::Speaking };
        }
        if let Some(p) = &self.pending {
            return if p.judge.is_some() { CallStatus::Transcribing } else { CallStatus::Holding };
        }
        CallStatus::Listening
    }

    fn fresh(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Feed one input at time `now` (ms); returns the commands to execute, in order.
    pub fn handle(&mut self, input: Input, now: f64) -> Vec<Command> {
        let mut out = vec![];
        self.now = now;
        match input {
            Input::Start => {
                if !self.active {
                    self.active = true;
                    self.error = None;
                }
            }
            Input::Hangup => self.hangup(&mut out),
            Input::Interrupt => self.interrupt(&mut out),
            Input::SpeechStart => self.on_speech_start(&mut out),
            Input::SpeechRealStart => self.on_speech_real_start(now, &mut out),
            Input::SpeechMisfire => self.on_speech_misfire(&mut out),
            Input::SpeechEnd { audio, sample_rate } => self.on_speech_end(audio, sample_rate, &mut out),
            Input::SttResult { req, outcome } => {
                if let Some(item) = self.stt_queue.iter_mut().find(|i| i.req == req) {
                    item.outcome = Some(outcome);
                    self.drain(now, &mut out);
                }
            }
            Input::Proactive { instruction } => {
                let quiet = self.active && self.current.is_none() && self.pending.is_none() && self.stt_queue.is_empty() && !self.user_speaking && self.interruption.is_none();
                if quiet {
                    self.start_assistant_turn(Some(instruction), &mut out);
                }
            }
            Input::AgentDelta { turn, delta } => self.on_agent_delta(turn, &delta, &mut out),
            Input::AgentFinished { turn, error } => self.on_agent_finished(turn, error, &mut out),
            Input::SynthFinished { seg, error } => self.on_synth_finished(seg, error, now, &mut out),
            Input::SegmentStarted { seg } => self.on_segment_started(seg, &mut out),
            Input::SegmentEnded { seg } => self.on_segment_ended(seg, now, &mut out),
            Input::JudgeResult { req, outcome } => self.on_judge_result(req, outcome, now, &mut out),
            Input::DecisionResult { req, outcome } => self.on_decision(req, outcome, now, &mut out),
            Input::Timer { id } => self.on_timer(id, now, &mut out),
        }
        out
    }

    // ---------- lifecycle ----------

    fn hangup(&mut self, out: &mut Vec<Command>) {
        if !self.active {
            return;
        }
        self.active = false;
        for item in self.stt_queue.drain(..) {
            out.push(Command::CancelTranscribe { req: item.req });
        }
        self.user_speaking = false;
        self.interruption = None;
        self.tentative_mute = false;
        self.cancel_commit(out);
        self.clear_pending(true, out);
        if self.current.is_some() {
            self.abort_current(false, out);
        }
        out.push(Command::SinkClear);
    }

    fn interrupt(&mut self, out: &mut Vec<Command>) {
        if !self.active {
            return;
        }
        self.cancel_commit(out);
        if self.current.is_some() {
            self.abort_current(true, out);
        }
        self.interruption = None;
        self.tentative_mute = false;
    }

    // ---------- VAD ----------

    fn on_speech_start(&mut self, out: &mut Vec<Command>) {
        if !self.active {
            return;
        }
        self.user_speaking = true;
        // They're continuing — don't answer mid-thought.
        if let Some(p) = &mut self.pending {
            if let Some(h) = p.hold.take() {
                out.push(Command::CancelTimer { id: h });
            }
        }
        let cur_free = self.current.as_ref().is_some_and(|c| !c.busy);
        if self.cfg.mute_on_speech_start && cur_free && self.interruption.is_none() {
            out.push(Command::SinkPause);
            self.tentative_mute = true;
        }
    }

    fn on_speech_real_start(&mut self, now: f64, out: &mut Vec<Command>) {
        if !self.active {
            return;
        }
        self.user_speaking = true;
        let horizon = match &self.current {
            Some(c) if !c.busy && self.interruption.is_none() => Some(c.queue.echo_horizon_text(now, self.cfg.echo_horizon_ms)),
            _ => None,
        };
        if let Some(horizon) = horizon {
            self.interruption_gen += 1;
            self.interruption = Some(Interruption { gen: self.interruption_gen, horizon });
            out.push(Command::SinkPause);
            self.tentative_mute = false;
            // Output is muted now, so anything still being said past commit_ms is the user, not
            // echo: cut the assistant without waiting for a transcript.
            self.cancel_commit(out);
            let id = TimerId(self.fresh());
            self.commit_timer = Some(id);
            out.push(Command::SetTimer { id, ms: self.cfg.commit_ms });
        }
    }

    fn on_speech_misfire(&mut self, out: &mut Vec<Command>) {
        if !self.active {
            return;
        }
        self.user_speaking = false;
        self.cancel_commit(out);
        if self.interruption.is_some() {
            if self.stt_queue.is_empty() {
                self.resume_output(out);
            }
        } else if self.tentative_mute {
            self.tentative_mute = false;
            if self.current.is_some() {
                out.push(Command::SinkResume);
            }
        }
        self.rearm_hold_if_idle(out);
    }

    fn on_speech_end(&mut self, audio: Vec<f32>, sample_rate: u32, out: &mut Vec<Command>) {
        if !self.active {
            return;
        }
        self.user_speaking = false;
        self.cancel_commit(out);
        self.tentative_mute = false;
        let req = ReqId(self.fresh());
        self.stt_queue.push_back(SttItem { req, interruption: self.interruption.clone(), outcome: None });
        out.push(Command::Transcribe { req, audio, sample_rate, timeout_ms: self.cfg.stt_timeout_ms });
    }

    // ---------- utterance chain ----------

    /// Process resolved transcriptions strictly in speaking order, stopping while an
    /// interjection decision / reaction is in flight.
    fn drain(&mut self, now: f64, out: &mut Vec<Command>) {
        while self.chain_wait.is_none() && self.active {
            let ready = self.stt_queue.front().is_some_and(|i| i.outcome.is_some());
            if !ready {
                break;
            }
            let item = self.stt_queue.pop_front().unwrap();
            match item.outcome.unwrap() {
                Outcome::Ok(text) => self.handle_utterance(&text, item.interruption, now, out),
                other => {
                    if let Outcome::Failed(msg) = other {
                        self.error = Some(format!("STT failed: {msg}"));
                    }
                    if self.same_interruption(&item.interruption) {
                        self.resume_output(out);
                    }
                    self.rearm_hold_if_idle(out);
                }
            }
        }
    }

    fn same_interruption(&self, i: &Option<Interruption>) -> bool {
        matches!((i, &self.interruption), (Some(a), Some(b)) if a.gen == b.gen)
    }

    fn handle_utterance(&mut self, raw: &str, interruption: Option<Interruption>, now: f64, out: &mut Vec<Command>) {
        let text = raw.trim().to_string();
        if let Some(cur) = &self.current {
            // Speech arrived while responding: echo, a remark, or a real interruption?
            let horizon = if self.same_interruption(&interruption) {
                interruption.unwrap().horizon
            } else {
                cur.queue.echo_horizon_text(now, self.cfg.echo_horizon_ms)
            };
            let kind = classify_interruption(&text, &horizon, self.cfg.echo_threshold);
            if kind != InterruptionKind::Speech {
                if self.interruption.is_some() {
                    self.resume_output(out);
                }
                return;
            }
            if !self.cfg.has_interjection_handler || is_stop_command(&text) {
                self.stop_and_take(&text, out);
                return;
            }
            let req = ReqId(self.fresh());
            let cur = self.current.as_mut().unwrap();
            cur.busy = true;
            cur.pending_interjection_text = text.clone();
            let turn = cur.id;
            out.push(Command::Decide {
                req,
                history: self.history.clone(),
                spoken_so_far: cur.queue.spoken_text(),
                playing: horizon,
                interjection: text,
            });
            self.chain_wait = Some(ChainWait::Decide { req, turn });
            return;
        }
        // Listening / holding.
        self.interruption = None;
        if text.is_empty() {
            self.rearm_hold_if_idle(out);
            return;
        }
        self.accept_fragment(&text, out);
    }

    fn on_decision(&mut self, req: ReqId, outcome: Outcome<InterjectionDecision>, now: f64, out: &mut Vec<Command>) {
        let Some(ChainWait::Decide { req: waiting, turn }) = self.chain_wait else { return };
        if waiting != req {
            return;
        }
        self.chain_wait = None;
        let Some(cur) = self.current.as_mut() else {
            self.drain(now, out);
            return;
        };
        if cur.id != turn {
            self.drain(now, out);
            return;
        }
        cur.busy = false;
        let decision = match outcome {
            Outcome::Ok(d) => d,
            Outcome::Aborted => InterjectionDecision::Ignore,
            Outcome::Failed(msg) => {
                self.error = Some(format!("Interjection handler failed: {msg}"));
                InterjectionDecision::Ignore
            }
        };
        // Recover the interjection text from the Decide command we issued: it is the last user
        // utterance handled, which we stash on the turn while waiting.
        let text = std::mem::take(&mut cur.pending_interjection_text);
        match decision {
            InterjectionDecision::Ignore => {
                self.interruption = None;
                out.push(Command::SinkResume);
            }
            InterjectionDecision::React { reaction } => {
                let reaction = reaction.trim().to_string();
                self.interruption = None;
                let cur = self.current.as_mut().unwrap();
                cur.interjections.push((text.clone(), if reaction.is_empty() { None } else { Some(reaction.clone()) }));
                let uid = TurnId(self.fresh());
                self.turns.push(Turn { id: uid, role: Role::User, text, at: now, interrupted: false, kind: Some(TurnKind::Interjection), is_final: true });
                if reaction.is_empty() {
                    out.push(Command::SinkResume);
                } else {
                    let rid = TurnId(self.fresh());
                    self.turns.push(Turn { id: rid, role: Role::Assistant, text: reaction.clone(), at: now, interrupted: false, kind: Some(TurnKind::Reaction), is_final: true });
                    // Speak it right now, ahead of whatever is paused; the sink resumes once the
                    // reaction has been synthesized, and the chain waits until it has played.
                    let seg = SegmentId(self.fresh());
                    let cur = self.current.as_mut().unwrap();
                    cur.queue.interjections.push(seg);
                    out.push(Command::Synthesize { seg, text: reaction, priority: true, timeout_ms: self.cfg.synth_timeout_ms });
                    self.chain_wait = Some(ChainWait::Reaction { seg, turn });
                }
            }
            InterjectionDecision::Stop => self.stop_and_take(&text, out),
        }
        self.drain(now, out);
    }

    /// The user cut the assistant off for real: keep what was heard, take their words as a new turn.
    fn stop_and_take(&mut self, text: &str, out: &mut Vec<Command>) {
        self.abort_current(true, out);
        self.interruption = None;
        self.accept_fragment(text, out);
    }

    /// Add a fragment to the user's turn-in-progress and decide whether they're done.
    fn accept_fragment(&mut self, text: &str, out: &mut Vec<Command>) {
        match &mut self.pending {
            Some(p) => {
                if let Some(h) = p.hold.take() {
                    out.push(Command::CancelTimer { id: h });
                }
                if let Some(j) = p.judge.take() {
                    out.push(Command::CancelJudge { req: j });
                }
                p.text = format!("{} {}", p.text, text).trim().to_string();
            }
            None => self.pending = Some(PendingUser { text: text.to_string(), hold: None, judge: None }),
        }
        let utterance = self.pending.as_ref().unwrap().text.clone();
        let heuristic = heuristic_turn_verdict(&utterance);
        if heuristic.is_some() || !self.cfg.has_turn_detector {
            self.apply_verdict(heuristic.unwrap_or(TurnVerdict::Complete), out);
            return;
        }
        let req = ReqId(self.fresh());
        self.pending.as_mut().unwrap().judge = Some(req);
        out.push(Command::Judge { req, history: self.history.clone(), utterance });
    }

    fn on_judge_result(&mut self, req: ReqId, outcome: Outcome<TurnVerdict>, _now: f64, out: &mut Vec<Command>) {
        let Some(p) = &mut self.pending else { return };
        if p.judge != Some(req) {
            return;
        }
        p.judge = None;
        let verdict = match outcome {
            Outcome::Ok(v) => v,
            Outcome::Aborted => return,
            Outcome::Failed(msg) => {
                self.error = Some(format!("Turn detector failed: {msg}"));
                TurnVerdict::Complete
            }
        };
        self.apply_verdict(verdict, out);
    }

    fn apply_verdict(&mut self, verdict: TurnVerdict, out: &mut Vec<Command>) {
        if self.pending.is_none() {
            return;
        }
        match verdict {
            TurnVerdict::Complete => self.respond(out),
            TurnVerdict::Incomplete => self.arm_hold(out),
        }
    }

    /// Wait for the user to continue; answer anyway after `hold_ms` of silence.
    fn arm_hold(&mut self, out: &mut Vec<Command>) {
        let id = TimerId(self.fresh());
        let Some(p) = &mut self.pending else { return };
        if let Some(h) = p.hold.take() {
            out.push(Command::CancelTimer { id: h });
        }
        p.hold = Some(id);
        out.push(Command::SetTimer { id, ms: self.cfg.hold_ms });
    }

    fn rearm_hold_if_idle(&mut self, out: &mut Vec<Command>) {
        if self.pending.as_ref().is_some_and(|p| p.hold.is_none() && p.judge.is_none()) {
            self.arm_hold(out);
        }
    }

    fn on_timer(&mut self, id: TimerId, now: f64, out: &mut Vec<Command>) {
        if self.commit_timer == Some(id) {
            self.commit_timer = None;
            if !self.user_speaking || self.interruption.is_none() || self.current.is_none() {
                return;
            }
            self.abort_current(true, out);
            self.interruption = None;
            return;
        }
        let is_hold = self.pending.as_ref().is_some_and(|p| p.hold == Some(id));
        if is_hold {
            self.pending.as_mut().unwrap().hold = None;
            // Still busy with them? Check again shortly.
            let judging = self.pending.as_ref().unwrap().judge.is_some();
            if self.user_speaking || !self.stt_queue.is_empty() || judging {
                self.arm_hold(out);
                return;
            }
            self.respond(out);
            let _ = now;
        }
    }

    fn respond(&mut self, out: &mut Vec<Command>) {
        let Some(p) = self.pending.take() else { return };
        if let Some(h) = p.hold {
            out.push(Command::CancelTimer { id: h });
        }
        // (judge is None here by construction; a live judge would have been the caller.)
        self.history.push(ChatMessage { role: Role::User, content: p.text.clone() });
        let id = TurnId(self.fresh());
        self.turns.push(Turn { id, role: Role::User, text: p.text, at: self.now, interrupted: false, kind: None, is_final: true });
        self.start_assistant_turn(None, out);
    }

    fn clear_pending(&mut self, abort_judge: bool, out: &mut Vec<Command>) {
        let Some(p) = self.pending.take() else { return };
        if let Some(h) = p.hold {
            out.push(Command::CancelTimer { id: h });
        }
        if abort_judge {
            if let Some(j) = p.judge {
                out.push(Command::CancelJudge { req: j });
            }
        }
    }

    fn cancel_commit(&mut self, out: &mut Vec<Command>) {
        if let Some(t) = self.commit_timer.take() {
            out.push(Command::CancelTimer { id: t });
        }
    }

    fn resume_output(&mut self, out: &mut Vec<Command>) {
        self.interruption = None;
        self.tentative_mute = false;
        if self.current.is_some() {
            out.push(Command::SinkResume);
        }
    }

    // ---------- assistant turn ----------

    fn start_assistant_turn(&mut self, nudge: Option<String>, out: &mut Vec<Command>) {
        let id = TurnId(self.fresh());
        self.turns.push(Turn { id, role: Role::Assistant, text: String::new(), at: self.now, interrupted: false, kind: None, is_final: false });
        let turn_index = self.turns.len() - 1;
        self.current = Some(AssistantTurn {
            id,
            turn_index,
            queue: TtsQueue::new(),
            segmenter: SentenceSegmenter::new(self.cfg.segmenter.clone()),
            interjections: vec![],
            busy: false,
            pending_interjection_text: String::new(),
        });
        out.push(Command::RunAgent { turn: id, history: self.history.clone(), nudge });
    }

    fn on_agent_delta(&mut self, turn: TurnId, delta: &str, out: &mut Vec<Command>) {
        let Some(cur) = self.current.as_mut() else { return };
        if cur.id != turn {
            return;
        }
        self.turns[cur.turn_index].text.push_str(delta);
        let sentences = cur.segmenter.push(delta);
        self.speak(sentences, out);
    }

    fn on_agent_finished(&mut self, turn: TurnId, error: Option<String>, out: &mut Vec<Command>) {
        let Some(cur) = self.current.as_mut() else { return };
        if cur.id != turn {
            return;
        }
        if let Some(msg) = error {
            self.error = Some(format!("Agent failed: {msg}"));
        } else {
            let rest = cur.segmenter.flush();
            self.speak(rest, out);
        }
        let cur = self.current.as_mut().unwrap();
        cur.queue.finished = true;
        self.maybe_drained(out);
    }

    fn speak(&mut self, sentences: Vec<String>, out: &mut Vec<Command>) {
        let cur = self.current.as_mut().unwrap();
        for s in sentences {
            let clean = strip_markdown(&s).trim().to_string();
            if !clean.is_empty() {
                let id = SegmentId(self.next_id);
                self.next_id += 1;
                cur.queue.segments.push(Segment { id, text: clean, state: SegState::Pending, ended_at: None });
            }
        }
        self.pump(out);
    }

    fn pump(&mut self, out: &mut Vec<Command>) {
        let Some(cur) = self.current.as_mut() else { return };
        for (seg, text) in cur.queue.pump(self.cfg.lookahead) {
            out.push(Command::Synthesize { seg, text, priority: false, timeout_ms: self.cfg.synth_timeout_ms });
        }
    }

    fn on_synth_finished(&mut self, seg: SegmentId, error: Option<String>, now: f64, out: &mut Vec<Command>) {
        let Some(cur) = self.current.as_mut() else { return };
        if cur.queue.interjections.contains(&seg) {
            cur.queue.interjections.retain(|s| *s != seg);
            if let Some(msg) = error {
                self.error = Some(format!("TTS failed: {msg}"));
            }
            // Hand playback back to the main sequence.
            out.push(Command::SinkResume);
            let _ = now;
            return;
        }
        let Some(s) = cur.queue.segments.iter_mut().find(|s| s.id == seg) else { return };
        if s.state == SegState::Synthesizing {
            s.state = SegState::Ready;
        }
        if let Some(msg) = error {
            // Skip this sentence, keep the conversation going.
            self.error = Some(format!("TTS failed: {msg}"));
        }
        self.pump(out);
    }

    fn on_segment_started(&mut self, seg: SegmentId, out: &mut Vec<Command>) {
        let Some(cur) = self.current.as_mut() else { return };
        let Some(s) = cur.queue.segments.iter_mut().find(|s| s.id == seg) else { return };
        s.state = SegState::Playing;
        self.pump(out);
    }

    fn on_segment_ended(&mut self, seg: SegmentId, now: f64, out: &mut Vec<Command>) {
        if let Some(ChainWait::Reaction { seg: waiting, .. }) = self.chain_wait {
            if waiting == seg {
                self.chain_wait = None;
                self.drain(now, out);
                return;
            }
        }
        let Some(cur) = self.current.as_mut() else { return };
        let Some(s) = cur.queue.segments.iter_mut().find(|s| s.id == seg) else { return };
        s.state = SegState::Done;
        s.ended_at = Some(now);
        self.pump(out);
        self.maybe_drained(out);
    }

    fn maybe_drained(&mut self, _out: &mut Vec<Command>) {
        let Some(cur) = self.current.as_mut() else { return };
        if cur.queue.drained || !cur.queue.finished || !cur.queue.all_done() {
            return;
        }
        cur.queue.drained = true;
        // finishTurn
        let cur = self.current.take().unwrap();
        let text = self.turns[cur.turn_index].text.trim().to_string();
        self.turns[cur.turn_index].is_final = true;
        if text.is_empty() {
            self.turns.remove(cur.turn_index);
        } else {
            self.history.push(ChatMessage { role: Role::Assistant, content: history_content(&text, &cur.interjections, false) });
        }
    }

    fn abort_current(&mut self, interrupted: bool, out: &mut Vec<Command>) {
        let Some(cur) = self.current.take() else { return };
        self.cancel_commit(out);
        // Drop any chain wait tied to this turn.
        match self.chain_wait {
            Some(ChainWait::Decide { req, turn }) if turn == cur.id => {
                out.push(Command::CancelDecide { req });
                self.chain_wait = None;
            }
            Some(ChainWait::Reaction { turn, .. }) if turn == cur.id => self.chain_wait = None,
            _ => {}
        }
        out.push(Command::CancelAgent { turn: cur.id });
        let spoken = cur.queue.spoken_text();
        for s in cur.queue.segments.iter().filter(|s| s.state == SegState::Synthesizing) {
            out.push(Command::CancelSynthesize { seg: s.id });
        }
        for seg in &cur.queue.interjections {
            out.push(Command::CancelSynthesize { seg: *seg });
        }
        out.push(Command::SinkClear);
        if !spoken.is_empty() {
            let t = &mut self.turns[cur.turn_index];
            t.text = spoken.clone();
            t.is_final = true;
            if interrupted {
                t.interrupted = true;
            }
            self.history.push(ChatMessage { role: Role::Assistant, content: history_content(&spoken, &cur.interjections, interrupted) });
        } else {
            // Nothing was heard → the turn never happened (interjection turns, if any, stay).
            self.turns.remove(cur.turn_index);
        }
    }
}

fn history_content(text: &str, interjections: &[(String, Option<String>)], interrupted: bool) -> String {
    let mut out = text.to_string();
    for (t, r) in interjections {
        out.push_str(&format!("\n[While you were speaking the user said: \"{t}\""));
        match r {
            Some(r) => out.push_str(&format!(" — you replied: \"{r}\"]")),
            None => out.push(']'),
        }
    }
    if interrupted {
        out.push_str("\n[interrupted by user]");
    }
    out
}
