//! Streaming mono resampler on top of rubato. Accepts arbitrary-length pushes, returns whatever
//! output is ready. Passthrough when rates match.

use rubato::{FftFixedIn, Resampler as _};

pub struct Resampler {
    inner: Option<FftFixedIn<f32>>,
    chunk_in: usize,
    pending: Vec<f32>,
    out_buf: Vec<Vec<f32>>,
}

impl Resampler {
    /// `chunk_ms` is the internal processing granularity (latency ≈ chunk_ms).
    pub fn new(in_rate: u32, out_rate: u32, chunk_ms: u32) -> anyhow::Result<Self> {
        if in_rate == out_rate {
            return Ok(Self { inner: None, chunk_in: 0, pending: vec![], out_buf: vec![] });
        }
        let chunk_in = (in_rate * chunk_ms / 1000) as usize;
        let inner = FftFixedIn::<f32>::new(in_rate as usize, out_rate as usize, chunk_in, 1, 1)?;
        let out_buf = inner.output_buffer_allocate(true);
        Ok(Self { inner: Some(inner), chunk_in, pending: Vec::with_capacity(chunk_in * 2), out_buf })
    }

    /// Feed input samples; returns output samples ready so far.
    pub fn push(&mut self, input: &[f32]) -> Vec<f32> {
        let Some(inner) = self.inner.as_mut() else { return input.to_vec() };
        self.pending.extend_from_slice(input);
        let mut out = Vec::new();
        while self.pending.len() >= self.chunk_in {
            let chunk: Vec<f32> = self.pending.drain(..self.chunk_in).collect();
            let (_, n) = inner.process_into_buffer(&[chunk.as_slice()], &mut self.out_buf, None).expect("resample");
            out.extend_from_slice(&self.out_buf[0][..n]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_and_tone_survive() {
        let mut r = Resampler::new(48000, 16000, 10).unwrap();
        // 1 s of 440 Hz at 48k, pushed in odd-sized pieces
        let input: Vec<f32> = (0..48000).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin()).collect();
        let mut out = vec![];
        for piece in input.chunks(333) {
            out.extend(r.push(piece));
        }
        // FFT resampler has some latency, so slightly fewer than 16000 samples so far.
        assert!(out.len() > 15000 && out.len() <= 16000, "{}", out.len());
        // Zero crossings ≈ 2 * 440 per second on the tail (skip warm-up).
        let tail = &out[out.len() - 8000..];
        let zc = tail.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        assert!((zc as i64 - 440).abs() < 12, "zc {zc}");
    }

    #[test]
    fn passthrough() {
        let mut r = Resampler::new(16000, 16000, 10).unwrap();
        assert_eq!(r.push(&[1.0, 2.0]), vec![1.0, 2.0]);
    }
}
