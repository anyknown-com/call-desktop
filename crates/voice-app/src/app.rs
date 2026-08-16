//! Top-level view: a slim top bar (wordmark + tabs) over the current page.

use crate::palette::{c, DISPLAY_FONT, HAIRLINE, INK, IVORY, IVORY_DIM, MUTED, PANEL};
use crate::{call_view::CallView, history_view::HistoryView, settings_view::SettingsView, voice_view::VoiceView};
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{h_flex, v_flex};
use voice_runtime::settings::{self, Keys, Settings};

pub struct AppState {
    pub settings: Settings,
    pub keys: Keys,
}
impl Global for AppState {}

actions!(voice_app, [Quit]);

pub fn init(cx: &mut App) {
    cx.set_global(AppState { settings: settings::load(), keys: Keys::load() });
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("space", crate::call_view::Interrupt, Some("CallView")),
        KeyBinding::new("escape", crate::call_view::Interrupt, Some("CallView")),
        KeyBinding::new("cmd-enter", crate::call_view::ToggleCall, Some("CallView")),
    ]);
    cx.set_menus(vec![Menu {
        name: "voice".into(),
        items: vec![MenuItem::action("Quit voice", Quit)],
    }]);
    cx.on_window_closed(|cx| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    cx.activate(true);
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

    fn tab(&self, page: Page, label: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.page == page;
        div()
            .id(label)
            .px_3()
            .py_1()
            .rounded_full()
            .text_sm()
            .cursor_pointer()
            .text_color(if active { c(IVORY) } else { c(MUTED) })
            .when(active, |d| d.bg(c(PANEL)))
            .hover(|d| d.text_color(c(IVORY_DIM)))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
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
        v_flex()
            .size_full()
            .bg(c(INK))
            .text_color(c(IVORY))
            .font_family(".SystemUIFont")
            .child(
                h_flex()
                    .h(px(44.))
                    .px_4()
                    .items_center()
                    .border_b_1()
                    .border_color(c(HAIRLINE))
                    .child(div().font_family(DISPLAY_FONT).italic().text_lg().text_color(c(IVORY)).child("voice"))
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .gap_1()
                            .child(self.tab(Page::Call, "Call", cx))
                            .child(self.tab(Page::Voice, "My voice", cx))
                            .child(self.tab(Page::History, "History", cx))
                            .child(self.tab(Page::Settings, "Settings", cx)),
                    ),
            )
            .child(div().flex_1().min_h_0().w_full().child(content))
    }
}
