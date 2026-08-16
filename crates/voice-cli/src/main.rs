//! `voice` — headless CLI for the voice desktop app. Phase 1 deliverable and e2e harness.

mod enroll;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};
use voice_core::call_machine::{CallStatus, Role, Turn, TurnKind};
use voice_runtime::settings::{self, DuckMode, Keys, Settings};
use voice_runtime::{Runtime, RuntimeCommand, RuntimeEvent, RuntimeOptions};

#[derive(Parser)]
#[command(name = "voice", about = "BYOK voice call with an LLM — headless CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start a call. Type `i`⏎ to interrupt, `q`⏎ (or Ctrl-C) to hang up.
    Call {
        /// Media mode: only your enrolled voice interrupts (needs `voice enroll`).
        #[arg(long)]
        media: bool,
        /// Mute other apps' audio while the assistant speaks.
        #[arg(long)]
        duck: bool,
        /// Override the input device (substring match).
        #[arg(long)]
        input: Option<String>,
        /// Override the output device (substring match).
        #[arg(long)]
        output: Option<String>,
        /// Offline mock providers (no API keys; tone TTS, canned STT/LLM) — audio-path smoke test.
        #[arg(long)]
        mock: bool,
        /// Use a WAV file as the microphone (test harness).
        #[arg(long)]
        mic_wav: Option<std::path::PathBuf>,
        /// Hang up automatically after this many seconds (test harness).
        #[arg(long)]
        seconds: Option<u64>,
    },
    /// Record six ~3 s clips + one held-out clip and build your speaker profile.
    Enroll,
    /// List audio devices.
    Devices,
    /// Store an API key (prompts for the value; kept in the app's private keys.json).
    Keys {
        #[command(subcommand)]
        cmd: KeysCmd,
    },
    /// Print the settings file location and current settings.
    Settings,
    /// Mute other apps' audio for a few seconds, then restore (verifies ducking works).
    DuckTest {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
}

#[derive(Subcommand)]
enum KeysCmd {
    /// Set a key: openai | anthropic | elevenlabs
    Set { account: String },
    /// Show which keys are present.
    Status,
}

fn models_dir() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("VOICE_MODELS_DIR") {
        return d.into();
    }
    // Next to the executable (packaged), else the workspace `models/` dir (dev).
    if let Ok(exe) = std::env::current_exe() {
        let p = exe.parent().unwrap().join("models");
        if p.exists() {
            return p;
        }
    }
    voice_ml::models::default_dir()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("voice=info".parse()?)).with_writer(std::io::stderr).init();
    voice_os::recover_after_crash();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Devices => {
            let (ins, outs) = voice_audio::engine::list_devices()?;
            println!("Input devices:");
            for d in ins {
                println!("  {d}");
            }
            println!("Output devices:");
            for d in outs {
                println!("  {d}");
            }
        }
        Cmd::Keys { cmd } => match cmd {
            KeysCmd::Set { account } => {
                if !["openai", "anthropic", "elevenlabs"].contains(&account.as_str()) {
                    return Err(anyhow!("account must be openai | anthropic | elevenlabs"));
                }
                let v = rpassword::prompt_password(format!("{account} API key (input hidden): "))?;
                Keys::store(&account, v.trim())?;
                println!("stored in {}", Keys::path().map(|p| p.display().to_string()).unwrap_or_default());
            }
            KeysCmd::Status => {
                let k = Keys::load();
                for (n, v) in [("openai", &k.openai), ("anthropic", &k.anthropic), ("elevenlabs", &k.elevenlabs)] {
                    println!("{n:11} {}", if v.is_empty() { "—" } else { "set" });
                }
            }
        },
        Cmd::Settings => {
            let s = settings::load();
            println!("{}", settings::settings_path().map(|p| p.display().to_string()).unwrap_or_default());
            println!("{}", serde_json::to_string_pretty(&s)?);
            if !settings::settings_path().is_some_and(|p| p.exists()) {
                settings::save(&s)?;
                println!("(defaults written)");
            }
        }
        Cmd::DuckTest { seconds } => {
            let mut d = voice_os::create_ducker()?;
            println!("backend: {}", d.backend_name());
            d.duck(voice_os::DuckMode::Mute)?;
            println!("other apps muted for {seconds}s…");
            tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
            d.restore()?;
            println!("restored");
        }
        Cmd::Enroll => enroll::run(models_dir()).await?,
        Cmd::Call { media, duck, input, output, mock, mic_wav, seconds } => call(media, duck, input, output, mock, mic_wav, seconds).await?,
    }
    Ok(())
}

