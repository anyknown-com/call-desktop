//! Shell: top bar · call logs · stage · live transcript. The gear swaps the stage for Settings.

use crate::call_view::CallView;
use crate::history_view::{LogsEvent, LogsView};
use crate::palette::*;
use crate::settings_view::SettingsView;
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{h_flex, v_flex};
use voice_core::call_machine::{Role, Turn, TurnKind};
use voice_runtime::settings::{self, Keys, Settings};

pub struct AppState {
    pub settings: Settings,
    pub keys: Keys,
}
impl Global for AppState {}

actions!(voice_app, [Quit, ToggleSettings]);

pub fn init(cx: &mut App) {
    cx.set_global(AppState { settings: settings::load(), keys: Keys::load() });
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", ToggleSettings, None),
        KeyBinding::new("space", crate::call_view::Interrupt, Some("CallView")),
        KeyBinding::new("escape", crate::call_view::Interrupt, Some("CallView")),
        KeyBinding::new("cmd-enter", crate::call_view::ToggleCall, Some("CallView")),
        KeyBinding::new("cmd-shift-m", crate::call_view::ToggleMute, Some("CallView")),
    ]);
    cx.set_menus(vec![Menu {
        name: "voice".into(),
        items: vec![MenuItem::action("Settings…", ToggleSettings), MenuItem::separator(), MenuItem::action("Quit voice", Quit)],
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

pub struct AppView {
    show_settings: bool,
    call: Entity<CallView>,
    logs: Entity<LogsView>,
    settings: Entity<SettingsView>,
    /// A past call being viewed in the transcript panel (while no call is running).
    viewing: Option<usize>,
    _subs: Vec<Subscription>,
}

impl AppView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let call = cx.new(|cx| CallView::new(window, cx));
        let logs = cx.new(|cx| LogsView::new(window, cx));
        let settings = cx.new(|cx| SettingsView::new(window, cx));
        let mut subs = vec![
            cx.observe(&call, |this, call, cx| {
                // When a call ends, refresh the logs (a transcript was just saved).
                if !call.read(cx).active() {
                    this.logs.update(cx, |l, cx| l.refresh(cx));
                }
                cx.notify();
            }),
            cx.observe(&logs, |_, _, cx| cx.notify()),
        ];
        subs.push(cx.subscribe(&logs, |this, _, ev: &LogsEvent, cx| {
            match ev {
                LogsEvent::Selected(i) => {
                    this.viewing = Some(*i);
                    this.show_settings = false;
                }
                LogsEvent::NewCall => {
                    this.viewing = None;
                    this.show_settings = false;
                }
            }
            cx.notify();
        }));
        Self { show_settings: std::env::var("VOICE_START_PAGE").as_deref() == Ok("settings"), call, logs, settings, viewing: None, _subs: subs }
    }

    fn toggle_settings(&mut self, _: &ToggleSettings, _w: &mut Window, cx: &mut Context<Self>) {
        self.show_settings = !self.show_settings;
        cx.notify();
    }

    fn transcript_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let call = self.call.read(cx);
        let live = call.active();
        let (turns, started, header): (Vec<Turn>, Option<std::time::SystemTime>, &'static str) = if live || self.viewing.is_none() {
            (call.state.turns.clone(), call.call_started.map(|(_, s)| s), "LIVE TRANSCRIPT")
        } else {
            let logs = self.logs.read(cx);
            (self.viewing.and_then(|i| logs.entries.get(i)).map(|e| e.turns.clone()).unwrap_or_default(), None, "TRANSCRIPT")
        };
        let name = {
            let n = cx.global::<AppState>().settings.assistant_name.trim().to_uppercase();
            if n.is_empty() { "AURA".to_string() } else { n }
        };
        let empty_msg = if live { "Listening — say something." } else if self.viewing.is_some() { "This call has no transcript." } else { "Start a call, or pick a past one on the left." };
        v_flex()
            .size_full()
            .bg(c(BG))
            .child(
                h_flex()
                    .px_4()
                    .h(px(56.))
                    .items_center()
                    .gap_2()
                    .child(div().size(px(7.)).rounded_full().bg(if live { c(DANGER) } else { c(TEXT_3) }))
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(c(TEXT_2)).child(header)),
            )
            .child(
                div().id("transcript-scroll").flex_1().min_h_0().overflow_y_scroll().child(if turns.is_empty() {
                    div().p_4().text_sm().text_color(c(TEXT_3)).child(empty_msg).into_any_element()
                } else {
                    v_flex()
                        .px_4()
                        .pb_4()
                        .gap_3()
                        .children(turns.iter().map(|t| {
                            let is_user = t.role == Role::User;
                            let who = match (&t.role, t.kind) {
                                (Role::User, Some(TurnKind::Interjection)) => "YOU · ASIDE".to_string(),
                                (Role::User, _) => "YOU".to_string(),
                                (Role::Assistant, Some(TurnKind::Reaction)) => format!("{name} · ASIDE"),
                                (Role::Assistant, _) => name.clone(),
                            };
                            let time = started.map(|s| clock(s + std::time::Duration::from_millis(t.at.max(0.0) as u64))).unwrap_or_default();
                            let mut text = t.text.clone();
                            if !t.is_final {
                                text.push('…');
                            }
                            div()
                                .w_full()
                                .flex()
                                .when(is_user, |d| d.justify_end())
                                .child(
                                    v_flex()
                                        .max_w(px(260.))
                                        .p_3()
                                        .gap_1()
                                        .rounded_lg()
                                        .bg(if is_user { c(ACCENT) } else { c(ELEVATED) })
                                        .border_1()
                                        .border_color(if is_user { c(ACCENT) } else { c(BORDER) })
                                        .child(
                                            h_flex()
                                                .justify_between()
                                                .gap_3()
                                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(if is_user { c(ACCENT_TEXT) } else { c(SUCCESS) }).child(who))
                                                .child(div().font_family(MONO).text_xs().text_color(if is_user { with_alpha(ACCENT_TEXT, 0.7) } else { c(TEXT_3) }).child(time)),
                                        )
                                        .child(div().text_sm().text_color(if is_user { c(ACCENT_TEXT) } else { c(TEXT) }).child(text))
                                        .when(t.interrupted, |d| d.child(div().text_xs().text_color(if is_user { with_alpha(ACCENT_TEXT, 0.7) } else { c(TEXT_3) }).child("cut off"))),
                                )
                        }))
                        .into_any_element()
                }),
            )
            .child(
                div().p_3().child(
                    h_flex()
                        .id("open-folder")
                        .px_3()
                        .py_2()
                        .gap_2()
                        .items_center()
                        .rounded_lg()
                        .bg(c(ELEVATED))
                        .border_1()
                        .border_color(c(BORDER))
                        .cursor_pointer()
                        .hover(|d| d.bg(c(HOVER)))
                        .child(svg().path("icons/folder.svg").size(px(14.)).text_color(c(TEXT_2)))
                        .child(div().text_xs().text_color(c(TEXT_2)).child("Open transcripts folder"))
                        .child(div().flex_1())
                        .child(svg().path("icons/chevron-right.svg").size(px(14.)).text_color(c(TEXT_3)))
                        .on_click(|_, _, _| {
                            if let Some(d) = voice_runtime::transcript::calls_dir() {
                                let _ = std::fs::create_dir_all(&d);
                                let _ = std::process::Command::new("open").arg(d).spawn();
                            }
                        }),
                ),
            )
    }
}

