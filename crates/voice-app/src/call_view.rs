//! The centre stage: assistant name, status pill, call timer, round controls. Holds the running
//! call; the transcript panel (in `app.rs`) reads its state.

use crate::app::{save_settings, AppState};
use crate::palette::*;
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{h_flex, v_flex, WindowExt};
use std::time::{Instant, SystemTime};
use tokio::sync::mpsc::UnboundedSender;
use voice_core::call_machine::{CallState, CallStatus};
use voice_core::media_gate::GateState;
use voice_runtime::settings::DuckMode;
use voice_runtime::{Runtime, RuntimeCommand, RuntimeEvent, RuntimeOptions};

actions!(voice_app, [Interrupt, ToggleCall, ToggleMute]);

pub struct CallView {
    focus_handle: FocusHandle,
    commands: Option<UnboundedSender<RuntimeCommand>>,
    pub state: CallState,
    pub level: f32,
    pub ducked: bool,
    pub gate: Option<GateState>,
    pub hint: Option<(String, Instant)>,
    pub starting: bool,
    pub mic_muted: bool,
    pub call_started: Option<(Instant, SystemTime)>,
    _events: Option<Task<()>>,
    _ticker: Option<Task<()>>,
}

impl CallView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            commands: None,
            state: CallState { active: false, status: CallStatus::Idle, turns: vec![], error: None },
            level: 0.0,
            ducked: false,
            gate: None,
            hint: None,
            starting: false,
            mic_muted: false,
            call_started: None,
            _events: None,
            _ticker: None,
        }
    }

    pub fn active(&self) -> bool {
        self.commands.is_some()
    }

    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active() || self.starting {
            return;
        }
        let (settings, keys) = {
            let g = cx.global::<AppState>();
            (g.settings.clone(), g.keys.clone())
        };
        let profile = if settings.media_mode {
            match voice_runtime::enroll::load_profile() {
                Some(p) => Some(p),
                None => {
                    window.push_notification(gpui_component::notification::Notification::warning("Media mode needs a voice profile — see Settings › My voice."), cx);
                    return;
                }
            }
        } else {
            None
        };
        let mock = std::env::var("VOICE_MOCK").is_ok();
        let missing = keys.missing(&settings);
        if !mock && !missing.is_empty() {
            window.push_notification(gpui_component::notification::Notification::warning(format!("Missing API keys: {} — see Settings.", missing.join(", "))), cx);
            return;
        }
        self.starting = true;
        cx.notify();
        let opts = RuntimeOptions { settings, keys, profile, models_dir: crate::models_dir(), mock, input_wav: std::env::var("VOICE_MIC_WAV").ok().map(Into::into) };
        let started = {
            let _g = crate::tokio().enter();
            Runtime::start(opts)
        };
        self.starting = false;
        let mut rt = match started {
            Ok(rt) => rt,
            Err(e) => {
                window.push_notification(gpui_component::notification::Notification::error(format!("Could not start: {e:#}")), cx);
                cx.notify();
                return;
            }
        };
        let _ = rt.commands.send(RuntimeCommand::Start);
        self.commands = Some(rt.commands.clone());
        self.state.turns.clear();
        self.state.error = None;
        self.mic_muted = false;
        self.call_started = Some((Instant::now(), SystemTime::now()));
        self._events = Some(cx.spawn(async move |this, cx| {
            while let Some(ev) = rt.events.recv().await {
                if this.update(cx, |this, cx| {
                    this.on_event(ev);
                    cx.notify();
                }).is_err() {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.commands = None;
                this.ducked = false;
                this.level = 0.0;
                this.call_started = None;
                cx.notify();
            });
            rt.join().await;
        }));
        // Timer redraw once a second while on a call.
        self._ticker = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
            let alive = this.update(cx, |this, cx| {
                cx.notify();
                this.active()
            });
            if !matches!(alive, Ok(true)) {
                break;
            }
        }));
        window.focus(&self.focus_handle);
    }

    fn on_event(&mut self, ev: RuntimeEvent) {
        match ev {
            RuntimeEvent::State(st) => self.state = st,
            RuntimeEvent::Level(l) => self.level = l,
            RuntimeEvent::GateState(g) => self.gate = Some(g),
            RuntimeEvent::Hint(h) => self.hint = Some((h, Instant::now())),
            RuntimeEvent::Error(e) => {
                tracing::warn!("{e}");
                self.state.error = Some(e);
            }
            RuntimeEvent::Ducked(d) => self.ducked = d,
            RuntimeEvent::Saved(p) => self.hint = Some((format!("Transcript saved to {}", p.parent().map(|d| d.display().to_string()).unwrap_or_default()), Instant::now())),
        }
    }

    pub fn hangup(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = &self.commands {
            let _ = c.send(RuntimeCommand::Hangup);
            let _ = c.send(RuntimeCommand::Shutdown);
        }
        cx.notify();
    }

    fn interrupt(&mut self, _: &Interrupt, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = &self.commands {
            let _ = c.send(RuntimeCommand::Interrupt);
        }
        cx.notify();
    }

    fn toggle_call(&mut self, _: &ToggleCall, window: &mut Window, cx: &mut Context<Self>) {
        if self.active() {
            self.hangup(cx);
        } else {
            self.start(window, cx);
        }
    }

    fn toggle_mute(&mut self, _: &ToggleMute, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = &self.commands {
            self.mic_muted = !self.mic_muted;
            let _ = c.send(RuntimeCommand::SetMicMuted(self.mic_muted));
        }
        cx.notify();
    }

    /// (label, colour) for the status pill.
    pub fn status_look(&self) -> (&'static str, u32) {
        match self.state.status {
            CallStatus::Idle => ("READY", TEXT_3),
            CallStatus::Listening => ("LISTENING…", SUCCESS),
            CallStatus::UserSpeaking => ("HEARING YOU", ACCENT),
            CallStatus::Transcribing => ("TRANSCRIBING", PURPLE),
            CallStatus::Holding => ("WAITING FOR YOU", WARN),
            CallStatus::Thinking => ("THINKING", PURPLE),
            CallStatus::Speaking => ("SPEAKING", ACCENT),
            CallStatus::Interrupted => ("ONE MOMENT", WARN),
        }
    }

    pub fn elapsed(&self) -> String {
        match self.call_started {
            Some((t, _)) => {
                let s = t.elapsed().as_secs();
                format!("{:02}:{:02}", s / 60, s % 60)
            }
            None => "00:00".into(),
        }
    }
}