async fn call(media: bool, duck: bool, input: Option<String>, output: Option<String>, mock: bool, mic_wav: Option<std::path::PathBuf>, seconds: Option<u64>) -> Result<()> {
    let mut s: Settings = settings::load();
    if media {
        s.media_mode = true;
    }
    if duck {
        s.audio.duck = DuckMode::Mute;
    }
    if input.is_some() {
        s.audio.input_device = input;
    }
    if output.is_some() {
        s.audio.output_device = output;
    }
    let keys = Keys::load();
    let profile = if s.media_mode {
        Some(voice_runtime::enroll::load_profile().ok_or_else(|| anyhow!("no speaker profile — run `voice enroll`"))?)
    } else {
        None
    };
    let mut rt = Runtime::start(RuntimeOptions { settings: s, keys, profile, models_dir: models_dir(), mock, input_wav: mic_wav })?;
    rt.commands.send(RuntimeCommand::Start)?;
    eprintln!("● call started — speak. `i`⏎ interrupt, `q`⏎ hang up.");

    let cmds = rt.commands.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match line.trim() {
                "i" => {
                    let _ = cmds.send(RuntimeCommand::Interrupt);
                }
                "q" => {
                    let _ = cmds.send(RuntimeCommand::Hangup);
                    let _ = cmds.send(RuntimeCommand::Shutdown);
                    break;
                }
                _ => {}
            }
        }
    });
    if let Some(secs) = seconds {
        let cmds = rt.commands.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            let _ = cmds.send(RuntimeCommand::Hangup);
            let _ = cmds.send(RuntimeCommand::Shutdown);
        });
    }
    let cmds = rt.commands.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = cmds.send(RuntimeCommand::Hangup);
        let _ = cmds.send(RuntimeCommand::Shutdown);
    });

    let mut printed: Vec<(voice_core::call_machine::TurnId, String)> = vec![];
    let mut last_status: Option<CallStatus> = None;
    while let Some(ev) = rt.events.recv().await {
        match ev {
            RuntimeEvent::State(st) => {
                if last_status != Some(st.status) {
                    last_status = Some(st.status);
                    eprintln!("[{:?}]", st.status);
                }
                print_turns(&st.turns, &mut printed);
                if !st.active && last_status == Some(CallStatus::Idle) {
                    // hung up
                }
            }
            RuntimeEvent::Hint(h) => eprintln!("· {h}"),
            RuntimeEvent::Error(e) => eprintln!("! {e}"),
            RuntimeEvent::Ducked(d) => eprintln!("· other audio {}", if d { "muted" } else { "restored" }),
            RuntimeEvent::GateState(g) => tracing::debug!(?g, "gate"),
            RuntimeEvent::Saved(p) => eprintln!("· transcript saved: {}", p.display()),
            RuntimeEvent::Level(_) => {}
        }
    }
    rt.join().await;
    // The stdin reader is a blocking task; don't let it keep the runtime alive.
    std::process::exit(0);
}

/// Print turns as they become final (assistant turns stream, so print those once final).
fn print_turns(turns: &[Turn], printed: &mut Vec<(voice_core::call_machine::TurnId, String)>) {
    let mut out = std::io::stdout().lock();
    for t in turns {
        if !t.is_final {
            continue;
        }
        if printed.iter().any(|(id, _)| *id == t.id) {
            continue;
        }
        printed.push((t.id, t.text.clone()));
        let who = match (&t.role, t.kind) {
            (Role::User, Some(TurnKind::Interjection)) => "you (aside)",
            (Role::User, _) => "you",
            (Role::Assistant, Some(TurnKind::Reaction)) => "ai (reaction)",
            (Role::Assistant, _) => "ai",
        };
        let flag = if t.interrupted { " ⏹" } else { "" };
        let _ = writeln!(out, "{who}{flag}: {}", t.text);
    }
}
