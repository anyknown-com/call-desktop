//! cpal device I/O. Owns the input and output streams on a dedicated thread.
//!
//! Capture: device → mono → 48 kHz → 10 ms frames → APM (render frames drained first) → 16 kHz →
//! `capture_tx` (variable-size chunks). Playback: `PlaybackSink::render` → far-end tap (SPSC to
//! the capture side for AEC) → device channels.

use crate::apm::{Apm, ApmOptions};
use crate::resample::Resampler;
use crate::sink::{PlaybackSink, SinkEvent};
use crate::{APM_FRAME, APM_RATE, MODEL_RATE};
use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    /// Substring match on the device name; None = default device.
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub apm: ApmOptions,
}

pub struct AudioEngine {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
    sink: PlaybackSink,
}

impl AudioEngine {
    /// Start both streams. `capture_tx` receives 16 kHz mono chunks (after AEC/NS/AGC);
    /// `sink_events` receives playback events.
    pub fn start(cfg: EngineConfig, capture_tx: Sender<Vec<f32>>, sink_events: Sender<SinkEvent>) -> Result<Self> {
        let sink = PlaybackSink::new(sink_events);
        let sink2 = sink.clone();
        let (stop_tx, stop_rx) = channel::<()>();
        let (ready_tx, ready_rx) = channel::<Result<()>>();
        let thread = std::thread::Builder::new()
            .name("voice-audio".into())
            .spawn(move || match build_streams(&cfg, sink2, capture_tx) {
                Ok((_in_stream, _out_stream)) => {
                    let _ = ready_tx.send(Ok(()));
                    let _ = stop_rx.recv(); // streams live until dropped here
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .context("spawn audio thread")?;
        ready_rx.recv().map_err(|_| anyhow!("audio thread died"))??;
        Ok(Self { stop: stop_tx, thread: Some(thread), sink })
    }

    pub fn sink(&self) -> &PlaybackSink {
        &self.sink
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn pick(mut devices: impl Iterator<Item = cpal::Device>, name: &Option<String>, default: Option<cpal::Device>) -> Result<cpal::Device> {
    match name {
        None => default.ok_or_else(|| anyhow!("no default audio device")),
        Some(n) => devices
            .find(|d| d.description().map(|d| d.name().contains(n.as_str())).unwrap_or(false))
            .ok_or_else(|| anyhow!("audio device matching {n:?} not found")),
    }
}

pub fn list_devices() -> Result<(Vec<String>, Vec<String>)> {
    let host = cpal::default_host();
    let ins = host.input_devices()?.filter_map(|d| d.description().ok().map(|d| d.name().to_string())).collect();
    let outs = host.output_devices()?.filter_map(|d| d.description().ok().map(|d| d.name().to_string())).collect();
    Ok((ins, outs))
}

fn build_streams(cfg: &EngineConfig, sink: PlaybackSink, capture_tx: Sender<Vec<f32>>) -> Result<(cpal::Stream, cpal::Stream)> {
    let host = cpal::default_host();
    let in_dev = pick(host.input_devices()?, &cfg.input_device, host.default_input_device())?;
    let out_dev = pick(host.output_devices()?, &cfg.output_device, host.default_output_device())?;
    let in_cfg = in_dev.default_input_config().context("input config")?;
    let out_cfg = out_dev.default_output_config().context("output config")?;
    tracing::info!(input = ?in_dev.description().map(|d| d.name().to_string()), rate = in_cfg.sample_rate(), ch = in_cfg.channels(), "capture");
    tracing::info!(output = ?out_dev.description().map(|d| d.name().to_string()), rate = out_cfg.sample_rate(), ch = out_cfg.channels(), "playback");

    let apm = Arc::new(Apm::new(cfg.apm)?);
    // Far-end tap: what the output callback actually played, 48 kHz mono, consumed by capture.
    let (mut far_tx, mut far_rx) = rtrb::RingBuffer::<f32>::new(APM_RATE as usize * 2);

    // ---- output ----
    let out_ch = out_cfg.channels() as usize;
    let out_rate = out_cfg.sample_rate();
    let mut out_resampler = Resampler::new(APM_RATE, out_rate, 10)?; // 48k → device rate
    let mut mono_scratch: Vec<f32> = Vec::new();
    let mut resampled_pending: std::collections::VecDeque<f32> = std::collections::VecDeque::new();
    let out_stream = out_dev.build_output_stream(
        out_cfg.config(),
        move |data: &mut [f32], _| {
            let frames = data.len() / out_ch;
            // Pull enough 48k audio to cover `frames` device frames after resampling.
            while resampled_pending.len() < frames {
                let need48 = ((frames - resampled_pending.len()) as f64 * APM_RATE as f64 / out_rate as f64).ceil() as usize + APM_FRAME;
                mono_scratch.resize(need48, 0.0);
                sink.render(&mut mono_scratch);
                for s in &mono_scratch {
                    let _ = far_tx.push(*s);
                }
                let r = out_resampler.push(&mono_scratch);
                if r.is_empty() && out_rate == APM_RATE {
                    break;
                }
                resampled_pending.extend(r);
                if out_rate == APM_RATE {
                    break;
                }
            }
            for f in 0..frames {
                let s = resampled_pending.pop_front().unwrap_or(0.0);
                for c in 0..out_ch {
                    data[f * out_ch + c] = s;
                }
            }
        },
        |e| tracing::error!("output stream error: {e}"),
        None,
    )?;

    // ---- input ----
    let in_ch = in_cfg.channels() as usize;
    let apm_c = apm.clone();
    let mut in_resampler = Resampler::new(in_cfg.sample_rate(), APM_RATE, 10)?;
    let mut down = Resampler::new(APM_RATE, MODEL_RATE, 10)?;
    let mut acc: Vec<f32> = Vec::with_capacity(APM_FRAME * 4);
    let mut far_frame = [0f32; APM_FRAME];
    let mut mono_in: Vec<f32> = Vec::new();
    let in_stream = in_dev.build_input_stream(
        in_cfg.config(),
        move |data: &[f32], _| {
            mono_in.clear();
            for fr in data.chunks_exact(in_ch) {
                mono_in.push(fr.iter().sum::<f32>() / in_ch as f32);
            }
            acc.extend(in_resampler.push(&mono_in));
            let mut out16: Vec<f32> = Vec::new();
            while acc.len() >= APM_FRAME {
                // Feed every complete far-end frame that has been played since last time.
                while far_rx.slots() >= APM_FRAME {
                    for s in far_frame.iter_mut() {
                        *s = far_rx.pop().unwrap_or(0.0);
                    }
                    apm_c.process_render(&far_frame);
                }
                let mut frame = [0f32; APM_FRAME];
                frame.copy_from_slice(&acc[..APM_FRAME]);
                acc.drain(..APM_FRAME);
                apm_c.process_capture(&mut frame);
                out16.extend(down.push(&frame));
            }
            if !out16.is_empty() {
                let _ = capture_tx.send(out16);
            }
        },
        |e| tracing::error!("input stream error: {e}"),
        None,
    )?;

    out_stream.play()?;
    in_stream.play()?;
    Ok((in_stream, out_stream))
}
