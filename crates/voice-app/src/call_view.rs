//! The call screen: status, transcript, level meter, controls, media-mode / ducking toggles.

use crate::app::{save_settings, AppState};
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{h_flex, v_flex, WindowExt};
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

    /// (word, colour) for the on-air light.
    fn status_look(&self) -> (&'static str, u32) {
        use crate::palette::*;
        match self.state.status {
            CallStatus::Idle => ("Off the line", IDLE),
            CallStatus::Listening => ("Listening", SAGE),
            CallStatus::UserSpeaking => ("You're speaking", CORAL),
            CallStatus::Transcribing => ("Hearing you out", LAVENDER),
            CallStatus::Holding => ("Take your time", SAGE),
            CallStatus::Thinking => ("Thinking", LAVENDER),
            CallStatus::Speaking => ("On air", AMBER),
            CallStatus::Interrupted => ("One moment", AMBER),
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
        use crate::palette::*;
        let (word, tone) = self.status_look();
        let active = self.active();
        let (media_mode, duck) = {
            let s = &cx.global::<AppState>().settings;
            (s.media_mode, s.audio.duck == DuckMode::Mute)
        };
        let has_profile = voice_runtime::enroll::profile_path().is_some_and(|p| p.exists());
        let hint = self.hint.as_ref().filter(|(_, at)| at.elapsed().as_secs() < 8).map(|(h, _)| h.clone());
        let level = if active { self.level.clamp(0.0, 1.0) } else { 0.0 };
        // The light: a soft halo that widens with your voice, a solid core in the state colour.
        let halo = 72.0 + 40.0 * level;
        let core = 22.0 + 10.0 * level;

        let stage = v_flex()
            .items_center()
            .pt_10()
            .pb_4()
            .gap_4()
            .flex_shrink_0()
            .child(
                div()
                    .size(px(120.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .size(px(halo))
                            .rounded_full()
                            .bg(with_alpha(tone, if active { 0.14 + 0.25 * level } else { 0.08 }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(div().size(px(core)).rounded_full().bg(c(tone))),
                    ),
            )
            .child(div().font_family(DISPLAY_FONT).italic().text_2xl().text_color(c(tone)).child(word))
            .child(
                div().h(px(18.)).text_xs().text_color(c(MUTED)).child(match (&self.state.error, hint, self.ducked) {
                    (Some(e), _, _) => format!("⚠ {e}"),
                    (None, Some(h), _) => h,
                    (None, None, true) => "Other apps are muted while it speaks".to_string(),
                    _ => String::new(),
                }),
            );

        let transcript_body = v_flex().gap_5().px_8().py_4().w_full().max_w(px(720.)).mx_auto().children(self.state.turns.iter().map(|t| {
            let (who, is_user) = match (&t.role, t.kind) {
                (Role::User, Some(TurnKind::Interjection)) => ("YOU · aside", true),
                (Role::User, _) => ("YOU", true),
                (Role::Assistant, Some(TurnKind::Reaction)) => ("AI · aside", false),
                (Role::Assistant, _) => ("AI", false),
            };
            let mut text = t.text.clone();
            if !t.is_final {
                text.push('…');
            }
            h_flex()
                .gap_5()
                .items_start()
                .w_full()
                .child(
                    div()
                        .w(px(84.))
                        .flex_shrink_0()
                        .pt_0p5()
                        .text_xs()
                        .text_color(if is_user { c(IVORY_DIM) } else { c(AMBER) })
                        .child(who),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_base()
                        .text_color(if is_user { c(IVORY_DIM) } else { c(IVORY) })
                        .child(text)
                        .when(t.interrupted, |d| d.child(div().text_xs().text_color(c(MUTED)).child("— you cut in"))),
                )
        }));

        let transcript = div()
            .id("transcript")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(if self.state.turns.is_empty() {
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(c(MUTED))
                    .child(if active { "Say something — it's listening." } else { "Press the button and just talk. Space cuts it off. ⌘↩ starts or ends the call." })
                    .into_any_element()
            } else {
                transcript_body.into_any_element()
            });

        let chip = |id: &'static str, label: &'static str, on: bool, enabled: bool| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_full()
                .border_1()
                .border_color(if on { with_alpha(AMBER, 0.6) } else { c(HAIRLINE) })
                .bg(if on { with_alpha(AMBER, 0.12) } else { c(INK) })
                .text_xs()
                .text_color(if !enabled { c(IDLE) } else if on { c(AMBER) } else { c(IVORY_DIM) })
                .when(enabled, |d| d.cursor_pointer().hover(|d| d.border_color(with_alpha(IVORY, 0.4))))
                .child(label)
        };

        let call_button = div()
            .id("call-button")
            .size(px(60.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(if active { c(CORAL) } else { c(SAGE) })
            .hover(|d| d.opacity(0.9))
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c(INK))
            .child(if active { "End" } else if self.starting { "…" } else { "Call" })
            .on_click(cx.listener(|this, _, w, cx| {
                if this.active() {
                    this.hangup(cx)
                } else {
                    this.start(w, cx)
                }
            }));

        let dock = h_flex()
            .flex_shrink_0()
            .w_full()
            .px_6()
            .py_4()
            .gap_4()
            .items_center()
            .justify_center()
            .border_t_1()
            .border_color(c(HAIRLINE))
            .bg(c(PANEL))
            .child(
                chip("duck", "Mute other apps while it speaks", duck, true).on_click(cx.listener(move |_, _, _, cx| {
                    cx.global_mut::<AppState>().settings.audio.duck = if duck { DuckMode::Off } else { DuckMode::Mute };
                    save_settings(cx);
                    cx.notify();
                })),
            )
            .child(chip("interrupt", "Cut in (space)", false, active).when(active, |d| d.on_click(cx.listener(|this, _, w, cx| this.interrupt(&Interrupt, w, cx)))))
            .child(call_button)
            .child(
                chip("media", if has_profile { "Only my voice interrupts" } else { "Only my voice interrupts · enroll first" }, media_mode, has_profile).when(has_profile, |d| {
                    d.on_click(cx.listener(move |_, _, _, cx| {
                        cx.global_mut::<AppState>().settings.media_mode = !media_mode;
                        save_settings(cx);
                        cx.notify();
                    }))
                }),
            );

        v_flex()
            .id("call-view")
            .key_context("CallView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::interrupt))
            .on_action(cx.listener(Self::toggle_call))
            .size_full()
            .child(stage)
            .child(transcript)
            .child(dock)
    }
}