fn clock(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    // local time via chrono-free approach: use `date`? keep it simple with libc localtime.
    let local = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let ts = secs as libc::time_t;
        libc::localtime_r(&ts, &mut tm);
        (tm.tm_hour, tm.tm_min)
    };
    format!("{:02}:{:02}", local.0, local.1)
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let call = self.call.read(cx);
        let live = call.active();
        let (status_text, status_tone) = if live { ("On call · AEC on", SUCCESS) } else { ("Ready", TEXT_3) };
        v_flex()
            .id("app")
            .on_action(cx.listener(Self::toggle_settings))
            .size_full()
            .bg(c(BG))
            .text_color(c(TEXT))
            .font_family(SANS)
            // top bar
            .child(
                h_flex()
                    .h(px(64.))
                    .px_4()
                    .items_center()
                    .border_b_1()
                    .border_color(c(BORDER))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .w(px(264.))
                            .child(
                                div()
                                    .size(px(28.))
                                    .rounded_md()
                                    .bg(c(ACCENT))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(svg().path("icons/audio-lines.svg").size(px(16.)).text_color(c(ACCENT_TEXT))),
                            )
                            .child(div().text_base().font_weight(FontWeight::BOLD).child("Voice")),
                    )
                    .child(
                        div().flex_1().flex().justify_center().child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_1()
                                .rounded_full()
                                .bg(c(PANEL))
                                .border_1()
                                .border_color(c(BORDER))
                                .child(div().size(px(6.)).rounded_full().bg(c(status_tone)))
                                .child(div().text_xs().text_color(c(TEXT_2)).child(status_text)),
                        ),
                    )
                    .child(
                        div().w(px(264.)).flex().justify_end().child(
                            div()
                                .id("settings")
                                .size(px(32.))
                                .rounded_md()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if self.show_settings { c(HOVER) } else { c(PANEL) })
                                .border_1()
                                .border_color(c(BORDER))
                                .cursor_pointer()
                                .hover(|d| d.bg(c(HOVER)))
                                .child(svg().path("icons/settings.svg").size(px(16.)).text_color(c(TEXT)))
                                .on_click(cx.listener(|this, _, w, cx| this.toggle_settings(&ToggleSettings, w, cx))),
                        ),
                    ),
            )
            // columns
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(div().w(px(280.)).h_full().border_r_1().border_color(c(BORDER)).child(self.logs.clone()))
                    .child(div().flex_1().min_w_0().h_full().child(if self.show_settings { self.settings.clone().into_any_element() } else { self.call.clone().into_any_element() }))
                    .when(!self.show_settings, |d| d.child(div().w(px(340.)).h_full().border_l_1().border_color(c(BORDER)).child(self.transcript_panel(cx)))),
            )
    }
}
