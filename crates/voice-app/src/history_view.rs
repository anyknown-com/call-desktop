//! Past calls: transcripts saved by the runtime (JSON + Markdown), newest first.

use gpui::{prelude::FluentBuilder, *};
use crate::palette::{c, MUTED};
use crate::settings_view::page_title;
use gpui_component::{button::*, h_flex, v_flex, Sizable};
use std::path::PathBuf;

pub struct HistoryView {
    files: Vec<(String, PathBuf)>,
}

impl HistoryView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut v = Self { files: vec![] };
        v.refresh(cx);
        v
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.files.clear();
        if let Some(dir) = voice_runtime::transcript::calls_dir() {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().is_some_and(|x| x == "md") {
                        let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        self.files.push((name, p));
                    }
                }
            }
        }
        self.files.sort_by(|a, b| b.0.cmp(&a.0));
        cx.notify();
    }
}

impl Render for HistoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dir = voice_runtime::transcript::calls_dir();
        div().id("history").size_full().overflow_y_scroll().child(
            v_flex()
                .gap_3()
                .p_6()
                .max_w(px(760.))
                .child(
                    h_flex()
                        .items_center()
                        .gap_3()
                        .child(page_title("History"))
                        .child(div().flex_1())
                        .child(Button::new("refresh").ghost().small().label("Refresh").on_click(cx.listener(|this, _, _, cx| this.refresh(cx))))
                        .when_some(dir, |d, dir| {
                            d.child(Button::new("reveal").ghost().small().label("Show in Finder").on_click(move |_, _, _| {
                                let _ = std::process::Command::new("open").arg(&dir).spawn();
                            }))
                        }),
                )
                .child(if self.files.is_empty() {
                    div().text_sm().text_color(c(MUTED)).child("No calls yet. Transcripts are saved automatically when you hang up.").into_any_element()
                } else {
                    v_flex()
                        .gap_1()
                        .children(self.files.iter().map(|(name, path)| {
                            let path = path.clone();
                            div()
                                .id(SharedString::from(name.clone()))
                                .px_3()
                                .py_2()
                                .rounded_lg()
                                .cursor_pointer()
                                .text_sm()
                                .hover(|d| d.bg(c(crate::palette::PANEL)))
                                .child(name.replace('_', "  ").replace('-', ":").replacen(':', "-", 2))
                                .on_click(move |_, _, _| {
                                    let _ = std::process::Command::new("open").arg(&path).spawn();
                                })
                        }))
                        .into_any_element()
                }),
        )
    }
}
