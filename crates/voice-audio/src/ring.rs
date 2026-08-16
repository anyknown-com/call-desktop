//! Bounded PCM ring buffer (4 s in Media mode). Positions are expressed in stream time: ms since
//! the first written sample, derived from sample count. Port of pcm-ring-buffer.ts.

pub struct PcmRingBuffer {
    buf: Vec<f32>,
    sample_rate: u32,
    /// Total samples ever written.
    total: usize,
}

impl PcmRingBuffer {
    pub fn new(capacity_ms: f64, sample_rate: u32) -> Self {
        let cap = ((capacity_ms / 1000.0) * sample_rate as f64).ceil() as usize;
        Self { buf: vec![0.0; cap], sample_rate, total: 0 }
    }

    pub fn write(&mut self, chunk: &[f32]) {
        let cap = self.buf.len();
        // Only the last `cap` samples of the chunk can survive.
        let src = if chunk.len() > cap { &chunk[chunk.len() - cap..] } else { chunk };
        if chunk.len() > cap {
            self.total += chunk.len() - cap;
        }
        let pos = self.total % cap;
        let first = src.len().min(cap - pos);
        self.buf[pos..pos + first].copy_from_slice(&src[..first]);
        if first < src.len() {
            self.buf[..src.len() - first].copy_from_slice(&src[first..]);
        }
        self.total += src.len();
    }

    /// Total stream time written, in ms.
    pub fn end_ms(&self) -> f64 {
        self.total as f64 / self.sample_rate as f64 * 1000.0
    }

    /// Earliest stream time still retained, in ms.
    pub fn start_ms(&self) -> f64 {
        self.total.saturating_sub(self.buf.len()) as f64 / self.sample_rate as f64 * 1000.0
    }

    /// Copy out [from_ms, to_ms) in stream time, clamped to what is retained.
    pub fn slice(&self, from_ms: f64, to_ms: f64) -> Vec<f32> {
        let cap = self.buf.len();
        let lo = self.total.saturating_sub(cap);
        let from = lo.max(((from_ms / 1000.0) * self.sample_rate as f64).floor().max(0.0) as usize);
        let to = self.total.min(((to_ms / 1000.0) * self.sample_rate as f64).floor().max(0.0) as usize);
        if to <= from {
            return vec![];
        }
        (from..to).map(|i| self.buf[i % cap]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_wrap_and_slice_in_stream_time() {
        let mut r = PcmRingBuffer::new(1000.0, 1000); // 1000 samples = 1 s
        r.write(&(0..600).map(|i| i as f32).collect::<Vec<_>>());
        assert_eq!(r.end_ms(), 600.0);
        assert_eq!(r.start_ms(), 0.0);
        r.write(&(600..1500).map(|i| i as f32).collect::<Vec<_>>());
        assert_eq!(r.end_ms(), 1500.0);
        assert_eq!(r.start_ms(), 500.0);
        let s = r.slice(490.0, 505.0); // clamped to retained start
        assert_eq!(s, (500..505).map(|i| i as f32).collect::<Vec<_>>());
        let s = r.slice(1490.0, 2000.0);
        assert_eq!(s, (1490..1500).map(|i| i as f32).collect::<Vec<_>>());
        assert!(r.slice(300.0, 400.0).is_empty());
    }

    #[test]
    fn oversized_chunk_keeps_tail() {
        let mut r = PcmRingBuffer::new(10.0, 1000);
        r.write(&(0..25).map(|i| i as f32).collect::<Vec<_>>());
        assert_eq!(r.slice(0.0, 100.0), (15..25).map(|i| i as f32).collect::<Vec<_>>());
        assert_eq!(r.start_ms(), 15.0);
    }
}
