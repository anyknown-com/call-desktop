//! The capture pipeline thread: 16 kHz chunks from the audio engine → 512-sample windows → Silero
//! VAD → either straight to the call machine (Conversation mode) or through the speaker gate
//! (Media mode). Runs on a plain thread; ML inference blocks here, never on the audio callbacks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use voice_audio::media_turn::{MediaTurnController, MediaTurnEvent, MediaTurnOptions, SpeakerScorer};
use voice_core::call_machine::Input;
use voice_core::media_gate::GateState;
use voice_core::speaker_profile::SpeakerProfile;
use voice_ml::{SpeakerEmbedder, VadDetector, VadEvent};

/// What the pipeline thread reports.
pub enum PipelineOut {
    Input(Input),
    Level { prob: f32 },
    GateState(GateState),
    TurnRejected(String),
    Error(String),
}

pub struct EmbedderScorer(pub SpeakerEmbedder);
impl SpeakerScorer for EmbedderScorer {
    fn embed(&mut self, pcm16k: &[f32]) -> anyhow::Result<Vec<f32>> {
        self.0.embed(pcm16k)
    }
}

pub struct PipelineConfig {
    pub vad: VadDetector,
    /// Media mode needs a profile and the embedder; None = conversation mode.
    pub media: Option<(SpeakerProfile, SpeakerEmbedder)>,
}

/// Runs until `capture_rx` closes or `stop` is set.
pub fn run(cfg: PipelineConfig, capture_rx: Receiver<Vec<f32>>, out: UnboundedSender<PipelineOut>, stop: Arc<AtomicBool>) {
    let PipelineConfig { mut vad, media } = cfg;
    let mut gate = media.map(|(profile, emb)| MediaTurnController::new(profile, EmbedderScorer(emb), MediaTurnOptions::default(), None));
    let mut acc: Vec<f32> = Vec::with_capacity(2048);
    let mut vad_active = false;
    while let Ok(chunk) = capture_rx.recv() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        acc.extend_from_slice(&chunk);
        while acc.len() >= 512 {
            let frame: Vec<f32> = acc.drain(..512).collect();
            let (prob, events) = match vad.process(&frame) {
                Ok(r) => r,
                Err(e) => {
                    let _ = out.send(PipelineOut::Error(format!("VAD: {e}")));
                    continue;
                }
            };
            let _ = out.send(PipelineOut::Level { prob });
            match gate.as_mut() {
                None => {
                    for ev in events {
                        let input = match ev {
                            VadEvent::Start => Input::SpeechStart,
                            VadEvent::RealStart => Input::SpeechRealStart,
                            VadEvent::Misfire => Input::SpeechMisfire,
                            VadEvent::End { audio } => Input::SpeechEnd { audio, sample_rate: 16_000 },
                        };
                        let _ = out.send(PipelineOut::Input(input));
                    }
                }
                Some(g) => {
                    // Media mode: generic VAD never touches playback; only the verified speaker
                    // interrupts, and only verified turns reach STT.
                    let mut evs = vec![];
                    for ev in events {
                        match ev {
                            VadEvent::Start => {
                                vad_active = true;
                                evs.extend(g.vad_start());
                            }
                            VadEvent::Misfire | VadEvent::End { .. } => {
                                vad_active = false;
                                evs.extend(g.vad_end());
                            }
                            VadEvent::RealStart => {}
                        }
                    }
                    let _ = vad_active;
                    evs.extend(g.push_frame(&frame));
                    for e in evs {
                        match e {
                            MediaTurnEvent::CutPlayback => {
                                let _ = out.send(PipelineOut::Input(Input::Interrupt));
                            }
                            MediaTurnEvent::TurnAccepted(audio) => {
                                let _ = out.send(PipelineOut::Input(Input::SpeechEnd { audio, sample_rate: 16_000 }));
                            }
                            MediaTurnEvent::TurnRejected(r) => {
                                let _ = out.send(PipelineOut::TurnRejected(format!("{r:?}")));
                            }
                            MediaTurnEvent::StateChanged(s) => {
                                let _ = out.send(PipelineOut::GateState(s));
                            }
                            MediaTurnEvent::Error(e) => {
                                let _ = out.send(PipelineOut::Error(e));
                            }
                        }
                    }
                }
            }
        }
    }
}
