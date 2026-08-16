//! Port of voice/test/call-session.test.ts against the sans-IO CallMachine. A tiny fake runtime
//! executes commands: TTS synth completes instantly, the sink is stepped by hand, timers are
//! recorded and fired by hand.
use std::collections::HashMap;
use voice_core::call_machine::*;
use voice_core::turn_heuristics::TurnVerdict;

struct H {
    m: CallMachine,
    now: f64,
    stt: Vec<ReqId>,
    stt_cancelled: Vec<ReqId>,
    agents: Vec<(TurnId, Vec<ChatMessage>)>,
    agents_cancelled: Vec<TurnId>,
    synth: Vec<(SegmentId, String, bool)>,
    /// Sink: play order (priority segments go in front), what has started / ended.
    order: Vec<SegmentId>,
    started: Vec<SegmentId>,
    ended: Vec<SegmentId>,
    paused: bool,
    cleared: usize,
    timers: HashMap<TimerId, f64>,
    judges: Vec<(ReqId, String)>,
    judges_cancelled: Vec<ReqId>,
    decides: Vec<(ReqId, String)>,
}

impl H {
    fn new(turn_detector: bool, interjection: bool) -> H {
        let cfg = CallConfig { has_turn_detector: turn_detector, has_interjection_handler: interjection, ..Default::default() };
        let mut h = H {
            m: CallMachine::new(cfg),
            now: 0.0,
            stt: vec![],
            stt_cancelled: vec![],
            agents: vec![],
            agents_cancelled: vec![],
            synth: vec![],
            order: vec![],
            started: vec![],
            ended: vec![],
            paused: false,
            cleared: 0,
            timers: HashMap::new(),
            judges: vec![],
            judges_cancelled: vec![],
            decides: vec![],
        };
        h.step(Input::Start);
        h
    }
    fn step(&mut self, input: Input) {
        let cmds = self.m.handle(input, self.now);
        for c in cmds {
            self.exec(c);
        }
    }
    fn exec(&mut self, c: Command) {
        match c {
            Command::Transcribe { req, .. } => self.stt.push(req),
            Command::CancelTranscribe { req } => self.stt_cancelled.push(req),
            Command::RunAgent { turn, history, .. } => self.agents.push((turn, history)),
            Command::CancelAgent { turn } => self.agents_cancelled.push(turn),
            Command::Synthesize { seg, text, priority, .. } => {
                self.synth.push((seg, text, priority));
                if priority {
                    self.order.insert(0, seg);
                } else {
                    self.order.push(seg);
                }
                // auto TTS: synth completes instantly
                self.step(Input::SynthFinished { seg, error: None });
            }
            Command::CancelSynthesize { .. } => {}
            Command::SinkPause => self.paused = true,
            Command::SinkResume => self.paused = false,
            Command::SinkClear => {
                self.cleared += 1;
                self.paused = false;
                self.order.clear();
            }
            Command::Judge { req, utterance, .. } => self.judges.push((req, utterance)),
            Command::CancelJudge { req } => self.judges_cancelled.push(req),
            Command::Decide { req, interjection, .. } => self.decides.push((req, interjection)),
            Command::CancelDecide { .. } => {}
            Command::SetTimer { id, ms } => {
                self.timers.insert(id, ms);
            }
            Command::CancelTimer { id } => {
                self.timers.remove(&id);
            }
        }
    }
    fn status(&self) -> CallStatus {
        self.m.status()
    }
    fn speak(&mut self) {
        self.step(Input::SpeechStart);
        self.step(Input::SpeechRealStart);
        self.step(Input::SpeechEnd { audio: vec![0.0; 16000], sample_rate: 16000 });
    }
    /// Answer the oldest unanswered STT request (like FakeStt.answer).
    fn stt_answer_oldest(&mut self, text: &str) {
        let req = self.stt.remove(0);
        self.step(Input::SttResult { req, outcome: Outcome::Ok(text.to_string()) });
    }
    fn last_agent(&self) -> TurnId {
        self.agents.last().unwrap().0
    }
    fn agent_push(&mut self, delta: &str) {
        let t = self.last_agent();
        self.step(Input::AgentDelta { turn: t, delta: delta.to_string() });
    }
    fn agent_done(&mut self) {
        let t = self.last_agent();
        self.step(Input::AgentFinished { turn: t, error: None });
    }
    /// sink: start the next not-yet-started segment in order
    fn start_next(&mut self) {
        let seg = *self.order.iter().find(|s| !self.started.contains(s)).expect("nothing to start");
        self.started.push(seg);
        self.step(Input::SegmentStarted { seg });
    }
    fn finish_current(&mut self) {
        let seg = *self.started.iter().find(|s| !self.ended.contains(s)).expect("nothing playing");
        self.ended.push(seg);
        self.step(Input::SegmentEnded { seg });
    }
    fn play_all_ended(&mut self) {
        loop {
            let next = self.order.iter().find(|s| !self.started.contains(s)).copied();
            match next {
                Some(_) => {
                    self.start_next();
                    self.finish_current();
                }
                None => break,
            }
        }
    }
    fn pending_timers(&self, ms: f64) -> usize {
        self.timers.values().filter(|v| **v == ms).count()
    }
    fn fire(&mut self, ms: f64) {
        let id = *self.timers.iter().find(|(_, v)| **v == ms).unwrap().0;
        self.timers.remove(&id);
        self.step(Input::Timer { id });
    }
    fn agent_aborted(&self, i: usize) -> bool {
        self.agents_cancelled.contains(&self.agents[i].0)
    }
    fn turns_summary(&self) -> Vec<(Role, String, bool)> {
        self.m.turns().iter().map(|t| (t.role.clone(), t.text.clone(), t.is_final)).collect()
    }
}

