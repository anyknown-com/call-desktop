//! The driver task: owns the [`CallMachine`], executes its commands with tokio (STT/LLM/TTS
//! tasks, timers), and multiplexes runtime commands, pipeline output and sink events.

use crate::ducking::{Ducking, DUCK_RESTORE_TIMER};
use crate::pipeline::{self, PipelineOut};
use crate::proactive::{Proactivity, GREETING};
use crate::providers::{build_providers, mock_providers, Providers};
use crate::settings::DuckMode;
use crate::{transcript, RuntimeCommand, RuntimeEvent, RuntimeOptions};
use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::AbortHandle;
use voice_audio::engine::{AudioEngine, EngineConfig};
use voice_audio::sink::{PlaybackSink, SinkEvent};
use voice_core::call_machine::{CallConfig, CallMachine, CallState, ChatMessage, Command, Input, Outcome, ReqId, Role, SegmentId, TimerId, TurnId};

pub struct Driver {
    machine: CallMachine,
    providers: Providers,
    engine: AudioEngine,
    sink: PlaybackSink,
    inputs_tx: UnboundedSender<Input>,
    inputs_rx: Option<UnboundedReceiver<Input>>,
    pipe_rx: Option<UnboundedReceiver<PipelineOut>>,
    sink_rx: Option<UnboundedReceiver<SinkEvent>>,
    events: UnboundedSender<RuntimeEvent>,
    started: Instant,
    stt_tasks: HashMap<ReqId, AbortHandle>,
    agent_tasks: HashMap<TurnId, AbortHandle>,
    synth_tasks: HashMap<SegmentId, AbortHandle>,
    judge_tasks: HashMap<ReqId, AbortHandle>,
    decide_tasks: HashMap<ReqId, AbortHandle>,
    timers: HashMap<TimerId, AbortHandle>,
    ducking: Option<Ducking>,
    last_state: Option<CallState>,
    proactive: Option<Proactivity>,
    mic_muted: bool,
    pipeline_stop: Arc<AtomicBool>,
    pipeline_thread: Option<std::thread::JoinHandle<()>>,
}

