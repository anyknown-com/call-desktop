mod common;

use common::{golden_dir, read_wav};
use voice_ml::vad::{FRAME_SAMPLES, SAMPLE_RATE};
use voice_ml::{models, SileroVad, VadConfig, VadDetector, VadEvent};

fn run(clip: &str) -> (Vec<VadEvent>, usize) {
    let pcm = read_wav(&golden_dir().join(clip));
    let mut det = VadDetector::new(SileroVad::new(models::silero_vad_v5()).unwrap(), VadConfig::default());
    let mut events = Vec::new();
    for frame in pcm.chunks_exact(FRAME_SAMPLES) {
        events.extend(det.process(frame).unwrap().1);
    }
    // Trailing silence so the redemption window can elapse after the clip's last speech frame.
    let silence = vec![0f32; FRAME_SAMPLES];
    for _ in 0..(SAMPLE_RATE as usize / FRAME_SAMPLES) {
        events.extend(det.process(&silence).unwrap().1);
    }
    (events, pcm.len())
}

#[test]
fn speech_clip_yields_one_segment() {
    let (events, clip_len) = run("en_a.wav");
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            VadEvent::Start => "start",
            VadEvent::RealStart => "real_start",
            VadEvent::Misfire => "misfire",
            VadEvent::End { .. } => "end",
        })
        .collect();
    assert_eq!(kinds, ["start", "real_start", "end"], "{kinds:?}");
    let VadEvent::End { audio } = &events[2] else { unreachable!() };
    // End audio = pre-speech pad + speech + redemption tail. The clip is ~3.5 s of speech; the
    // segment must cover most of it plus (pad + redemption) frames of context.
    let cfg = VadConfig::default();
    let context = (cfg.pre_speech_pad_frames() + cfg.redemption_frames()) * FRAME_SAMPLES;
    let full_frames = clip_len / FRAME_SAMPLES * FRAME_SAMPLES;
    assert!(audio.len() <= full_frames + context, "audio {} > clip {} + context {}", audio.len(), full_frames, context);
    assert!(audio.len() >= full_frames - 3 * FRAME_SAMPLES, "audio {} much shorter than clip {}", audio.len(), full_frames);
}

#[test]
fn silence_yields_nothing() {
    let (events, _) = run("silence_2s.wav");
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn frame_params_match_vad_web() {
    let cfg = VadConfig::default();
    assert_eq!(cfg.redemption_frames(), 21); // floor(700 / 32)
    assert_eq!(cfg.pre_speech_pad_frames(), 9); // floor(300 / 32)
    assert_eq!(cfg.min_speech_frames(), 7); // floor(250 / 32)
}

#[test]
fn model_io_matches_vad_web() {
    let mut vad = SileroVad::new(models::silero_vad_v5()).unwrap();
    let silence = vec![0f32; FRAME_SAMPLES];
    let p = vad.process(&silence).unwrap();
    assert!((0.0..0.1).contains(&p), "silence prob {p}");
    assert!(vad.process(&vec![0f32; 100]).is_err());
}