fn user(s: &str) -> ChatMessage {
    ChatMessage { role: Role::User, content: s.into() }
}
fn assistant(s: &str) -> ChatMessage {
    ChatMessage { role: Role::Assistant, content: s.into() }
}

// ---------- basic turn ----------

#[test]
fn basic_turn_flow() {
    let mut h = H::new(false, false);
    assert_eq!(h.status(), CallStatus::Listening);
    h.step(Input::SpeechStart);
    assert_eq!(h.status(), CallStatus::UserSpeaking);
    h.speak();
    assert_eq!(h.status(), CallStatus::Transcribing);
    h.stt_answer_oldest("你好");
    assert_eq!(h.status(), CallStatus::Thinking);
    assert_eq!(h.agents[0].1, vec![user("你好")]);
    h.agent_push("你好！");
    h.agent_push("很高興認識你。");
    assert_eq!(h.order.len(), 1); // "你好！" too short → merged
    h.agent_done();
    assert_eq!(h.order.len(), 1);
    h.start_next();
    assert_eq!(h.status(), CallStatus::Speaking);
    h.finish_current();
    assert_eq!(h.status(), CallStatus::Listening);
    assert_eq!(
        h.turns_summary(),
        vec![(Role::User, "你好".into(), true), (Role::Assistant, "你好！很高興認識你。".into(), true)]
    );
    h.speak();
    h.stt_answer_oldest("再見");
    assert_eq!(h.agents[1].1, vec![user("你好"), assistant("你好！很高興認識你。"), user("再見")]);
}

#[test]
fn proactive_only_when_quietly_listening() {
    let mut h = H::new(false, false);
    h.step(Input::Proactive { instruction: "greet".into() });
    assert_eq!(h.agents.len(), 1);
    assert!(h.agents[0].1.is_empty()); // no user turn was invented
    assert_eq!(h.status(), CallStatus::Thinking);
    // busy → ignored
    h.step(Input::Proactive { instruction: "again".into() });
    assert_eq!(h.agents.len(), 1);
    h.agent_push("Hi there, how are you today?");
    h.agent_done();
    h.play_all_ended();
    assert_eq!(h.status(), CallStatus::Listening);
    assert_eq!(h.m.history().len(), 1);
    // user speaking → ignored
    h.step(Input::SpeechStart);
    h.step(Input::Proactive { instruction: "x".into() });
    assert_eq!(h.agents.len(), 1);
}

#[test]
fn empty_transcript_ignored() {
    let mut h = H::new(false, false);
    h.speak();
    h.stt_answer_oldest("   ");
    assert_eq!(h.status(), CallStatus::Listening);
    assert!(h.agents.is_empty());
}

#[test]
fn stt_failure_keeps_listening() {
    let mut h = H::new(false, false);
    h.speak();
    let req = h.stt[0];
    h.step(Input::SttResult { req, outcome: Outcome::Failed("401".into()) });
    assert_eq!(h.status(), CallStatus::Listening);
    assert!(h.m.state().error.unwrap().contains("STT failed"));
}