impl Driver {
    pub fn new(opts: RuntimeOptions, events: UnboundedSender<RuntimeEvent>) -> Result<Driver> {
        let RuntimeOptions { settings, keys, profile, models_dir, mock, input_wav } = opts;
        let providers = if mock { mock_providers(&settings) } else { build_providers(&settings, &keys)? };

        // Models
        let vad_model = voice_ml::SileroVad::new(models_dir.join(voice_ml::models::SILERO_VAD_V5_FILE)).context("load Silero VAD")?;
        let vad_cfg = voice_ml::VadConfig { redemption_ms: settings.audio.silence_ms, ..Default::default() };
        let vad = voice_ml::VadDetector::new(vad_model, vad_cfg);
        let media = if settings.media_mode {
            let profile = profile.ok_or_else(|| anyhow!("media mode needs a speaker profile (enroll first)"))?;
            let emb = voice_ml::SpeakerEmbedder::new(models_dir.join(voice_ml::models::CAMPPLUS_FILE)).context("load CAM++")?;
            if !voice_core::speaker_profile::profile_compatible(&profile, voice_ml::CAMPPLUS_SHA256, voice_core::fbank::FRONTEND_VERSION) {
                return Err(anyhow!("speaker profile was built with a different model/frontend; re-enroll"));
            }
            Some((profile, emb))
        } else {
            None
        };

        // Audio
        let (cap_tx, cap_rx) = std::sync::mpsc::channel::<Vec<f32>>();
        let (sink_std_tx, sink_std_rx) = std::sync::mpsc::channel::<SinkEvent>();
        let engine = AudioEngine::start(
            EngineConfig {
                input_device: settings.audio.input_device.clone(),
                output_device: settings.audio.output_device.clone(),
                apm: voice_audio::apm::ApmOptions { noise_suppression: settings.audio.noise_suppression, agc: settings.audio.agc },
                input_wav,
            },
            cap_tx,
            sink_std_tx,
        )
        .context("start audio engine")?;
        let sink = engine.sink().clone();

        // Bridge std channels → tokio channels.
        let (sink_tx, sink_rx) = unbounded_channel();
        std::thread::Builder::new()
            .name("sink-events".into())
            .spawn(move || {
                while let Ok(ev) = sink_std_rx.recv() {
                    if sink_tx.send(ev).is_err() {
                        break;
                    }
                }
            })
            .context("spawn sink bridge")?;
        let (pipe_tx, pipe_rx) = unbounded_channel();
        let pipeline_stop = Arc::new(AtomicBool::new(false));
        let stop2 = pipeline_stop.clone();
        let pipeline_thread = std::thread::Builder::new()
            .name("voice-pipeline".into())
            .spawn(move || pipeline::run(pipeline::PipelineConfig { vad, media }, cap_rx, pipe_tx, stop2))
            .context("spawn pipeline")?;

        let (inputs_tx, inputs_rx) = unbounded_channel();
        let ducking = match settings.audio.duck {
            DuckMode::Off => None,
            DuckMode::Mute => Ducking::open(inputs_tx.clone(), events.clone()),
        };

        let cfg = CallConfig {
            hold_ms: settings.turn.hold_ms as f64,
            commit_ms: settings.turn.commit_ms as f64,
            has_turn_detector: providers.judge.is_some(),
            has_interjection_handler: providers.interjection.is_some(),
            ..Default::default()
        };
        Ok(Driver {
            machine: CallMachine::new(cfg),
            providers,
            engine,
            sink,
            inputs_tx,
            inputs_rx: Some(inputs_rx),
            pipe_rx: Some(pipe_rx),
            sink_rx: Some(sink_rx),
            events,
            started: Instant::now(),
            stt_tasks: HashMap::new(),
            agent_tasks: HashMap::new(),
            synth_tasks: HashMap::new(),
            judge_tasks: HashMap::new(),
            decide_tasks: HashMap::new(),
            timers: HashMap::new(),
            ducking,
            last_state: None,
            mic_muted: false,
            proactive: settings.turn.proactive.then(|| Proactivity::new(settings.turn.idle_nudge_secs)),
            pipeline_stop,
            pipeline_thread: Some(pipeline_thread),
        })
    }

    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    pub async fn run(mut self, mut cmds: UnboundedReceiver<RuntimeCommand>) {
        let mut inputs_rx = self.inputs_rx.take().unwrap();
        let mut pipe_rx = self.pipe_rx.take().unwrap();
        let mut sink_rx = self.sink_rx.take().unwrap();
        self.emit_state();
        tracing::info!("driver running");
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if let Some(i) = self.proactive.as_mut().and_then(|p| p.due()) {
                        self.step(Input::Proactive { instruction: i.into() });
                    }
                }
                Some(cmd) = cmds.recv() => match cmd {
                    RuntimeCommand::Start => {
                        self.step(Input::Start);
                        if self.proactive.is_some() {
                            self.step(Input::Proactive { instruction: GREETING.into() });
                        }
                    }
                    RuntimeCommand::Hangup => self.hangup(),
                    RuntimeCommand::Interrupt => self.step(Input::Interrupt),
                    RuntimeCommand::SetMicMuted(m) => {
                        self.mic_muted = m;
                        if m {
                            self.step(Input::SpeechMisfire);
                        }
                    }
                    RuntimeCommand::Shutdown => break,
                },
                Some(input) = inputs_rx.recv() => self.step(input),
                Some(out) = pipe_rx.recv() => match out {
                    PipelineOut::Input(_) if self.mic_muted => {}
                    PipelineOut::Input(i) => self.step(i),
                    PipelineOut::Level { prob } => { let _ = self.events.send(RuntimeEvent::Level(if self.mic_muted { 0.0 } else { prob })); }
                    PipelineOut::GateState(s) => { let _ = self.events.send(RuntimeEvent::GateState(s)); }
                    PipelineOut::TurnRejected(r) => { let _ = self.events.send(RuntimeEvent::Hint(format!("voice not verified ({r})"))); }
                    PipelineOut::Error(e) => { let _ = self.events.send(RuntimeEvent::Error(e)); }
                },
                Some(ev) = sink_rx.recv() => match ev {
                    SinkEvent::SegmentStarted(seg) => self.step(Input::SegmentStarted { seg }),
                    SinkEvent::SegmentEnded(seg) => self.step(Input::SegmentEnded { seg }),
                    SinkEvent::Active(on) => {
                        if let Some(d) = self.ducking.as_mut() {
                            d.on_active(on);
                        }
                    }
                },
                else => break,
            }
        }
        tracing::info!("driver stopping");
        self.hangup();
        if let Some(d) = self.ducking.as_mut() {
            d.restore();
        }
        self.pipeline_stop.store(true, Ordering::Relaxed);
        // Dropping the engine closes the capture channel, which ends the pipeline thread.
        let Driver { engine, pipeline_thread, .. } = self;
        drop(engine);
        if let Some(t) = pipeline_thread {
            let _ = t.join();
        }
    }

    fn hangup(&mut self) {
        if !self.machine.state().active {
            return;
        }
        self.step(Input::Hangup);
        match transcript::save(self.machine.turns()) {
            Ok(Some(p)) => {
                let _ = self.events.send(RuntimeEvent::Saved(p));
            }
            Ok(None) => {}
            Err(e) => {
                let _ = self.events.send(RuntimeEvent::Error(format!("save transcript: {e}")));
            }
        }
        if let Some(d) = self.ducking.as_mut() {
            d.restore();
        }
    }

    fn step(&mut self, input: Input) {
        if input == (Input::Timer { id: DUCK_RESTORE_TIMER }) {
            if let Some(d) = self.ducking.as_mut() {
                d.on_restore_timer();
            }
            return;
        }
        let cmds = self.machine.handle(input, self.now());
        for c in cmds {
            self.exec(c);
        }
        self.emit_state();
    }

    fn emit_state(&mut self) {
        let st = self.machine.state();
        if self.last_state.as_ref() != Some(&st) {
            if let Some(p) = self.proactive.as_mut() {
                p.observe(&st);
            }
            self.last_state = Some(st.clone());
            let _ = self.events.send(RuntimeEvent::State(st));
        }
    }

    // ---------- command execution ----------

    fn exec(&mut self, c: Command) {
        let tx = self.inputs_tx.clone();
        match c {
            Command::Transcribe { req, audio, sample_rate, timeout_ms } => {
                let stt = self.providers.stt.clone();
                let h = tokio::spawn(async move {
                    let outcome = match tokio::time::timeout(Duration::from_millis(timeout_ms as u64), stt.transcribe(&audio, sample_rate)).await {
                        Ok(Ok(t)) => Outcome::Ok(t),
                        Ok(Err(e)) => Outcome::Failed(e.to_string()),
                        Err(_) => Outcome::Aborted,
                    };
                    let _ = tx.send(Input::SttResult { req, outcome });
                });
                self.stt_tasks.insert(req, h.abort_handle());
            }
            Command::CancelTranscribe { req } => abort(&mut self.stt_tasks, &req),
            Command::RunAgent { turn, mut history, nudge } => {
                let agent = self.providers.agent.clone();
                if let Some(n) = nudge {
                    history.push(ChatMessage { role: Role::User, content: format!("(system note, not spoken by the user: {n})") });
                }
                let h = tokio::spawn(async move {
                    let error = match agent.run(history).await {
                        Ok(mut stream) => {
                            let mut err = None;
                            while let Some(item) = stream.next().await {
                                match item {
                                    Ok(delta) => {
                                        let _ = tx.send(Input::AgentDelta { turn, delta });
                                    }
                                    Err(e) => {
                                        err = Some(e.to_string());
                                        break;
                                    }
                                }
                            }
                            err
                        }
                        Err(e) => Some(e.to_string()),
                    };
                    let _ = tx.send(Input::AgentFinished { turn, error });
                });
                self.agent_tasks.insert(turn, h.abort_handle());
            }
            Command::CancelAgent { turn } => abort(&mut self.agent_tasks, &turn),
            Command::Synthesize { seg, text, priority, timeout_ms } => {
                let tts = self.providers.tts.clone();
                let sink = self.sink.clone();
                sink.add_segment(seg, priority);
                let h = tokio::spawn(async move {
                    let work = async {
                        let mut stream = tts.synthesize(&text).await?;
                        while let Some(chunk) = stream.chunks.next().await {
                            let chunk = chunk?;
                            sink.write(seg, &chunk, stream.sample_rate);
                        }
                        Ok::<(), voice_providers::Error>(())
                    };
                    let error = match tokio::time::timeout(Duration::from_millis(timeout_ms as u64), work).await {
                        Ok(Ok(())) => None,
                        Ok(Err(e)) => Some(e.to_string()),
                        Err(_) => Some("timeout".into()),
                    };
                    sink.end(seg);
                    let _ = tx.send(Input::SynthFinished { seg, error });
                });
                self.synth_tasks.insert(seg, h.abort_handle());
            }
            Command::CancelSynthesize { seg } => abort(&mut self.synth_tasks, &seg),
            Command::SinkPause => self.sink.pause(),
            Command::SinkResume => self.sink.resume(),
            Command::SinkClear => self.sink.clear(),
            Command::Judge { req, history, utterance } => {
                let Some(j) = self.providers.judge.clone() else { return };
                let h = tokio::spawn(async move {
                    let outcome = match j.judge(&history, &utterance).await {
                        Ok(v) => Outcome::Ok(v),
                        Err(e) => Outcome::Failed(e.to_string()),
                    };
                    let _ = tx.send(Input::JudgeResult { req, outcome });
                });
                self.judge_tasks.insert(req, h.abort_handle());
            }
            Command::CancelJudge { req } => abort(&mut self.judge_tasks, &req),
            Command::Decide { req, history, spoken_so_far, playing, interjection } => {
                let Some(d) = self.providers.interjection.clone() else { return };
                let h = tokio::spawn(async move {
                    let outcome = match d.decide(&history, &spoken_so_far, &playing, &interjection).await {
                        Ok(v) => Outcome::Ok(v),
                        Err(e) => Outcome::Failed(e.to_string()),
                    };
                    let _ = tx.send(Input::DecisionResult { req, outcome });
                });
                self.decide_tasks.insert(req, h.abort_handle());
            }
            Command::CancelDecide { req } => abort(&mut self.decide_tasks, &req),
            Command::SetTimer { id, ms } => {
                let h = tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(ms as u64)).await;
                    let _ = tx.send(Input::Timer { id });
                });
                self.timers.insert(id, h.abort_handle());
            }
            Command::CancelTimer { id } => abort(&mut self.timers, &id),
        }
    }
}

fn abort<K: std::hash::Hash + Eq>(map: &mut HashMap<K, AbortHandle>, k: &K) {
    if let Some(h) = map.remove(k) {
        h.abort();
    }
}
