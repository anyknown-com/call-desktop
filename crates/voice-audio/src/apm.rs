//! WebRTC audio processing (AEC3 + NS + AGC2 + HPF) at 48 kHz mono, 10 ms frames.
//! The `Processor` is thread-safe: the output callback feeds render frames, the input callback
//! feeds capture frames.

use crate::{APM_FRAME, APM_RATE};
use webrtc_audio_processing::Processor;
use webrtc_audio_processing::config::{
    AdaptiveDigital, Config, EchoCanceller, GainController, GainController2, HighPassFilter, NoiseSuppression,
    NoiseSuppressionLevel,
};

#[derive(Debug, Clone, Copy)]
pub struct ApmOptions {
    pub noise_suppression: bool,
    pub agc: bool,
}

impl Default for ApmOptions {
    fn default() -> Self {
        Self { noise_suppression: true, agc: true }
    }
}

pub struct Apm {
    proc_: Processor,
}

impl Apm {
    pub fn new(opts: ApmOptions) -> anyhow::Result<Self> {
        let proc_ = Processor::new(APM_RATE)?;
        debug_assert_eq!(proc_.num_samples_per_frame(), APM_FRAME);
        let config = Config {
            high_pass_filter: Some(HighPassFilter::default()),
            // AEC is always on: barge-in depends on it.
            echo_canceller: Some(EchoCanceller::Full { stream_delay_ms: None }),
            noise_suppression: opts
                .noise_suppression
                .then_some(NoiseSuppression { level: NoiseSuppressionLevel::Moderate, analyze_linear_aec_output: false }),
            gain_controller: opts.agc.then_some(GainController::GainController2(GainController2 {
                input_volume_controller_enabled: false,
                adaptive_digital: Some(AdaptiveDigital::default()),
                fixed_digital: Default::default(),
            })),
            ..Default::default()
        };
        proc_.set_config(config);
        Ok(Self { proc_ })
    }

    /// What is about to be played (48 kHz mono, exactly 480 samples).
    pub fn process_render(&self, frame: &[f32; APM_FRAME]) {
        let mut f = *frame;
        let _ = self.proc_.process_render_frame([&mut f[..]]);
    }

    /// Mic audio in place (48 kHz mono, exactly 480 samples).
    pub fn process_capture(&self, frame: &mut [f32; APM_FRAME]) {
        let _ = self.proc_.process_capture_frame([&mut frame[..]]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancels_a_pure_echo() {
        // Render a tone; capture = the same tone (perfect echo). After convergence the capture
        // output should be much quieter than the input.
        let apm = Apm::new(ApmOptions { noise_suppression: false, agc: false }).unwrap();
        let mut in_rms = 0f64;
        let mut out_rms = 0f64;
        for n in 0..300 {
            let mut render = [0f32; APM_FRAME];
            for (i, s) in render.iter_mut().enumerate() {
                let t = (n * APM_FRAME + i) as f32 / APM_RATE as f32;
                *s = 0.3 * (2.0 * std::f32::consts::PI * 700.0 * t).sin() * (1.0 + 0.5 * (2.0 * std::f32::consts::PI * 3.0 * t).sin());
            }
            apm.process_render(&render);
            let mut cap = render;
            apm.process_capture(&mut cap);
            if n >= 200 {
                in_rms += render.iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
                out_rms += cap.iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
            }
        }
        let erle_db = 10.0 * (in_rms / out_rms.max(1e-12)).log10();
        assert!(erle_db > 15.0, "ERLE only {erle_db:.1} dB");
    }
}
