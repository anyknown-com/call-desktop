//! Exact Kaldi log-mel fbank matching the pinned CAM++ model's frontend (verified cosine 1.0
//! against sherpa-onnx's SpeakerEmbeddingExtractor):
//!
//!   16 kHz float PCM in [-1, 1] (do NOT scale by 32768)
//!   frame 25 ms / shift 10 ms, snip_edges = false, dither = 0
//!   window = povey, remove_dc_offset = true, preemph = 0.97
//!   80 mel bins, low 20 Hz, high 7600 Hz, use_energy = false
//!   post: subtract per-dim mean over all frames (utterance CMN)
//!
//! Golden fixtures: fixtures/speaker/golden (max abs err < 1e-3 where there is signal).
//! Port of voice/src/core/speaker/kaldi-fbank.ts — keep numerically identical.

use std::sync::OnceLock;

/// Bump when any numeric behavior of this frontend changes; stored in profiles.
pub const FRONTEND_VERSION: u32 = 1;
pub const SAMPLE_RATE: u32 = 16_000;
pub const NUM_BINS: usize = 80;

const FRAME_LEN: usize = 400; // 25 ms
const FRAME_SHIFT: usize = 160; // 10 ms
const PADDED: usize = 512; // next power of two
const PREEMPH: f64 = 0.97;
const LOW_FREQ: f64 = 20.0;
const HIGH_FREQ: f64 = 7600.0;
const FLT_EPSILON: f64 = 1.1920928955078125e-7;

fn mel_scale(freq: f64) -> f64 {
    1127.0 * (1.0 + freq / 700.0).ln()
}

struct MelBank {
    first: usize,
    weights: Vec<f64>,
}

struct Tables {
    povey: [f64; FRAME_LEN],
    banks: Vec<MelBank>,
    tw_cos: [f64; PADDED / 2],
    tw_sin: [f64; PADDED / 2],
    bitrev: [u16; PADDED],
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        // Povey window: (0.5 - 0.5 cos(2πn/(N-1)))^0.85
        let mut povey = [0f64; FRAME_LEN];
        for (i, w) in povey.iter_mut().enumerate() {
            *w = (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (FRAME_LEN - 1) as f64).cos()).powf(0.85);
        }
        // Sparse mel filterbank: per bin, first FFT index + triangular weights.
        let num_fft_bins = PADDED / 2; // Kaldi excludes the nyquist bin from mel banks
        let fft_bin_width = SAMPLE_RATE as f64 / PADDED as f64;
        let mel_low = mel_scale(LOW_FREQ);
        let mel_high = mel_scale(HIGH_FREQ);
        let mel_delta = (mel_high - mel_low) / (NUM_BINS + 1) as f64;
        let mut banks = Vec::with_capacity(NUM_BINS);
        for b in 0..NUM_BINS {
            let left = mel_low + b as f64 * mel_delta;
            let center = mel_low + (b + 1) as f64 * mel_delta;
            let right = mel_low + (b + 2) as f64 * mel_delta;
            let mut first: Option<usize> = None;
            let mut weights = Vec::new();
            for i in 0..num_fft_bins {
                let mel = mel_scale(fft_bin_width * i as f64);
                if mel > left && mel < right {
                    let w = if mel <= center { (mel - left) / (center - left) } else { (right - mel) / (right - center) };
                    if first.is_none() {
                        first = Some(i);
                    }
                    weights.push(w);
                } else if first.is_some() {
                    break;
                }
            }
            banks.push(MelBank { first: first.unwrap_or(0), weights });
        }
        let mut tw_cos = [0f64; PADDED / 2];
        let mut tw_sin = [0f64; PADDED / 2];
        for i in 0..PADDED / 2 {
            let a = -2.0 * std::f64::consts::PI * i as f64 / PADDED as f64;
            tw_cos[i] = a.cos();
            tw_sin[i] = a.sin();
        }
        let bits = PADDED.trailing_zeros();
        let mut bitrev = [0u16; PADDED];
        for (i, r) in bitrev.iter_mut().enumerate() {
            let mut v = 0usize;
            for b in 0..bits {
                v |= ((i >> b) & 1) << (bits - 1 - b);
            }
            *r = v as u16;
        }
        Tables { povey, banks, tw_cos, tw_sin, bitrev }
    })
}