// ---------- barge-in ----------

fn responding_with_speech(interjection: bool) -> H {
    let mut h = H::new(false, interjection);
    h.speak();
    h.stt_answer_oldest("講個故事");
    h.agent_push("從前有一座山。山裡有一座廟。廟裡有個老和尚。");
    h.start_next();
    h.finish_current(); // sentence 1 heard
    h.start_next(); // sentence 2 playing
    h
}

#[test]
fn echo_resumes() {
    let mut h = responding_with_speech(false);
    h.step(Input::SpeechRealStart);
    assert!(h.paused);
    assert_eq!(h.status(), CallStatus::Interrupted);
    h.step(Input::SpeechEnd { audio: vec![0.0; 8000], sample_rate: 16000 });
    h.stt_answer_oldest("山裡有一座廟");
    assert!(!h.paused);
    assert_eq!(h.status(), CallStatus::Speaking);
    assert_eq!(h.agents.len(), 1);
    assert!(!h.agent_aborted(0));
}

#[test]
fn backchannel_and_empty_resume() {
    for t in ["嗯嗯", ""] {
        let mut h = responding_with_speech(false);
        h.step(Input::SpeechRealStart);
        h.step(Input::SpeechEnd { audio: vec![0.0; 8000], sample_rate: 16000 });
        h.stt_answer_oldest(t);
        assert!(!h.paused, "{t:?}");
    }
}

#[test]
fn real_speech_aborts_and_starts_new_turn() {
    let mut h = responding_with_speech(false);
    h.step(Input::SpeechRealStart);
    h.step(Input::SpeechEnd { audio: vec![0.0; 8000], sample_rate: 16000 });
    h.stt_answer_oldest("等一下，我想問別的問題");
    assert!(h.agent_aborted(0));
    assert_eq!(h.cleared, 1);
    assert!(!h.paused);
    assert_eq!(h.agents.len(), 2);
    assert_eq!(
        h.agents[1].1,
        vec![user("講個故事"), assistant("從前有一座山。 山裡有一座廟。\n[interrupted by user]"), user("等一下，我想問別的問題")]
    );
    let t = &h.m.turns()[1];
    assert_eq!((t.role.clone(), t.text.as_str(), t.interrupted, t.is_final), (Role::Assistant, "從前有一座山。 山裡有一座廟。", true, true));
    assert_eq!(h.m.turns()[2].text, "等一下，我想問別的問題");
    assert_eq!(h.status(), CallStatus::Thinking);
}

#[test]
fn interrupt_cuts_at_once_next_utterance_is_new_turn() {
    let mut h = responding_with_speech(false);
    h.step(Input::Interrupt);
    assert!(h.agent_aborted(0));
    assert_eq!(h.cleared, 1);
    assert_eq!(h.status(), CallStatus::Listening);
    let t = &h.m.turns()[1];
    assert!(t.interrupted && t.is_final && t.text == "從前有一座山。 山裡有一座廟。");
    // Media mode: verified audio arrives via SpeechEnd only, no VAD start events.
    h.step(Input::SpeechEnd { audio: vec![0.0; 16000], sample_rate: 16000 });
    h.stt_answer_oldest("換個話題");
    assert_eq!(h.agents.len(), 2);
    assert_eq!(h.agents[1].1.last().unwrap(), &user("換個話題"));
    assert_eq!(h.status(), CallStatus::Thinking);
}

#[test]
fn interrupt_while_listening_is_noop() {
    let mut h = H::new(false, false);
    h.step(Input::Interrupt);
    assert_eq!(h.cleared, 0);
    assert_eq!(h.status(), CallStatus::Listening);
}

#[test]
fn speech_while_thinking_drops_partial_turn_and_chains() {
    let mut h = H::new(false, false);
    h.speak();
    h.stt_answer_oldest("first question");
    h.agent_push("Let me think about");
    h.speak();
    h.stt_answer_oldest("and also a second question");
    assert!(h.agent_aborted(0));
    assert_eq!(h.agents[1].1, vec![user("first question"), user("and also a second question")]);
    let roles: Vec<Role> = h.m.turns().iter().map(|t| t.role.clone()).collect();
    assert_eq!(roles, vec![Role::User, Role::User, Role::Assistant]);
}

