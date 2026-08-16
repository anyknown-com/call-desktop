//! Ordered PCM playback sink. Segments are declared in play order, filled with `write` (any
//! sample rate — resampled to 48 kHz), closed with `end`. Playback proceeds through segments in
//! declaration order, waiting (silence) if the current one is still being filled. `priority`
//! segments go to the front (ahead of the paused/current one). The output callback pulls mono
//! 48 kHz audio with [`PlaybackSink::render`] and reports segment start/end + activity events.

use crate::resample::Resampler;
use crate::APM_RATE;
use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use voice_core::call_machine::SegmentId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkEvent {
    SegmentStarted(SegmentId),
    SegmentEnded(SegmentId),
    /// Whether audible output is currently being produced (for media ducking).
    Active(bool),
}

struct Seg {
    id: SegmentId,
    samples: VecDeque<f32>,
    ended: bool,
    started: bool,
    resampler: Option<(u32, Resampler)>,
}

struct State {
    segments: VecDeque<Seg>,
    paused: bool,
    active: bool,
}

#[derive(Clone)]
pub struct PlaybackSink {
    st: Arc<Mutex<State>>,
    events: Sender<SinkEvent>,
}

impl PlaybackSink {
    pub fn new(events: Sender<SinkEvent>) -> Self {
        Self { st: Arc::new(Mutex::new(State { segments: VecDeque::new(), paused: false, active: false })), events }
    }

    pub fn add_segment(&self, id: SegmentId, priority: bool) {
        let mut st = self.st.lock().unwrap();
        let seg = Seg { id, samples: VecDeque::new(), ended: false, started: false, resampler: None };
        if priority {
            st.segments.push_front(seg);
        } else {
            st.segments.push_back(seg);
        }
    }

    pub fn write(&self, id: SegmentId, chunk: &[f32], sample_rate: u32) {
        let mut st = self.st.lock().unwrap();
        let Some(seg) = st.segments.iter_mut().find(|s| s.id == id) else { return };
        if sample_rate == APM_RATE {
            seg.samples.extend(chunk);
            return;
        }
        if seg.resampler.as_ref().is_none_or(|(r, _)| *r != sample_rate) {
            seg.resampler = Resampler::new(sample_rate, APM_RATE, 10).ok().map(|r| (sample_rate, r));
        }
        if let Some((_, r)) = seg.resampler.as_mut() {
            seg.samples.extend(r.push(chunk));
        }
    }

    pub fn end(&self, id: SegmentId) {
        let mut st = self.st.lock().unwrap();
        if let Some(seg) = st.segments.iter_mut().find(|s| s.id == id) {
            // Flush the resampler tail with silence so the last few ms are not lost.
            if let Some((rate, r)) = seg.resampler.as_mut() {
                let pad = vec![0f32; (*rate / 100 * 2) as usize];
                seg.samples.extend(r.push(&pad));
            }
            seg.ended = true;
        }
    }

    pub fn pause(&self) {
        self.st.lock().unwrap().paused = true;
    }
    pub fn resume(&self) {
        self.st.lock().unwrap().paused = false;
    }
    /// Drop everything, including the segment currently playing, and un-pause.
    pub fn clear(&self) {
        let mut st = self.st.lock().unwrap();
        st.segments.clear();
        st.paused = false;
    }
    pub fn is_paused(&self) -> bool {
        self.st.lock().unwrap().paused
    }

