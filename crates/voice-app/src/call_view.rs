//! The call screen: status, transcript, level meter, controls, media-mode / ducking toggles.

use crate::app::{save_settings, AppState};
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{button::*, h_flex, switch::Switch, v_flex, ActiveTheme, Disableable, WindowExt};
use tokio::sync::mpsc::UnboundedSender;
use voice_core::call_machine::{CallState, CallStatus, Role, TurnKind};
use voice_core::media_gate::GateState;
use voice_runtime::settings::DuckMode;
use voice_runtime::{Runtime, RuntimeCommand, RuntimeEvent, RuntimeOptions};

actions!(voice_app, [Interrupt, ToggleCall]);

pub struct CallView {
    focus_handle: FocusHandle,
    commands: Option<UnboundedSender<RuntimeCommand>>,
    state: CallState,
    level: f32,
    ducked: bool,
    gate: Option<GateState>,
    hint: Option<(String, std::time::Instant)>,
    starting: bool,
    scroll: ScrollHandle,
    _task: Option<Task<()>>,
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
            scroll: ScrollHandle::new(),
            _task: None,
        }
    }

    fn active(&self) -> bool {
        self.commands.is_some()
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                    window.push_notification(gpui_component::notification::Notification::warning("Media mode needs a voice profile — see “My voice”."), cx);
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
        // Runtime::start needs the tokio context (it spawns tasks) but blocks briefly on audio setup.
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
        let task = cx.spawn(async move |this, cx| {
            while let Some(ev) = rt.events.recv().await {
                tracing::debug!(?ev, "runtime event");
                let stop = this
                    .update(cx, |this, cx| {
                        let stop = this.on_event(ev, cx);
                        cx.notify();
                        stop
                    })
                    .unwrap_or(true);
                if stop {
                    break;
                }
            }
            tracing::info!("runtime event loop ended");
            let _ = this.update(cx, |this, cx| {
                this.commands = None;
                this.ducked = false;
                this.level = 0.0;
                cx.notify();
            });
            rt.join().await;
        });
        self._task = Some(task);
        window.focus(&self.focus_handle);
    }

    /// Returns true when the runtime is finished.
    fn on_event(&mut self, ev: RuntimeEvent, cx: &mut Context<Self>) -> bool {
        match ev {
            RuntimeEvent::State(st) => {
                let ended = !st.active && self.state.active;
                self.state = st;
                self.scroll.scroll_to_bottom();
                if ended {
                    return false; // wait for Saved / channel close
                }
            }
            RuntimeEvent::Level(l) => self.level = l,
            RuntimeEvent::GateState(g) => self.gate = Some(g),
            RuntimeEvent::Hint(h) => self.hint = Some((h, std::time::Instant::now())),
            RuntimeEvent::Error(e) => {
                tracing::warn!("{e}");
                self.state.error = Some(e);
            }
            RuntimeEvent::Ducked(d) => self.ducked = d,
            RuntimeEvent::Saved(p) => self.hint = Some((format!("Transcript saved: {}", p.display()), std::time::Instant::now())),
        }
        let _ = cx;
        false
    }

    fn hangup(&mut self, cx: &mut Context<Self>) {
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

    fn status_label(&self) -> (&'static str, &'static str) {
        match self.state.status {
            CallStatus::Idle => ("Idle", "muted"),
            CallStatus::Listening => ("Listening", "success"),
            CallStatus::UserSpeaking => ("You're speaking", "primary"),
            CallStatus::Transcribing => ("Transcribing…", "info"),
            CallStatus::Holding => ("Waiting for you to continue…", "warning"),
            CallStatus::Thinking => ("Thinking…", "info"),
            CallStatus::Speaking => ("Speaking", "primary"),
            CallStatus::Interrupted => ("Deciding…", "warning"),
        }
    }
}