#[test]
fn utterances_handled_in_speaking_order() {
    let mut h = H::new(false, false);
    h.speak();
    h.speak();
    let (r0, r1) = (h.stt[0], h.stt[1]);
    h.step(Input::SttResult { req: r1, outcome: Outcome::Ok("second".into()) });
    assert!(h.agents.is_empty());
    h.step(Input::SttResult { req: r0, outcome: Outcome::Ok("first".into()) });
    assert_eq!(h.agents.len(), 2);
    let contents: Vec<&str> = h.agents[1].1.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["first", "second"]);
}

// ---------- hangup ----------

#[test]
fn hangup_aborts_everything() {
    let mut h = H::new(false, false);
    h.speak();
    h.stt_answer_oldest("hi");
    h.agent_push("Hello there, friend. ");
    h.start_next();
    h.speak(); // transcription in flight
    h.step(Input::Hangup);
    assert_eq!(h.status(), CallStatus::Idle);
    assert!(h.agent_aborted(0));
    assert!(h.cleared > 0);
    assert_eq!(h.stt_cancelled.len(), 1);
    // late STT result ignored
    let late = h.stt[0];
    h.step(Input::SttResult { req: late, outcome: Outcome::Ok("late".into()) });
    assert_eq!(h.agents.len(), 1);
    let last = h.m.turns().last().unwrap();
    assert!(last.role == Role::Assistant && last.text == "Hello there, friend." && last.is_final);
}

#[test]
fn agent_error_ends_turn_gracefully() {
    let mut h = H::new(false, false);
    h.speak();
    h.stt_answer_oldest("hi");
    let t = h.last_agent();
    h.step(Input::AgentFinished { turn: t, error: Some("rate limited".into()) });
    assert_eq!(h.status(), CallStatus::Listening);
    assert!(h.m.state().error.unwrap().contains("Agent failed"));
    assert_eq!(h.m.turns().len(), 1);
}

// ---------- semantic turn-taking (hold) ----------

#[test]
fn trailing_conjunction_holds_without_detector() {
    let mut h = H::new(true, false);
    h.speak();
    h.stt_answer_oldest("我覺得這個設計，然後");
    assert!(h.judges.is_empty());
    assert_eq!(h.status(), CallStatus::Holding);
    assert_eq!(h.pending_timers(6000.0), 1);
    assert!(h.agents.is_empty());
    h.speak(); // continuing cancels the hold
    assert_eq!(h.pending_timers(6000.0), 0);
    h.stt_answer_oldest("應該要再簡單一點？");
    assert_eq!(h.agents.len(), 1);
    assert_eq!(h.agents[0].1, vec![user("我覺得這個設計，然後 應該要再簡單一點？")]);
}

#[test]
fn ambiguous_asks_detector_incomplete_holds_then_responds() {
    let mut h = H::new(true, false);
    h.speak();
    h.stt_answer_oldest("我在想那個功能");
    assert_eq!(h.judges[0].1, "我在想那個功能");
    assert_eq!(h.status(), CallStatus::Transcribing);
    let req = h.judges[0].0;
    h.step(Input::JudgeResult { req, outcome: Outcome::Ok(TurnVerdict::Incomplete) });
    assert_eq!(h.status(), CallStatus::Holding);
    h.fire(6000.0);
    assert_eq!(h.agents.len(), 1);
    assert_eq!(h.agents[0].1[0].content, "我在想那個功能");
    assert_eq!(h.status(), CallStatus::Thinking);
}

#[test]
fn detector_complete_responds() {
    let mut h = H::new(true, false);
    h.speak();
    h.stt_answer_oldest("幫我看一下這段程式碼有沒有問題");
    let req = h.judges[0].0;
    h.step(Input::JudgeResult { req, outcome: Outcome::Ok(TurnVerdict::Complete) });
    assert_eq!(h.agents.len(), 1);
}

#[test]
fn new_fragment_while_judging_rejudges_whole_turn() {
    let mut h = H::new(true, false);
    h.speak();
    h.stt_answer_oldest("第一段");
    let first = h.judges[0].0;
    h.speak();
    h.stt_answer_oldest("第二段");
    assert!(h.judges_cancelled.contains(&first));
    assert_eq!(h.judges.last().unwrap().1, "第一段 第二段");
}

