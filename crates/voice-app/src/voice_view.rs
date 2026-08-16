//! "My voice": speaker enrollment for Media mode. The Enroller (audio + models) runs on a
//! background thread; this view sends it commands and shows progress.

use crate::state::AppState;
use crate::palette::{c, BORDER, DANGER, PANEL, TEXT, TEXT_3};
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{button::*, h_flex, v_flex, Disableable, Sizable};
use std::sync::mpsc::Sender;
use tokio::sync::mpsc::UnboundedReceiver;
use voice_runtime::enroll::{self, describe_rejection, Enroller, FinishError, CLIP_SECS};

enum Cmd {
    Clip,
    Held,
    Stop,
}

enum Msg {
    Ready,
    Recording,
    Accepted(usize, usize),
    Rejected(String),
    Done { held_out_score: f64 },
    Failed(String),
    Error(String),
}

#[derive(Clone, PartialEq)]
enum Phase {
    Idle,
    Starting,
    Ready { accepted: usize, needed: usize },
    Recording,
    Processing,
    Done,
}

pub struct VoiceView {
    phase: Phase,
    log: Vec<String>,
    has_profile: bool,
    profile_summary: Option<String>,
    cmd: Option<Sender<Cmd>>,
    _task: Option<Task<()>>,
}

impl VoiceView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut v = Self { phase: Phase::Idle, log: vec![], has_profile: false, profile_summary: None, cmd: None, _task: None };
        v.refresh_profile();
        let _ = cx;
        v
    }

    fn refresh_profile(&mut self) {
        let p = enroll::load_profile();
        self.has_profile = p.is_some();
        self.profile_summary = p.map(|p| {
            format!(
                "Enrolled {} · held-out {:.2} · θ_high {:.2} · full-turn {:.2}",
                chrono_like(p.created_at),
                p.held_out_score,
                p.thresholds.streaming_high,
                p.thresholds.full_turn
            )
        });
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.cmd.is_some() {
            return;
        }
        self.phase = Phase::Starting;
        self.log.clear();
        let settings = cx.global::<AppState>().settings.clone();
        let models = crate::models_dir();
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Cmd>();
        let (msg_tx, msg_rx) = tokio::sync::mpsc::unbounded_channel::<Msg>();
        std::thread::Builder::new()
            .name("enroll".into())
            .spawn(move || {
                let mut e = match Enroller::new(&settings, &models) {
                    Ok(e) => e,
                    Err(err) => {
                        let _ = msg_tx.send(Msg::Error(format!("{err:#}")));
                        return;
                    }
                };
                let _ = msg_tx.send(Msg::Ready);
                while let Ok(c) = cmd_rx.recv() {
                    match c {
                        Cmd::Stop => break,
                        Cmd::Clip => {
                            let _ = msg_tx.send(Msg::Recording);
                            let pcm = e.record(CLIP_SECS);
                            match e.submit_clip(&pcm) {
                                Ok(Ok(n)) => {
                                    let _ = msg_tx.send(Msg::Accepted(n, e.needed()));
                                }
                                Ok(Err(r)) => {
                                    let _ = msg_tx.send(Msg::Rejected(describe_rejection(r).into()));
                                }
                                Err(err) => {
                                    let _ = msg_tx.send(Msg::Error(format!("{err:#}")));
                                }
                            }
                        }
                        Cmd::Held => {
                            let _ = msg_tx.send(Msg::Recording);
                            let pcm = e.record(CLIP_SECS);
                            match e.finish(&pcm) {
                                Ok(Ok(p)) => {
                                    let _ = msg_tx.send(Msg::Done { held_out_score: p.held_out_score });
                                    break;
                                }
                                Ok(Err(FinishError::Clip(r))) => {
                                    let _ = msg_tx.send(Msg::Rejected(describe_rejection(r).into()));
                                }
                                Ok(Err(FinishError::HeldOutBelowThreshold { held_out_score })) => {
                                    let _ = msg_tx.send(Msg::Failed(format!(
                                        "Check clip scored {held_out_score:.2}, below the required margin — Media mode would be unreliable with this mic/room. Try again closer to the mic, somewhere quieter."
                                    )));
                                    break;
                                }
                                Err(err) => {
                                    let _ = msg_tx.send(Msg::Error(format!("{err:#}")));
                                }
                            }
                        }
                    }
                }
            })
            .expect("spawn enroll thread");
        self.cmd = Some(cmd_tx);
        self._task = Some(cx.spawn(async move |this, cx| Self::pump(this, msg_rx, cx).await));
        cx.notify();
    }

    async fn pump(this: WeakEntity<Self>, mut rx: UnboundedReceiver<Msg>, cx: &mut AsyncApp) {
        while let Some(m) = rx.recv().await {
            let done = this
                .update(cx, |this, cx| {
                    let done = match m {
                        Msg::Ready => {
                            this.phase = Phase::Ready { accepted: 0, needed: 6 };
                            false
                        }
                        Msg::Recording => {
                            this.phase = Phase::Recording;
                            false
                        }
                        Msg::Accepted(n, needed) => {
                            this.log.push(format!("Clip {n}/{needed} accepted ✓"));
                            this.phase = Phase::Ready { accepted: n, needed };
                            false
                        }
                        Msg::Rejected(r) => {
                            this.log.push(format!("Rejected: {r}"));
                            let (a, n) = match this.phase {
                                Phase::Ready { accepted, needed } => (accepted, needed),
                                _ => (this.log.iter().filter(|l| l.contains("accepted")).count(), 6),
                            };
                            this.phase = Phase::Ready { accepted: a, needed: n };
                            false
                        }
                        Msg::Done { held_out_score } => {
                            this.log.push(format!("Profile saved (held-out score {held_out_score:.2}). Media mode is ready."));
                            this.phase = Phase::Done;
                            true
                        }
                        Msg::Failed(f) | Msg::Error(f) => {
                            this.log.push(f);
                            this.phase = Phase::Idle;
                            true
                        }
                    };
                    if done {
                        this.cmd = None;
                        this.refresh_profile();
                    }
                    cx.notify();
                    done
                })
                .unwrap_or(true);
            if done {
                break;
            }
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.cmd.take() {
            let _ = c.send(Cmd::Stop);
        }
        self.phase = Phase::Idle;
        cx.notify();
    }
}

