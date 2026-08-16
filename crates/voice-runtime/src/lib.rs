//! The running call: executes `voice_core::call_machine` commands with tokio (STT/LLM/TTS
//! tasks, timers), drives the audio engine + pipeline thread, ducks other apps' audio while the
//! assistant speaks, and persists transcripts. The CLI, the GPUI app and the MCP server are thin
//! front-ends over [`Runtime`].

pub mod mock;
pub mod pipeline;
pub mod settings;
pub mod transcript;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use settings::{DuckMode, Keys, Settings, TtsProvider};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::AbortHandle;
use voice_audio::engine::{AudioEngine, EngineConfig};
use voice_audio::sink::{PlaybackSink, SinkEvent};
use voice_core::call_machine::{CallConfig, CallMachine, CallState, Command, Input, Outcome, ReqId, SegmentId, TimerId, TurnId};
use voice_core::media_gate::GateState;
use voice_core::speaker_profile::SpeakerProfile;
use voice_providers::{
    Agent, AgentClient, AgentConfig, ElevenLabsStt, ElevenLabsSttConfig, ElevenLabsTts, ElevenLabsTtsConfig, FastLlm, FastLlmConfig,
    InterjectionHandler, OpenAiTts, OpenAiTtsConfig, SttClient, TtsClient, TurnDetector,
};

/// What the pipeline thread reports.
pub enum PipelineOut {
    Input(Input),
    Level { prob: f32 },
    GateState(GateState),
    TurnRejected(String),
    Error(String),
}

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

struct Providers {
    stt: Arc<dyn SttClient>,
    tts: Arc<dyn TtsClient>,
    agent: Arc<dyn AgentClient>,
    judge: Option<Arc<dyn TurnDetector>>,
    interjection: Option<Arc<dyn InterjectionHandler>>,
}

fn build_providers(s: &Settings, k: &Keys) -> Result<Providers> {
    let missing = k.missing(s);
    if !missing.is_empty() {
        return Err(anyhow!("missing API keys: {}", missing.join(", ")));
    }
    let llm_key = match s.llm.provider {
        voice_providers::LlmProvider::OpenAi => k.openai.clone(),
        voice_providers::LlmProvider::Anthropic => k.anthropic.clone(),
    };
    let stt: Arc<dyn SttClient> = Arc::new(ElevenLabsStt::new(ElevenLabsSttConfig {
        api_key: k.elevenlabs.clone(),
        model: s.stt.model.clone(),
        language_code: s.stt.language_code.clone(),
    }));
    let tts: Arc<dyn TtsClient> = match s.tts.provider {
        TtsProvider::ElevenLabs => Arc::new(ElevenLabsTts::new(ElevenLabsTtsConfig {
            api_key: k.elevenlabs.clone(),
            model: s.tts.elevenlabs_model.clone(),
            voice_id: s.tts.elevenlabs_voice_id.clone(),
        })),
        TtsProvider::OpenAi => Arc::new(OpenAiTts::new(OpenAiTtsConfig { api_key: k.openai.clone(), model: s.tts.openai_model.clone(), voice: s.tts.openai_voice.clone() })),
    };
    let agent: Arc<dyn AgentClient> = Arc::new(Agent::new(AgentConfig {
        provider: s.llm.provider,
        model: s.llm.model.clone(),
        api_key: llm_key.clone(),
        system_prompt: s.system_prompt.clone(),
        effort: s.llm.effort,
    }));
    let fast = Arc::new(FastLlm::new(FastLlmConfig { provider: s.llm.provider, model: s.llm.fast_model.clone(), api_key: llm_key, effort: s.llm.fast_effort }));
    Ok(Providers {
        stt,
        tts,
        agent,
        judge: s.turn.semantic.then(|| fast.clone() as Arc<dyn TurnDetector>),
        interjection: s.turn.interjections.then_some(fast as Arc<dyn InterjectionHandler>),
    })
}

fn mock_providers(s: &Settings) -> Providers {
    let fast = Arc::new(mock::MockFast);
    Providers {
        stt: Arc::new(mock::MockStt::new()),
        tts: Arc::new(mock::MockTts),
        agent: Arc::new(mock::MockAgent),
        judge: s.turn.semantic.then(|| fast.clone() as Arc<dyn TurnDetector>),
        interjection: s.turn.interjections.then_some(fast as Arc<dyn InterjectionHandler>),
    }
}

struct Driver {
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
    ducker: Option<Box<dyn voice_os::MediaDucker>>,
    duck_restore: Option<AbortHandle>,
    last_state: Option<CallState>,
    pipeline_stop: Arc<AtomicBool>,
    pipeline_thread: Option<std::thread::JoinHandle<()>>,
}