impl Focusable for CallView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CallView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (label, tone) = self.status_label();
        let tone_color = match tone {
            "success" => theme.success,
            "primary" => theme.primary,
            "info" => theme.info,
            "warning" => theme.warning,
            _ => theme.muted_foreground,
        };
        let active = self.active();
        let (media_mode, duck) = {
            let s = &cx.global::<AppState>().settings;
            (s.media_mode, s.audio.duck == DuckMode::Mute)
        };
        let has_profile = voice_runtime::enroll::profile_path().is_some_and(|p| p.exists());
        let hint = self.hint.as_ref().filter(|(_, at)| at.elapsed().as_secs() < 8).map(|(h, _)| h.clone());

        let transcript = v_flex().gap_3().p_4().children(self.state.turns.iter().map(|t| {
            let (who, is_user) = match (&t.role, t.kind) {
                (Role::User, Some(TurnKind::Interjection)) => ("You (aside)", true),
                (Role::User, _) => ("You", true),
                (Role::Assistant, Some(TurnKind::Reaction)) => ("Assistant (reaction)", false),
                (Role::Assistant, _) => ("Assistant", false),
            };
            let mut text = t.text.clone();
            if !t.is_final {
                text.push('…');
            }
            v_flex()
                .gap_1()
                .max_w(px(720.))
                .when(is_user, |d| d.ml_auto().items_end())
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(who)
                        .when(t.interrupted, |d| d.child("· interrupted")),
                )
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(if is_user { theme.primary } else { theme.secondary })
                        .text_color(if is_user { theme.primary_foreground } else { theme.secondary_foreground })
                        .child(text),
                )
        }));

        v_flex()
            .id("call-view")
            .key_context("CallView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::interrupt))
            .on_action(cx.listener(Self::toggle_call))
            .size_full()
            // header
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().size(px(10.)).rounded_full().bg(tone_color))
                    .child(div().font_weight(FontWeight::MEDIUM).child(label))
                    .when_some(self.gate, |d, g| d.child(div().text_xs().text_color(theme.muted_foreground).child(format!("gate {g:?}"))))
                    .when(self.ducked, |d| d.child(div().text_xs().px_2().py_0p5().rounded_md().bg(theme.warning).text_color(theme.warning_foreground).child("other audio muted")))
                    .child(div().flex_1())
                    // level meter
                    .child(
                        div().w(px(120.)).h(px(6.)).rounded_full().bg(theme.muted).child(
                            div().h_full().rounded_full().bg(theme.primary).w(relative(self.level.clamp(0.0, 1.0))),
                        ),
                    ),
            )
            // transcript
            .child(
                div()
                    .id("transcript")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(if self.state.turns.is_empty() {
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(if active { "Say something." } else { "Start a call and just talk. Space interrupts; ⌘↩ starts/ends." })
                            .into_any_element()
                    } else {
                        transcript.into_any_element()
                    }),
            )
            // hint / error line
            .when_some(hint, |d, h| d.child(div().px_4().py_1().text_xs().text_color(theme.muted_foreground).child(h)))
            .when_some(self.state.error.clone(), |d, e| d.child(div().px_4().py_1().text_xs().text_color(theme.danger).child(e)))
            // controls
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .items_center()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(if active {
                        Button::new("hangup").danger().label("Hang up").on_click(cx.listener(|this, _, _, cx| this.hangup(cx)))
                    } else {
                        Button::new("start").primary().label(if self.starting { "Starting…" } else { "Start call" }).loading(self.starting).on_click(cx.listener(|this, _, w, cx| this.start(w, cx)))
                    })
                    .child(Button::new("interrupt").outline().label("Interrupt (space)").disabled(!active).on_click(cx.listener(|this, _, w, cx| this.interrupt(&Interrupt, w, cx))))
                    .child(div().flex_1())
                    .child(
                        Switch::new("duck")
                            .checked(duck)
                            .label("Mute other apps while speaking")
                            .on_click(cx.listener(|_, on: &bool, _, cx| {
                                cx.global_mut::<AppState>().settings.audio.duck = if *on { DuckMode::Mute } else { DuckMode::Off };
                                save_settings(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Switch::new("media")
                            .checked(media_mode)
                            .disabled(!has_profile)
                            .label(if has_profile { "Media mode" } else { "Media mode (enroll first)" })
                            .on_click(cx.listener(|_, on: &bool, _, cx| {
                                cx.global_mut::<AppState>().settings.media_mode = *on;
                                save_settings(cx);
                                cx.notify();
                            })),
                    ),
            )
    }
}