#[test]
fn hold_timer_with_transcription_in_flight_rearms() {
    let mut h = H::new(true, false);
    h.speak();
    h.stt_answer_oldest("我在想那個功能");
    let req = h.judges[0].0;
    h.step(Input::JudgeResult { req, outcome: Outcome::Ok(TurnVerdict::Incomplete) });
    h.speak(); // hold cancelled by speech start
    assert_eq!(h.pending_timers(6000.0), 0);
    h.stt_answer_oldest(""); // empty → re-arm
    assert_eq!(h.pending_timers(6000.0), 1);
    assert!(h.agents.is_empty());
}

#[test]
fn detector_failure_falls_back_to_responding() {
    let mut h = H::new(true, false);
    h.speak();
    h.stt_answer_oldest("這樣可以嗎");
    let req = h.judges[0].0;
    h.step(Input::JudgeResult { req, outcome: Outcome::Failed("boom".into()) });
    assert_eq!(h.agents.len(), 1);
    assert!(h.m.state().error.unwrap().contains("Turn detector failed"));
}

// ---------- yielding & interjections ----------

#[test]
fn mutes_on_speech_start_unmutes_on_misfire() {
    let mut h = responding_with_speech(false);
    h.step(Input::SpeechStart);
    assert!(h.paused);
    assert_eq!(h.status(), CallStatus::UserSpeaking);
    h.step(Input::SpeechMisfire);
    assert!(!h.paused);
    assert_eq!(h.status(), CallStatus::Speaking);
}

#[test]
fn sustained_speech_past_commit_cuts_without_stt() {
    let mut h = responding_with_speech(false);
    h.step(Input::SpeechStart);
    h.step(Input::SpeechRealStart);
    assert_eq!(h.pending_timers(1200.0), 1);
    h.fire(1200.0);
    assert!(h.agent_aborted(0));
    assert_eq!(h.cleared, 1);
    assert_eq!(h.status(), CallStatus::UserSpeaking);
    let t = &h.m.turns()[1];
    assert!(t.interrupted && t.text == "從前有一座山。 山裡有一座廟。");
    h.step(Input::SpeechEnd { audio: vec![0.0; 16000], sample_rate: 16000 });
    h.stt_answer_oldest("其實我想聽別的故事，關於海的");
    assert_eq!(h.agents.len(), 2);
    assert_eq!(h.agents[1].1.last().unwrap(), &user("其實我想聽別的故事，關於海的"));
}

#[test]
fn speech_ending_before_commit_cancels_timer() {
    let mut h = responding_with_speech(false);
    h.step(Input::SpeechRealStart);
    h.step(Input::SpeechEnd { audio: vec![0.0; 8000], sample_rate: 16000 });
    assert_eq!(h.pending_timers(1200.0), 0);
    assert!(!h.agent_aborted(0));
}

#[test]
fn remark_ignore_resumes() {
    let mut h = responding_with_speech(true);
    h.step(Input::SpeechRealStart);
    h.step(Input::SpeechEnd { audio: vec![0.0; 8000], sample_rate: 16000 });
    h.stt_answer_oldest("哇這故事好老套");
    assert_eq!(h.decides[0].1, "哇這故事好老套");
    assert_eq!(h.status(), CallStatus::Interrupted);
    let req = h.decides[0].0;
    h.step(Input::DecisionResult { req, outcome: Outcome::Ok(InterjectionDecision::Ignore) });
    assert!(!h.paused);
    assert_eq!(h.status(), CallStatus::Speaking);
}

#[test]
fn remark_react_plays_ahead_then_continues() {
    let mut h = responding_with_speech(true);
    h.step(Input::SpeechRealStart);
    h.step(Input::SpeechEnd { audio: vec![0.0; 8000], sample_rate: 16000 });
    h.stt_answer_oldest("哇這故事好老套");
    let req = h.decides[0].0;
    h.step(Input::DecisionResult { req, outcome: Outcome::Ok(InterjectionDecision::React { reaction: "哈哈，老套但經典啊。".into() }) });
    let (front, text, prio) = h.synth.last().unwrap().clone();
    assert_eq!(text, "哈哈，老套但經典啊。");
    assert!(prio);
    assert_eq!(h.order[0], front);
    assert!(!h.paused); // resumed once the reaction was synthesized
    // play the reaction (preempting the paused sentence 2)
    h.started.push(front);
    h.step(Input::SegmentStarted { seg: front });
    h.ended.push(front);
    h.step(Input::SegmentEnded { seg: front });
    assert!(!h.agent_aborted(0));
    let kinds: Vec<String> = h.m.turns().iter().map(|t| match t.kind {
        Some(TurnKind::Interjection) => "interjection".into(),
        Some(TurnKind::Reaction) => "reaction".into(),
        None => format!("{:?}", t.role).to_lowercase(),
    }).collect();
    assert_eq!(kinds, vec!["user", "assistant", "interjection", "reaction"]);
    // finish the turn: history carries the remark
    h.finish_current(); // sentence 2
    h.agent_done();
    h.play_all_ended();
    h.speak();
    h.stt_answer_oldest("好，繼續？");
    assert!(h.agents[1].1[1].content.contains("[While you were speaking the user said: \"哇這故事好老套\" — you replied: \"哈哈，老套但經典啊。\"]"));
}