fn chrono_like(ms: f64) -> String {
    let secs = (ms / 1000.0) as i64;
    let days = secs / 86400;
    // civil from days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

impl Render for VoiceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let phase = self.phase.clone();
        let busy = matches!(phase, Phase::Starting | Phase::Recording | Phase::Processing);
        let (accepted, needed) = match phase {
            Phase::Ready { accepted, needed } => (accepted, needed),
            _ => (0, 6),
        };
        v_flex()
            .gap_3()
            .p_5()
            .rounded_lg()
            .bg(c(PANEL))
            .border_1()
            .border_color(c(BORDER))
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(c(TEXT)).child("My voice · Media mode"))
            .child(div().w(px(660.)).flex_shrink_0().text_sm().text_color(c(TEXT_3)).child(
                "Media mode lets you watch a video while talking to the assistant: generic speech never interrupts it — only your verified voice does. \
                 Enroll once: six clips of about 3 seconds each, then one check clip. Speak naturally, e.g. read a sentence.",
            ))
            .child(
                v_flex().gap_2().child(div().text_xs().text_color(c(TEXT_3)).child("PROFILE")).child(
                    match &self.profile_summary {
                        Some(s) => div().text_sm().child(s.clone()),
                        None => div().text_sm().text_color(c(TEXT_3)).child("No profile yet."),
                    },
                )
                .when(self.has_profile, |d| {
                    d.child(Button::new("delete").ghost().small().label("Delete profile").on_click(cx.listener(|this, _, _, cx| {
                        let _ = enroll::delete_profile();
                        cx.global_mut::<AppState>().settings.media_mode = false;
                        crate::state::save_settings(cx);
                        this.refresh_profile();
                        cx.notify();
                    })))
                }),
            )
            .child(
                v_flex()
                    .gap_3()
                    .child(div().text_xs().text_color(c(TEXT_3)).child("ENROLL"))
                    .child(match &phase {
                        Phase::Idle | Phase::Done => h_flex().gap_2().child(Button::new("start").primary().label(if self.has_profile { "Re-enroll" } else { "Start enrollment" }).on_click(cx.listener(|this, _, _, cx| this.start(cx)))),
                        Phase::Starting => h_flex().child(div().text_sm().child("Opening microphone…")),
                        Phase::Recording => h_flex().gap_2().items_center().child(div().size(px(10.)).rounded_full().bg(c(DANGER))).child(div().text_sm().child("Recording — keep talking…")),
                        Phase::Processing => h_flex().child(div().text_sm().child("Checking…")),
                        Phase::Ready { .. } => h_flex()
                            .gap_2()
                            .items_center()
                            .child(if accepted < needed {
                                Button::new("clip").primary().label(format!("Record clip {}/{}", accepted + 1, needed)).disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(c) = &this.cmd {
                                        let _ = c.send(Cmd::Clip);
                                    }
                                    this.phase = Phase::Processing;
                                    cx.notify();
                                }))
                            } else {
                                Button::new("held").primary().label("Record check clip").disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(c) = &this.cmd {
                                        let _ = c.send(Cmd::Held);
                                    }
                                    this.phase = Phase::Processing;
                                    cx.notify();
                                }))
                            })
                            .child(Button::new("cancel").ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| this.stop(cx))))
                            .child(div().text_xs().text_color(c(TEXT_3)).child("Press, then speak for ~3 s.")),
                    })
                    .child(v_flex().gap_1().children(self.log.iter().map(|l| div().text_sm().child(l.clone())))),
            )
    }
}
