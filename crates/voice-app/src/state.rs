//! App-wide state: the loaded settings and API keys, as a GPUI global.

use gpui::{App, Global};
use voice_runtime::keys::Keys;
use voice_runtime::settings::{self, Settings};

pub struct AppState {
    pub settings: Settings,
    pub keys: Keys,
}
impl Global for AppState {}

pub fn init(cx: &mut App) {
    cx.set_global(AppState { settings: settings::load(), keys: Keys::load() });
}

pub fn save_settings(cx: &mut App) {
    let s = cx.global::<AppState>().settings.clone();
    if let Err(e) = settings::save(&s) {
        tracing::error!("save settings: {e}");
    }
}
