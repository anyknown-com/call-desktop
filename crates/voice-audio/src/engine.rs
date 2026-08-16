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
    /// Test harness: read the "microphone" from this WAV (mono/stereo, any rate) in real time
    /// instead of a device. Still goes through the APM so echo behaviour is exercised.
    pub input_wav: Option<std::path::PathBuf>,
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

fn build_streams(cfg: &EngineConfig, sink: PlaybackSink, capture_tx: Sender<Vec<f32>>) -> Result<(Option<cpal::Stream>, cpal::Stream)> {
    let host = cpal::default_host();
    let out_dev = pick(host.output_devices()?, &cfg.output_device, host.default_output_device())?;
    let out_cfg = out_dev.default_output_config().context("output config")?;
    tracing::info!(output = ?out_dev.description().map(|d| d.name().to_string()), rate = out_cfg.sample_rate(), ch = out_cfg.channels(), "playback");

    let apm = Arc::new(Apm::new(cfg.apm)?);
    // Far-end tap: what the output callback actually played, 48 kHz mono, consumed by capture.
    let (mut far_tx, far_rx) = rtrb::RingBuffer::<f32>::new(APM_RATE as usize * 2);

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

    out_stream.play()?;

    // ---- input ----
    if let Some(path) = &cfg.input_wav {
        spawn_wav_mic(path.clone(), apm.clone(), far_rx, capture_tx)?;
        return Ok((None, out_stream));
    }
    let in_dev = pick(host.input_devices()?, &cfg.input_device, host.default_input_device())?;
    let in_cfg = in_dev.default_input_config().context("input config")?;
    tracing::info!(input = ?in_dev.description().map(|d| d.name().to_string()), rate = in_cfg.sample_rate(), ch = in_cfg.channels(), "capture");
    let in_ch = in_cfg.channels() as usize;
    let mut cap = CaptureChain::new(in_cfg.sample_rate(), apm.clone(), far_rx, capture_tx)?;
    let mut mono_in: Vec<f32> = Vec::new();
    let in_stream = in_dev.build_input_stream(
        in_cfg.config(),
        move |data: &[f32], _| {
            mono_in.clear();
            for fr in data.chunks_exact(in_ch) {
                mono_in.push(fr.iter().sum::<f32>() / in_ch as f32);
            }
            cap.push(&mono_in);
        },
        |e| tracing::error!("input stream error: {e}"),
        None,
    )?;

    in_stream.play()?;
    Ok((Some(in_stream), out_stream))
}

/// Test harness: play a WAV as the microphone in real time (10 ms ticks), then silence forever.
fn spawn_wav_mic(path: std::path::PathBuf, apm: Arc<Apm>, far_rx: rtrb::Consumer<f32>, capture_tx: Sender<Vec<f32>>) -> Result<()> {
    let mut reader = hound::WavReader::open(&path).with_context(|| format!("open {}", path.display()))?;
    let spec = reader.spec();
    let ch = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.unwrap_or(0) as f32 / scale).collect()
        }
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
    };
    let mono: Vec<f32> = samples.chunks_exact(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();
    let rate = spec.sample_rate;
    tracing::info!(?path, rate, secs = mono.len() as f32 / rate as f32, "wav mic");
    let mut cap = CaptureChain::new(rate, apm, far_rx, capture_tx)?;
    let tick = (rate / 100) as usize; // 10 ms
    std::thread::Builder::new()
        .name("wav-mic".into())
        .spawn(move || {
            let start = std::time::Instant::now();
            let mut i = 0usize;
            let silence = vec![0f32; tick];
            let mut n = 0u64;
            loop {
                let chunk = if i < mono.len() { &mono[i..(i + tick).min(mono.len())] } else { &silence[..] };
                i += tick;
                cap.push(chunk);
                n += 1;
                let due = start + std::time::Duration::from_millis(n * 10);
                let now = std::time::Instant::now();
                if due > now {
                    std::thread::sleep(due - now);
                }
            }
        })
        .context("spawn wav mic")?;
    Ok(())
}

/// Mic samples (mono, device rate) → 48 kHz → APM (render frames drained first) → 16 kHz → channel.
struct CaptureChain {
    apm: Arc<Apm>,
    far_rx: rtrb::Consumer<f32>,
    capture_tx: Sender<Vec<f32>>,
    in_resampler: Resampler,
    down: Resampler,
    acc: Vec<f32>,
    far_frame: [f32; APM_FRAME],
}

impl CaptureChain {
    fn new(in_rate: u32, apm: Arc<Apm>, far_rx: rtrb::Consumer<f32>, capture_tx: Sender<Vec<f32>>) -> Result<Self> {
        Ok(Self {
            apm,
            far_rx,
            capture_tx,
            in_resampler: Resampler::new(in_rate, APM_RATE, 10)?,
            down: Resampler::new(APM_RATE, MODEL_RATE, 10)?,
            acc: Vec::with_capacity(APM_FRAME * 4),
            far_frame: [0f32; APM_FRAME],
        })
    }

    fn push(&mut self, mono_in: &[f32]) {
        self.acc.extend(self.in_resampler.push(mono_in));
        let mut out16: Vec<f32> = Vec::new();
        while self.acc.len() >= APM_FRAME {
            // Feed every complete far-end frame that has been played since last time.
            while self.far_rx.slots() >= APM_FRAME {
                for s in self.far_frame.iter_mut() {
                    *s = self.far_rx.pop().unwrap_or(0.0);
                }
                self.apm.process_render(&self.far_frame);
            }
            let mut frame = [0f32; APM_FRAME];
            frame.copy_from_slice(&self.acc[..APM_FRAME]);
            self.acc.drain(..APM_FRAME);
            self.apm.process_capture(&mut frame);
            out16.extend(self.down.push(&frame));
        }
        if !out16.is_empty() {
            let _ = self.capture_tx.send(out16);
        }
    }
}
