//! Top-level view: sidebar navigation + pages. Settings/keys live in a GPUI global.

use crate::{call_view::CallView, history_view::HistoryView, settings_view::SettingsView, voice_view::VoiceView};
use gpui::*;
use gpui_component::{button::*, h_flex, v_flex, ActiveTheme};
use voice_runtime::settings::{self, Keys, Settings};

pub struct AppState {
    pub settings: Settings,
    pub keys: Keys,
}
impl Global for AppState {}

pub fn init(cx: &mut App) {
    cx.set_global(AppState { settings: settings::load(), keys: Keys::load() });
    cx.bind_keys([
        KeyBinding::new("space", crate::call_view::Interrupt, Some("CallView")),
        KeyBinding::new("escape", crate::call_view::Interrupt, Some("CallView")),
        KeyBinding::new("cmd-enter", crate::call_view::ToggleCall, Some("CallView")),
    ]);
}

pub fn save_settings(cx: &mut App) {
    let s = cx.global::<AppState>().settings.clone();
    if let Err(e) = settings::save(&s) {
        tracing::error!("save settings: {e}");
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Call,
    Settings,
    Voice,
    History,
}

pub struct AppView {
    page: Page,
    call: Entity<CallView>,
    settings: Entity<SettingsView>,
    voice: Entity<VoiceView>,
    history: Entity<HistoryView>,
}

impl AppView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Dev aid: VOICE_START_PAGE=settings|voice|history opens on that page.
        let page = match std::env::var("VOICE_START_PAGE").as_deref() {
            Ok("settings") => Page::Settings,
            Ok("voice") => Page::Voice,
            Ok("history") => Page::History,
            _ => Page::Call,
        };
        Self {
            page,
            call: cx.new(|cx| CallView::new(window, cx)),
            settings: cx.new(|cx| SettingsView::new(window, cx)),
            voice: cx.new(|cx| VoiceView::new(window, cx)),
            history: cx.new(|cx| HistoryView::new(window, cx)),
        }
    }

    fn nav(&self, page: Page, label: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.page == page;
        let mut b = Button::new(label).label(label).w_full();
        b = if active { b.primary() } else { b.ghost() };
        b.on_click(cx.listener(move |this, _, _, cx| {
            this.page = page;
            if page == Page::History {
                this.history.update(cx, |h, cx| h.refresh(cx));
            }
            cx.notify();
        }))
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content: AnyElement = match self.page {
            Page::Call => self.call.clone().into_any_element(),
            Page::Settings => self.settings.clone().into_any_element(),
            Page::Voice => self.voice.clone().into_any_element(),
            Page::History => self.history.clone().into_any_element(),
        };
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                v_flex()
                    .w(px(180.))
                    .h_full()
                    .p_3()
                    .gap_1()
                    .bg(cx.theme().sidebar)
                    .border_r_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(div().px_2().pb_3().pt_1().text_lg().font_weight(FontWeight::BOLD).child("voice"))
                    .child(self.nav(Page::Call, "Call", cx))
                    .child(self.nav(Page::Settings, "Settings", cx))
                    .child(self.nav(Page::Voice, "My voice", cx))
                    .child(self.nav(Page::History, "History", cx))
                    .child(div().flex_1())
                    .child(div().px_2().text_xs().text_color(cx.theme().muted_foreground).child(format!("v{}", env!("CARGO_PKG_VERSION")))),
            )
            .child(div().flex_1().h_full().min_w_0().child(content))
    }
}