impl Focusable for CallView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// A round control with a label under it.
fn control(id: &'static str, icon: &'static str, label: &'static str, tone: Option<u32>, enabled: bool) -> Stateful<Div> {
    v_flex()
        .id(id)
        .items_center()
        .gap_2()
        .w(px(72.))
        .when(enabled, |d| d.cursor_pointer())
        .child(
            div()
                .size(px(52.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(match tone {
                    Some(t) => c(t),
                    None => c(ELEVATED),
                })
                .border_1()
                .border_color(match tone {
                    Some(t) => c(t),
                    None => c(BORDER),
                })
                .text_color(if tone.is_some() { c(ACCENT_TEXT) } else if enabled { c(TEXT) } else { c(TEXT_3) })
                .when(enabled, |d| d.hover(|d| d.border_color(c(BORDER_STRONG)).bg(if tone.is_some() { d_bg(tone) } else { c(HOVER) })))
                .child(svg().path(icon).size(px(20.)).text_color(if tone.is_some() { c(ACCENT_TEXT) } else if enabled { c(TEXT) } else { c(TEXT_3) })),
        )
        .child(div().text_xs().text_color(if enabled { c(TEXT_2) } else { c(TEXT_3) }).child(label))
}

fn d_bg(tone: Option<u32>) -> Hsla {
    with_alpha(tone.unwrap_or(ELEVATED), 0.85)
}

impl Render for CallView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, tone) = self.status_look();
        let active = self.active();
        let (name, duck) = {
            let s = &cx.global::<AppState>().settings;
            (s.assistant_name.clone(), s.audio.duck == DuckMode::Mute)
        };
        let hint = self.hint.as_ref().filter(|(_, at)| at.elapsed().as_secs() < 8).map(|(h, _)| h.clone());
        let name = if name.trim().is_empty() { "AURA".to_string() } else { name.to_uppercase() };
        let level = if active { self.level.clamp(0.0, 1.0) } else { 0.0 };

        v_flex()
            .id("stage")
            .key_context("CallView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::interrupt))
            .on_action(cx.listener(Self::toggle_call))
            .on_action(cx.listener(Self::toggle_mute))
            .size_full()
            .items_center()
            // header: name + status pill
            .child(
                v_flex()
                    .items_center()
                    .gap_3()
                    .pt_12()
                    .flex_shrink_0()
                    .child(div().text_3xl().font_weight(FontWeight::BOLD).text_color(c(TEXT)).child(name))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_1()
                            .rounded_full()
                            .bg(with_alpha(tone, 0.12))
                            .border_1()
                            .border_color(with_alpha(tone, 0.35))
                            .child(div().size(px(6.)).rounded_full().bg(c(tone)))
                            .child(div().font_family(MONO).text_xs().text_color(c(tone)).child(label)),
                    ),
            )
            // middle: level + timer
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(
                        // level ring
                        div()
                            .size(px(160.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(div().size(px(64.0 + 80.0 * level)).rounded_full().bg(with_alpha(if active { tone } else { TEXT_3 }, 0.08 + 0.18 * level))),
                    )
                    .child(div().font_family(MONO).text_3xl().text_color(c(TEXT)).child(self.elapsed()))
                    .child(div().font_family(MONO).text_xs().text_color(c(TEXT_3)).child(if active { "AEC ON · 48KHZ · LOCAL VAD" } else { "OFF THE LINE" }))
                    .child(div().h(px(18.)).text_xs().text_color(if self.state.error.is_some() { c(DANGER) } else { c(TEXT_3) }).child(match (&self.state.error, hint, self.ducked) {
                        (Some(e), _, _) => e.clone(),
                        (None, Some(h), _) => h,
                        (None, None, true) => "Other apps muted while it speaks".to_string(),
                        _ => String::new(),
                    })),
            )
            // controls
            .child(
                h_flex()
                    .flex_shrink_0()
                    .mb_10()
                    .px_6()
                    .py_4()
                    .gap_2()
                    .rounded_full()
                    .bg(c(PANEL))
                    .border_1()
                    .border_color(c(BORDER))
                    .child(
                        control("mute", if self.mic_muted { "icons/mic-off.svg" } else { "icons/mic.svg" }, if self.mic_muted { "Unmute" } else { "Mute" }, self.mic_muted.then_some(DANGER), active)
                            .when(active, |d| d.on_click(cx.listener(|this, _, w, cx| this.toggle_mute(&ToggleMute, w, cx)))),
                    )
                    .child(
                        control("quiet", if duck { "icons/volume-x.svg" } else { "icons/volume-2.svg" }, "Others", duck.then_some(ACCENT), true).on_click(cx.listener(move |_, _, _, cx| {
                            cx.global_mut::<AppState>().settings.audio.duck = if duck { DuckMode::Off } else { DuckMode::Mute };
                            save_settings(cx);
                            cx.notify();
                        })),
                    )
                    .child(control("cutin", "icons/waveform.svg", "Cut in", None, active).when(active, |d| d.on_click(cx.listener(|this, _, w, cx| this.interrupt(&Interrupt, w, cx)))))
                    .child(
                        control(
                            "call",
                            if active { "icons/phone-off.svg" } else { "icons/phone.svg" },
                            if active { "End" } else if self.starting { "…" } else { "Call" },
                            Some(if active { DANGER } else { SUCCESS }),
                            true,
                        )
                        .on_click(cx.listener(|this, _, w, cx| {
                            if this.active() {
                                this.hangup(cx)
                            } else {
                                this.start(w, cx)
                            }
                        })),
                    ),
            )
    }
}