#[test]
fn remark_stop_yields_and_starts_new_turn() {
    let mut h = responding_with_speech(true);
    h.step(Input::SpeechRealStart);
    h.step(Input::SpeechEnd { audio: vec![0.0; 8000], sample_rate: 16000 });
    h.stt_answer_oldest("不用了，換一個");
    let req = h.decides[0].0;
    h.step(Input::DecisionResult { req, outcome: Outcome::Ok(InterjectionDecision::Stop) });
    assert!(h.agent_aborted(0));
    assert_eq!(h.cleared, 1);
    assert_eq!(h.agents.len(), 2);
    assert_eq!(h.agents[1].1.last().unwrap(), &user("不用了，換一個"));
}

#[test]
fn stop_words_skip_handler() {
    let mut h = responding_with_speech(true);
    h.step(Input::SpeechRealStart);
    h.step(Input::SpeechEnd { audio: vec![0.0; 8000], sample_rate: 16000 });
    h.stt_answer_oldest("等一下");
    assert!(h.decides.is_empty());
    assert!(h.agent_aborted(0));
    assert_eq!(h.agents.len(), 2);
}

// ---------- tts queue behaviour (from tts-queue.test.ts) ----------

#[test]
fn lookahead_limits_in_flight_synthesis() {
    // manual synth: don't auto-complete
    let cfg = CallConfig { lookahead: 1, ..Default::default() };
    let mut m = CallMachine::new(cfg);
    m.handle(Input::Start, 0.0);
    m.handle(Input::SpeechEnd { audio: vec![0.0; 16000], sample_rate: 16000 }, 0.0);
    let cmds = m.handle(Input::SttResult { req: ReqId(1), outcome: Outcome::Ok("hi".into()) }, 0.0);
    let turn = cmds.iter().find_map(|c| if let Command::RunAgent { turn, .. } = c { Some(*turn) } else { None }).unwrap();
    let cmds = m.handle(Input::AgentDelta { turn, delta: "Sentence a. Sentence b. Sentence c. Sentence d. ".into() }, 0.0);
    let synth: Vec<SegmentId> = cmds.iter().filter_map(|c| if let Command::Synthesize { seg, .. } = c { Some(*seg) } else { None }).collect();
    assert_eq!(synth.len(), 2);
    // a ready (not yet playing) → still counts, no new synth
    let cmds = m.handle(Input::SynthFinished { seg: synth[0], error: None }, 0.0);
    assert!(!cmds.iter().any(|c| matches!(c, Command::Synthesize { .. })));
    // a starts playing → budget frees
    let cmds = m.handle(Input::SegmentStarted { seg: synth[0] }, 0.0);
    assert_eq!(cmds.iter().filter(|c| matches!(c, Command::Synthesize { .. })).count(), 1);
}

#[test]
fn echo_horizon_ages_out() {
    let mut h = H::new(false, false);
    h.speak();
    h.stt_answer_oldest("hi");
    h.agent_push("first sentence. second one. third. ");
    h.start_next();
    h.finish_current(); // 1 done at t=0
    h.now = 500.0;
    h.start_next(); // 2 playing
    // A real-start now snapshots the horizon: both sentences.
    h.step(Input::SpeechRealStart);
    h.step(Input::SpeechEnd { audio: vec![0.0; 8000], sample_rate: 16000 });
    h.stt_answer_oldest("first sentence"); // echo of an aged-in sentence → resume
    assert!(!h.paused);
    h.now = 3500.0;
    h.step(Input::SpeechRealStart);
    h.step(Input::SpeechEnd { audio: vec![0.0; 8000], sample_rate: 16000 });
    h.stt_answer_oldest("first sentence"); // aged out (>2 s) → not echo → real speech
    assert!(h.agent_aborted(0));
}