impl Driver {
    fn new(opts: RuntimeOptions, events: UnboundedSender<RuntimeEvent>) -> Result<Driver> {
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

        let ducker = match settings.audio.duck {
            DuckMode::Off => None,
            DuckMode::Mute => match voice_os::create_ducker() {
                Ok(d) => {
                    tracing::info!(backend = d.backend_name(), "media ducking enabled");
                    Some(d)
                }
                Err(e) => {
                    let _ = events.send(RuntimeEvent::Error(format!("media ducking unavailable: {e}")));
                    None
                }
            },
        };

        let cfg = CallConfig {
            hold_ms: settings.turn.hold_ms as f64,
            commit_ms: settings.turn.commit_ms as f64,
            has_turn_detector: providers.judge.is_some(),
            has_interjection_handler: providers.interjection.is_some(),
            ..Default::default()
        };
        let (inputs_tx, inputs_rx) = unbounded_channel();
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
            ducker,
            duck_restore: None,
            last_state: None,
            pipeline_stop,
            pipeline_thread: Some(pipeline_thread),
        })
    }

    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    async fn run(mut self, mut cmds: UnboundedReceiver<RuntimeCommand>) {
        let mut inputs_rx = self.inputs_rx.take().unwrap();
        let mut pipe_rx = self.pipe_rx.take().unwrap();
        let mut sink_rx = self.sink_rx.take().unwrap();
        self.emit_state();
        loop {
            tokio::select! {
                Some(cmd) = cmds.recv() => match cmd {
                    RuntimeCommand::Start => self.step(Input::Start),
                    RuntimeCommand::Hangup => self.hangup(),
                    RuntimeCommand::Interrupt => self.step(Input::Interrupt),
                    RuntimeCommand::Shutdown => break,
                },
                Some(input) = inputs_rx.recv() => self.step(input),
                Some(out) = pipe_rx.recv() => match out {
                    PipelineOut::Input(i) => self.step(i),
                    PipelineOut::Level { prob } => { let _ = self.events.send(RuntimeEvent::Level(prob)); }
                    PipelineOut::GateState(s) => { let _ = self.events.send(RuntimeEvent::GateState(s)); }
                    PipelineOut::TurnRejected(r) => { let _ = self.events.send(RuntimeEvent::Hint(format!("voice not verified ({r})"))); }
                    PipelineOut::Error(e) => { let _ = self.events.send(RuntimeEvent::Error(e)); }
                },
                Some(ev) = sink_rx.recv() => match ev {
                    SinkEvent::SegmentStarted(seg) => self.step(Input::SegmentStarted { seg }),
                    SinkEvent::SegmentEnded(seg) => self.step(Input::SegmentEnded { seg }),
                    SinkEvent::Active(on) => self.on_active(on),
                },
                else => break,
            }
        }
        self.hangup();
        self.restore_duck();
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
        self.restore_duck();
    }

    fn step(&mut self, input: Input) {
        if input == (Input::Timer { id: DUCK_RESTORE_TIMER }) {
            self.duck_restore = None;
            self.restore_duck();
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
            self.last_state = Some(st.clone());
            let _ = self.events.send(RuntimeEvent::State(st));
        }
    }

    // ---------- ducking ----------

    fn on_active(&mut self, on: bool) {
        if self.ducker.is_none() {
            return;
        }
        if on {
            if let Some(h) = self.duck_restore.take() {
                h.abort();
            }
            if let Some(d) = self.ducker.as_mut() {
                if !d.is_ducked() {
                    match d.duck(voice_os::DuckMode::Mute) {
                        Ok(()) => {
                            let _ = self.events.send(RuntimeEvent::Ducked(true));
                        }
                        Err(e) => {
                            let _ = self.events.send(RuntimeEvent::Error(format!("duck: {e}")));
                        }
                    }
                }
            }
        } else {
            // Debounce so back-to-back sentences don't flap.
            if let Some(h) = self.duck_restore.take() {
                h.abort();
            }
            let tx = self.inputs_tx.clone();
            let h = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let _ = tx.send(Input::Timer { id: DUCK_RESTORE_TIMER });
            });
            self.duck_restore = Some(h.abort_handle());
        }
    }

    fn restore_duck(&mut self) {
        if let Some(h) = self.duck_restore.take() {
            h.abort();
        }
        if let Some(d) = self.ducker.as_mut() {
            if d.is_ducked() {
                if let Err(e) = d.restore() {
                    let _ = self.events.send(RuntimeEvent::Error(format!("restore audio: {e}")));
                }
                let _ = self.events.send(RuntimeEvent::Ducked(false));
            }
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
            Command::RunAgent { turn, history } => {
                let agent = self.providers.agent.clone();
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

/// Timer id reserved for the ducking debounce (machine ids start at 1 and count up; u64::MAX
/// can never collide).
const DUCK_RESTORE_TIMER: TimerId = TimerId(u64::MAX);
