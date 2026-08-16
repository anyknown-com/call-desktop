//! Mute every other app for 5 seconds, then restore. Run with music playing:
//! `cargo run -p voice-os --example duck_demo`

use std::time::Duration;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("voice_os=debug")
        .init();
    voice_os::recover_after_crash();
    let mut ducker = voice_os::create_ducker().expect("no ducker for this OS");
    println!("backend: {}", ducker.backend_name());
    match ducker.duck(voice_os::DuckMode::Mute) {
        Ok(()) => println!("ducked (mute) for 5s..."),
        Err(e) => {
            println!("duck failed: {e}");
            return;
        }
    }
    std::thread::sleep(Duration::from_secs(5));
    ducker.restore().expect("restore");
    println!("restored (ducked = {})", ducker.is_ducked());
}
