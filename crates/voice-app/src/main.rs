//! voice-app — GPUI desktop front-end over `voice_runtime`.

mod app;
mod call_view;
mod history_view;
mod palette;
mod settings_view;
mod voice_view;

use gpui::*;
use gpui_component::Root;
use std::sync::OnceLock;

/// The tokio runtime that hosts `voice_runtime` (network + timers). GPUI has its own executor;
/// tokio channels bridge the two.
pub static TOKIO: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

pub fn tokio() -> &'static tokio::runtime::Runtime {
    TOKIO.get().expect("tokio runtime")
}

pub fn models_dir() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("VOICE_MODELS_DIR") {
        return d.into();
    }
    if let Ok(exe) = std::env::current_exe() {
        // Bundled: Contents/MacOS/voice-app → Contents/Resources/models
        for cand in [exe.parent().map(|p| p.join("models")), exe.parent().and_then(|p| p.parent()).map(|p| p.join("Resources/models"))].into_iter().flatten() {
            if cand.exists() {
                return cand;
            }
        }
    }
    voice_ml::models::default_dir()
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("voice=info".parse().unwrap()))
        .with_writer(std::io::stderr)
        .init();
    voice_os::recover_after_crash();
    TOKIO.set(tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(2).build().expect("tokio")).ok();

    let application = Application::new();
    application.run(move |cx| {
        gpui_component::init(cx);
        app::init(cx);
        cx.spawn(async move |cx| {
            let bounds = cx.update(|cx| Bounds::centered(None, size(px(1040.), px(720.)), cx))?;
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions { title: Some("voice".into()), ..Default::default() }),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(|cx| app::AppView::new(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )?;
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });
}