/// In-place iterative radix-2 FFT on separate re/im arrays of length 512.
fn fft(t: &Tables, re: &mut [f64; PADDED], im: &mut [f64; PADDED]) {
    for i in 0..PADDED {
        let j = t.bitrev[i] as usize;
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut size = 2;
    while size <= PADDED {
        let half = size >> 1;
        let step = PADDED / size;
        let mut base = 0;
        while base < PADDED {
            let mut tw = 0;
            for k in 0..half {
                let wr = t.tw_cos[tw];
                let wi = t.tw_sin[tw];
                let i0 = base + k;
                let i1 = i0 + half;
                let tr = re[i1] * wr - im[i1] * wi;
                let ti = re[i1] * wi + im[i1] * wr;
                re[i1] = re[i0] - tr;
                im[i1] = im[i0] - ti;
                re[i0] += tr;
                im[i0] += ti;
                tw += step;
            }
            base += size;
        }
        size <<= 1;
    }
}

/// Kaldi frame count with snip_edges = false.
pub fn num_frames(num_samples: usize) -> usize {
    (num_samples + FRAME_SHIFT / 2) / FRAME_SHIFT
}

/// Samples of raw PCM needed for `n` frames to exist.
pub fn samples_for_frames(n: usize) -> usize {
    n * FRAME_SHIFT - FRAME_SHIFT / 2
}

/// Log-mel fbank for a full utterance. Returns row-major `[T, 80]` BEFORE mean subtraction
/// (matches golden `.fbank.f32`) together with `T`.
pub fn compute_fbank(pcm: &[f32]) -> (Vec<f32>, usize) {
    let t = tables();
    let n = pcm.len() as isize;
    let frames = num_frames(pcm.len());
    let mut out = vec![0f32; frames * NUM_BINS];
    let mut re = [0f64; PADDED];
    let mut im = [0f64; PADDED];
    let mut win = [0f64; FRAME_LEN];

    for f in 0..frames {
        // snip_edges=false: frame is centered; out-of-range samples reflect off the edges.
        let start = (f * FRAME_SHIFT + FRAME_SHIFT / 2) as isize - (FRAME_LEN / 2) as isize;
        for (k, w) in win.iter_mut().enumerate() {
            let mut s = start + k as isize;
            while s < 0 || s >= n {
                s = if s < 0 { -s - 1 } else { 2 * n - s - 1 };
            }
            *w = pcm[s as usize] as f64;
        }
        // remove_dc_offset
        let mean = win.iter().sum::<f64>() / FRAME_LEN as f64;
        for w in win.iter_mut() {
            *w -= mean;
        }
        // pre-emphasis (in reverse; first sample against itself, as Kaldi does)
        for k in (1..FRAME_LEN).rev() {
            win[k] -= PREEMPH * win[k - 1];
        }
        win[0] -= PREEMPH * win[0];
        // povey window, zero-pad, FFT
        for (k, w) in win.iter().enumerate() {
            re[k] = w * t.povey[k];
        }
        re[FRAME_LEN..].fill(0.0);
        im.fill(0.0);
        fft(t, &mut re, &mut im);
        // mel energies over power spectrum bins [0, 256)
        let row = f * NUM_BINS;
        for (b, bank) in t.banks.iter().enumerate() {
            let mut e = 0f64;
            for (i, w) in bank.weights.iter().enumerate() {
                let bin = bank.first + i;
                e += w * (re[bin] * re[bin] + im[bin] * im[bin]);
            }
            out[row + b] = e.max(FLT_EPSILON).ln() as f32;
        }
    }
    (out, frames)
}

/// Per-utterance mean subtraction (the model's feature_normalize_type=global-mean).
pub fn mean_subtract(frames: &[f32], t: usize) -> Vec<f32> {
    let mut mean = [0f64; NUM_BINS];
    for f in 0..t {
        for b in 0..NUM_BINS {
            mean[b] += frames[f * NUM_BINS + b] as f64;
        }
    }
    for m in mean.iter_mut() {
        *m /= t as f64;
    }
    let mut out = vec![0f32; frames.len()];
    for f in 0..t {
        for b in 0..NUM_BINS {
            out[f * NUM_BINS + b] = (frames[f * NUM_BINS + b] as f64 - mean[b]) as f32;
        }
    }
    out
}

/// Full frontend: PCM in [-1,1] → model-ready mean-subtracted features `[T, 80]`.
pub fn compute_features(pcm: &[f32]) -> (Vec<f32>, usize) {
    let (frames, t) = compute_fbank(pcm);
    (mean_subtract(&frames, t), t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_accounting_matches_snip_edges_false() {
        assert_eq!(num_frames(16000), 100);
        assert_eq!(num_frames(16123), 101);
        assert!(samples_for_frames(num_frames(16000)) <= 16000);
    }
}