    /// Fill `out` (mono, 48 kHz) from the queue. Called from the output callback.
    pub fn render(&self, out: &mut [f32]) {
        let mut st = self.st.lock().unwrap();
        let mut produced = false;
        for o in out.iter_mut() {
            *o = 0.0;
            if st.paused {
                continue;
            }
            while let Some(front) = st.segments.front_mut() {
                if let Some(s) = front.samples.pop_front() {
                    if !front.started {
                        front.started = true;
                        let _ = self.events.send(SinkEvent::SegmentStarted(front.id));
                    }
                    *o = s;
                    produced = true;
                    break;
                }
                if front.ended {
                    if !front.started {
                        // Empty segment: still report start/end so bookkeeping proceeds.
                        let _ = self.events.send(SinkEvent::SegmentStarted(front.id));
                    }
                    let id = front.id;
                    st.segments.pop_front();
                    let _ = self.events.send(SinkEvent::SegmentEnded(id));
                    continue;
                }
                break; // still being filled: wait
            }
        }
        if produced != st.active {
            st.active = produced;
            let _ = self.events.send(SinkEvent::Active(produced));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn drain(rx: &std::sync::mpsc::Receiver<SinkEvent>) -> Vec<SinkEvent> {
        rx.try_iter().collect()
    }

    #[test]
    fn plays_in_order_and_reports_events() {
        let (tx, rx) = channel();
        let s = PlaybackSink::new(tx);
        s.add_segment(SegmentId(1), false);
        s.add_segment(SegmentId(2), false);
        s.write(SegmentId(2), &[0.2; 10], 48000);
        s.end(SegmentId(2));
        let mut out = [0f32; 8];
        s.render(&mut out); // seg 1 not filled: silence, nothing starts
        assert!(out.iter().all(|x| *x == 0.0));
        assert!(drain(&rx).is_empty());
        s.write(SegmentId(1), &[0.1; 4], 48000);
        s.end(SegmentId(1));
        s.render(&mut out);
        assert_eq!(&out[..6], &[0.1, 0.1, 0.1, 0.1, 0.2, 0.2]);
        let ev = drain(&rx);
        assert_eq!(ev, vec![SinkEvent::SegmentStarted(SegmentId(1)), SinkEvent::SegmentEnded(SegmentId(1)), SinkEvent::SegmentStarted(SegmentId(2)), SinkEvent::Active(true)]);
        s.render(&mut out); // 6 remaining samples of seg 2, then it ends
        assert_eq!(drain(&rx), vec![SinkEvent::SegmentEnded(SegmentId(2))]);
        s.render(&mut out); // silence → inactive
        assert_eq!(drain(&rx), vec![SinkEvent::Active(false)]);
    }

    #[test]
    fn priority_pause_and_clear() {
        let (tx, rx) = channel();
        let s = PlaybackSink::new(tx);
        s.add_segment(SegmentId(1), false);
        s.write(SegmentId(1), &[0.1; 100], 48000);
        s.end(SegmentId(1));
        let mut out = [0f32; 4];
        s.render(&mut out);
        s.pause();
        s.render(&mut out);
        assert!(out.iter().all(|x| *x == 0.0));
        s.add_segment(SegmentId(9), true);
        s.write(SegmentId(9), &[0.9; 4], 48000);
        s.end(SegmentId(9));
        s.resume();
        s.render(&mut out);
        assert_eq!(out, [0.9; 4]);
        s.render(&mut out);
        assert_eq!(out, [0.1; 4]); // back to the main sequence
        s.clear();
        s.render(&mut out);
        assert!(out.iter().all(|x| *x == 0.0));
        let ev = drain(&rx);
        assert!(ev.contains(&SinkEvent::SegmentEnded(SegmentId(9))));
        assert!(!ev.contains(&SinkEvent::SegmentEnded(SegmentId(1)))); // cleared, never ended
    }

    #[test]
    fn resamples_writes() {
        let (tx, _rx) = channel();
        let s = PlaybackSink::new(tx);
        s.add_segment(SegmentId(1), false);
        s.write(SegmentId(1), &vec![0.5; 2400], 24000); // 100 ms
        s.end(SegmentId(1));
        let mut out = vec![0f32; 4800];
        s.render(&mut out);
        let nz = out.iter().filter(|x| x.abs() > 0.1).count();
        assert!(nz > 4000, "{nz}"); // ~100 ms at 48k, minus filter edges
    }
}
