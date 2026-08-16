//! Interactive enrollment on the terminal (logic lives in voice_runtime::enroll).

use anyhow::{anyhow, Result};
use std::io::Write;
use std::path::PathBuf;
use voice_runtime::enroll::{describe_rejection, Enroller, FinishError, CLIP_SECS};

pub async fn run(models_dir: PathBuf) -> Result<()> {
    let s = voice_runtime::settings::load();
    let mut e = Enroller::new(&s, &models_dir)?;
    println!("Voice enrollment: {} clips of ~3 s each, then one extra check clip.", e.needed());
    println!("Speak naturally (any language), e.g. read a sentence from a book. Press Enter to record each clip.\n");
    while !e.complete() {
        wait_enter(&format!("Clip {}/{} — press Enter, then speak for 3 seconds", e.accepted() + 1, e.needed()))?;
        let pcm = record(&mut e);
        match e.submit_clip(&pcm)? {
            Ok(_) => println!("  ✓ accepted\n"),
            Err(r) => println!("  ✗ {}\n", describe_rejection(r)),
        }
    }
    loop {
        wait_enter("Check clip — press Enter, then speak for 3 seconds")?;
        let pcm = record(&mut e);
        match e.finish(&pcm)? {
            Ok(profile) => {
                println!(
                    "✓ profile saved (held-out score {:.3}, θ_high {:.2}, full-turn {:.2}). Media mode is now available: `voice call --media`.",
                    profile.held_out_score, profile.thresholds.streaming_high, profile.thresholds.full_turn
                );
                return Ok(());
            }
            Err(FinishError::Clip(r)) => println!("  ✗ {}\n", describe_rejection(r)),
            Err(FinishError::HeldOutBelowThreshold { held_out_score }) => {
                return Err(anyhow!(
                    "held-out clip scored {held_out_score:.3}, below the required margin — media mode would be unreliable with this mic/room. Try again closer to the mic, in a quieter spot."
                ))
            }
        }
    }
}

fn wait_enter(prompt: &str) -> Result<()> {
    print!("{prompt} ");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(())
}

fn record(e: &mut Enroller) -> Vec<f32> {
    print!("  ● recording… ");
    let _ = std::io::stdout().flush();
    let pcm = e.record(CLIP_SECS);
    println!("done");
    pcm
}
