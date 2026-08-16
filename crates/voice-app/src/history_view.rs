//! Left column: call logs (saved transcripts), newest first. Selecting one shows it in the
//! transcript panel.

use crate::palette::*;
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{h_flex, v_flex};
use std::path::PathBuf;
use voice_core::call_machine::{Role, Turn};

pub struct LogEntry {
    pub path: PathBuf,
    pub title: String,
    pub snippet: String,
    pub when: String,
    pub turns: Vec<Turn>,
}

pub enum LogsEvent {
    Selected(usize),
    NewCall,
}

pub struct LogsView {
    pub entries: Vec<LogEntry>,
    pub selected: Option<usize>,
}

impl EventEmitter<LogsEvent> for LogsView {}

impl LogsView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut v = Self { entries: vec![], selected: None };
        v.refresh(cx);
        v
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.entries.clear();
        if let Some(dir) = voice_runtime::transcript::calls_dir() {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().is_none_or(|x| x != "json") {
                        continue;
                    }
                    let Ok(bytes) = std::fs::read(&p) else { continue };
                    let Ok(turns) = serde_json::from_slice::<Vec<Turn>>(&bytes) else { continue };
                    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    // "2026-08-16_09-24-37" → "08-16 09:24"
                    let when = stem.get(5..16).map(|s| s.replace('_', " ").replacen('-', "-", 1)).unwrap_or(stem.clone());
                    let when = when.chars().enumerate().map(|(i, ch)| if i >= 6 && ch == '-' { ':' } else { ch }).collect::<String>();
                    let title = turns.iter().find(|t| t.role == Role::User).map(|t| trim(&t.text, 34)).unwrap_or_else(|| "Call".into());
                    let snippet = turns.iter().find(|t| t.role == Role::Assistant).map(|t| trim(&t.text, 90)).unwrap_or_default();
                    self.entries.push(LogEntry { path: p, title, snippet, when, turns });
                }
            }
        }
        self.entries.sort_by(|a, b| b.path.cmp(&a.path));
        if self.selected.is_some_and(|i| i >= self.entries.len()) {
            self.selected = None;
        }
        cx.notify();
    }
}

fn trim(s: &str, n: usize) -> String {
    let s = s.trim().replace('\n', " ");
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s
    }
}

impl Render for LogsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(c(BG))
            .child(
                h_flex()
                    .px_4()
                    .h(px(56.))
                    .items_center()
                    .justify_between()
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(c(TEXT_2)).child("CALL LOGS"))
                    .child(
                        div()
                            .id("new-call")
                            .size(px(28.))
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(c(ELEVATED))
                            .border_1()
                            .border_color(c(BORDER))
                            .cursor_pointer()
                            .hover(|d| d.bg(c(HOVER)))
                            .child(svg().path("icons/plus.svg").size(px(14.)).text_color(c(TEXT)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selected = None;
                                cx.emit(LogsEvent::NewCall);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div().id("logs").flex_1().min_h_0().overflow_y_scroll().child(
                    v_flex().px_3().gap_2().pb_4().children(self.entries.iter().enumerate().map(|(i, e)| {
                        let sel = self.selected == Some(i);
                        v_flex()
                            .id(("log", i))
                            .p_3()
                            .gap_1()
                            .rounded_lg()
                            .cursor_pointer()
                            .bg(if sel { c(ELEVATED) } else { c(BG) })
                            .border_1()
                            .border_color(if sel { c(BORDER_STRONG) } else { c(BG) })
                            .hover(|d| d.bg(c(ELEVATED)))
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .gap_2()
                                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(c(TEXT)).overflow_hidden().child(e.title.clone()))
                                    .child(div().font_family(MONO).text_xs().text_color(c(TEXT_3)).flex_shrink_0().child(e.when.clone())),
                            )
                            .when(!e.snippet.is_empty(), |d| d.child(div().text_xs().text_color(c(TEXT_2)).child(e.snippet.clone())))
                            .child(
                                h_flex().gap_1().items_center().pt_1().child(svg().path("icons/clock.svg").size(px(11.)).text_color(c(TEXT_3))).child(
                                    div().font_family(MONO).text_xs().text_color(c(TEXT_3)).child(format!("{} turns", e.turns.len())),
                                ),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = Some(i);
                                cx.emit(LogsEvent::Selected(i));
                                cx.notify();
                            }))
                    })),
                ),
            )
    }
}
