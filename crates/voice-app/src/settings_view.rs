//! Settings: API keys (keychain), LLM/STT/TTS, turn-taking, audio devices, system prompt.
//! Every change is saved immediately.

use crate::app::{save_settings, AppState};
use crate::palette::{c, BORDER, PANEL, TEXT, TEXT_2, TEXT_3};
use crate::voice_view::VoiceView;
use gpui::*;
use gpui_component::{
    button::*,
    input::{Input, InputEvent, InputState},
    select::{SearchableVec, Select, SelectEvent, SelectState},
    switch::Switch,
    h_flex, v_flex, IndexPath, Sizable, WindowExt,
};
use voice_providers::{Effort, LlmProvider};
use voice_runtime::settings::{Keys, Settings, TtsProvider};

type Sel = SelectState<SearchableVec<SharedString>>;

const EFFORTS: [Effort; 8] = [Effort::Unset, Effort::None, Effort::Minimal, Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max];

fn effort_label(e: Effort) -> SharedString {
    if e == Effort::Unset { "default".into() } else { e.as_str().into() }
}

pub struct SettingsView {
    // keys
    key_openai: Entity<InputState>,
    key_anthropic: Entity<InputState>,
    key_elevenlabs: Entity<InputState>,
    // llm
    llm_provider: Entity<Sel>,
    llm_model: Entity<InputState>,
    llm_effort: Entity<Sel>,
    fast_model: Entity<InputState>,
    fast_effort: Entity<Sel>,
    // stt / tts
    stt_model: Entity<InputState>,
    stt_lang: Entity<InputState>,
    tts_provider: Entity<Sel>,
    el_model: Entity<InputState>,
    el_voice: Entity<InputState>,
    oa_model: Entity<InputState>,
    oa_voice: Entity<InputState>,
    // turn
    hold_ms: Entity<InputState>,
    commit_ms: Entity<InputState>,
    idle_secs: Entity<InputState>,
    // audio
    input_dev: Entity<Sel>,
    output_dev: Entity<Sel>,
    input_names: Vec<SharedString>,
    output_names: Vec<SharedString>,
    silence_ms: Entity<InputState>,
    // prompt
    system_prompt: Entity<InputState>,
    assistant_name: Entity<InputState>,
    voice: Entity<VoiceView>,
    _subs: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let s = cx.global::<AppState>().settings.clone();
        let mut subs = vec![];

        // --- helpers ---
        let mut text = |cx: &mut Context<Self>, value: &str, placeholder: &str, apply: fn(&mut Settings, String)| -> Entity<InputState> {
            let st = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()).default_value(value.to_string()));
            subs.push(cx.subscribe(&st, move |_, st, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    let v = st.read(cx).value().to_string();
                    apply(&mut cx.global_mut::<AppState>().settings, v);
                    save_settings(cx);
                }
            }));
            st
        };
        let assistant_name = text(cx, &s.assistant_name, "Aura", |s, v| s.assistant_name = v);
        let llm_model = text(cx, &s.llm.model, "gpt-4o-mini", |s, v| s.llm.model = v);
        let fast_model = text(cx, &s.llm.fast_model, "gpt-4o-mini", |s, v| s.llm.fast_model = v);
        let stt_model = text(cx, &s.stt.model, "scribe_v2", |s, v| s.stt.model = v);
        let stt_lang = text(cx, &s.stt.language_code, "auto (or e.g. zh, en)", |s, v| s.stt.language_code = v);
        let el_model = text(cx, &s.tts.elevenlabs_model, "eleven_v3", |s, v| s.tts.elevenlabs_model = v);
        let el_voice = text(cx, &s.tts.elevenlabs_voice_id, "voice id", |s, v| s.tts.elevenlabs_voice_id = v);
        let oa_model = text(cx, &s.tts.openai_model, "tts-1", |s, v| s.tts.openai_model = v);
        let oa_voice = text(cx, &s.tts.openai_voice, "alloy", |s, v| s.tts.openai_voice = v);
        let hold_ms = text(cx, &s.turn.hold_ms.to_string(), "6000", |s, v| {
            if let Ok(n) = v.trim().parse() {
                s.turn.hold_ms = n
            }
        });
        let commit_ms = text(cx, &s.turn.commit_ms.to_string(), "1200", |s, v| {
            if let Ok(n) = v.trim().parse() {
                s.turn.commit_ms = n
            }
        });
        let idle_secs = text(cx, &s.turn.idle_nudge_secs.to_string(), "20", |s, v| {
            if let Ok(n) = v.trim().parse() {
                s.turn.idle_nudge_secs = n
            }
        });
        let silence_ms = text(cx, &s.audio.silence_ms.to_string(), "700", |s, v| {
            if let Ok(n) = v.trim().parse() {
                s.audio.silence_ms = n
            }
        });
        let system_prompt = cx.new(|cx| InputState::new(window, cx).multi_line(true).rows(4).default_value(s.system_prompt.clone()));
        subs.push(cx.subscribe(&system_prompt, |_, st, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.global_mut::<AppState>().settings.system_prompt = st.read(cx).value().to_string();
                save_settings(cx);
            }
        }));

        // keys: masked, never prefilled
        let mut key = |cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("paste key, press ⏎ to store"));
        let key_openai = key(cx);
        let key_anthropic = key(cx);
        let key_elevenlabs = key(cx);
        for (st, account) in [(&key_openai, "openai"), (&key_anthropic, "anthropic"), (&key_elevenlabs, "elevenlabs")] {
            subs.push(cx.subscribe(st, move |_, st, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    let v = st.read(cx).value().to_string();
                    match Keys::store(account, v.trim()) {
                        Ok(()) => {
                            cx.global_mut::<AppState>().keys = Keys::load();
                        }
                        Err(e) => tracing::error!("keychain: {e}"),
                    }
                    cx.notify();
                }
            }));
        }

        // selects
        let mut select = |cx: &mut Context<Self>, items: Vec<SharedString>, selected: usize, apply: fn(&mut Settings, &str)| -> Entity<Sel> {
            let st = cx.new(|cx| SelectState::new(SearchableVec::new(items), Some(IndexPath::new(selected)), window, cx));
            subs.push(cx.subscribe(&st, move |_, _, ev: &SelectEvent<SearchableVec<SharedString>>, cx| {
                let SelectEvent::Confirm(Some(v)) = ev else { return };
                apply(&mut cx.global_mut::<AppState>().settings, v.as_ref());
                save_settings(cx);
                cx.notify();
            }));
            st
        };
        let llm_provider = select(cx, vec!["openai".into(), "anthropic".into()], if s.llm.provider == LlmProvider::Anthropic { 1 } else { 0 }, |s, v| {
            s.llm.provider = if v == "anthropic" { LlmProvider::Anthropic } else { LlmProvider::OpenAi }
        });
        let efforts: Vec<SharedString> = EFFORTS.iter().map(|e| effort_label(*e)).collect();
        let idx = |e: Effort| EFFORTS.iter().position(|x| *x == e).unwrap_or(0);
        let llm_effort = select(cx, efforts.clone(), idx(s.llm.effort), |s, v| s.llm.effort = parse_effort(v));
        let fast_effort = select(cx, efforts, idx(s.llm.fast_effort), |s, v| s.llm.fast_effort = parse_effort(v));
        let tts_provider = select(cx, vec!["elevenlabs".into(), "openai".into()], if s.tts.provider == TtsProvider::OpenAi { 1 } else { 0 }, |s, v| {
            s.tts.provider = if v == "openai" { TtsProvider::OpenAi } else { TtsProvider::ElevenLabs }
        });
        let (ins, outs) = voice_audio::engine::list_devices().unwrap_or_default();
        let mut input_names: Vec<SharedString> = vec!["System default".into()];
        input_names.extend(ins.into_iter().map(SharedString::from));
        let mut output_names: Vec<SharedString> = vec!["System default".into()];
        output_names.extend(outs.into_iter().map(SharedString::from));
        let pos = |names: &[SharedString], cur: &Option<String>| cur.as_ref().and_then(|c| names.iter().position(|n| n.as_ref() == c)).unwrap_or(0);
        let input_dev = select(cx, input_names.clone(), pos(&input_names, &s.audio.input_device), |s, v| {
            s.audio.input_device = (v != "System default").then(|| v.to_string())
        });
        let output_dev = select(cx, output_names.clone(), pos(&output_names, &s.audio.output_device), |s, v| {
            s.audio.output_device = (v != "System default").then(|| v.to_string())
        });

        Self {
            key_openai,
            key_anthropic,
            key_elevenlabs,
            llm_provider,
            llm_model,
            llm_effort,
            fast_model,
            fast_effort,
            stt_model,
            stt_lang,
            tts_provider,
            el_model,
            el_voice,
            oa_model,
            oa_voice,
            hold_ms,
            commit_ms,
            idle_secs,
            input_dev,
            output_dev,
            input_names,
            output_names,
            silence_ms,
            system_prompt,
            assistant_name,
            voice: cx.new(|cx| VoiceView::new(window, cx)),
            _subs: subs,
        }
    }
}

fn parse_effort(v: &str) -> Effort {
    EFFORTS.iter().copied().find(|e| effort_label(*e).as_ref() == v).unwrap_or(Effort::Unset)
}

pub fn section(title: &'static str, _cx: &App) -> Div {
    v_flex().gap_3().p_5().rounded_lg().bg(c(PANEL)).border_1().border_color(c(BORDER)).child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(c(TEXT)).child(title))
}

pub fn row(label: &'static str, control: impl IntoElement, _cx: &App) -> impl IntoElement {
    h_flex().gap_3().items_center().child(div().w(px(200.)).flex_shrink_0().text_sm().text_color(c(TEXT_2)).child(label)).child(div().flex_1().min_w_0().child(control))
}

pub fn page_title(t: &'static str) -> Div {
    div().text_xl().font_weight(FontWeight::BOLD).text_color(c(TEXT)).child(t)
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (settings, keys) = {
            let g = cx.global::<AppState>();
            (g.settings.clone(), g.keys.clone())
        };
        let key_status = |present: bool| if present { "stored ✓" } else { "not set" };
        let _ = (&self.input_names, &self.output_names);

        div()
            .id("settings")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_4()
                    .p_6()
                    .max_w(px(760.))
                    .child(page_title("Settings"))
                    .child(
                        section("API keys", cx)
                            .child(div().flex_shrink_0().text_xs().text_color(c(TEXT_3)).child("Kept in a private file in this app's data folder (never in the settings file, and no keychain prompts). Env vars OPENAI_API_KEY / ANTHROPIC_API_KEY / ELEVENLABS_API_KEY also work."))
                            .child(row("ElevenLabs (STT + TTS)", h_flex().gap_2().items_center().child(Input::new(&self.key_elevenlabs).small()).child(div().text_xs().child(key_status(!keys.elevenlabs.is_empty()))), cx))
                            .child(row("OpenAI", h_flex().gap_2().items_center().child(Input::new(&self.key_openai).small()).child(div().text_xs().child(key_status(!keys.openai.is_empty()))), cx))
                            .child(row("Anthropic", h_flex().gap_2().items_center().child(Input::new(&self.key_anthropic).small()).child(div().text_xs().child(key_status(!keys.anthropic.is_empty()))), cx))
                            .child(
                                Button::new("clear-keys").ghost().small().label("Forget all keys").on_click(cx.listener(|_, _, window, cx| {
                                    for a in ["openai", "anthropic", "elevenlabs"] {
                                        let _ = Keys::store(a, "");
                                    }
                                    cx.global_mut::<AppState>().keys = Keys::load();
                                    window.push_notification(gpui_component::notification::Notification::info("Keys removed."), cx);
                                    cx.notify();
                                })),
                            ),
                    )
                    .child(
                        section("Assistant", cx)
                            .child(row("Name", Input::new(&self.assistant_name).small(), cx))
                            .child(row("System prompt", Input::new(&self.system_prompt), cx)),
                    )
                    .child(
                        section("Language model", cx)
                            .child(row("Provider", Select::new(&self.llm_provider).small(), cx))
                            .child(row("Model", Input::new(&self.llm_model).small(), cx))
                            .child(row("Reasoning effort", Select::new(&self.llm_effort).small(), cx))
                            .child(row("Fast model (turn-taking)", Input::new(&self.fast_model).small(), cx))
                            .child(row("Fast model effort", Select::new(&self.fast_effort).small(), cx)),
                    )
                    .child(
                        section("Speech", cx)
                            .child(row("STT model (ElevenLabs)", Input::new(&self.stt_model).small(), cx))
                            .child(row("STT language", Input::new(&self.stt_lang).small(), cx))
                            .child(row("TTS provider", Select::new(&self.tts_provider).small(), cx))
                            .child(row("ElevenLabs TTS model", Input::new(&self.el_model).small(), cx))
                            .child(row("ElevenLabs voice id", Input::new(&self.el_voice).small(), cx))
                            .child(row("OpenAI TTS model", Input::new(&self.oa_model).small(), cx))
                            .child(row("OpenAI voice", Input::new(&self.oa_voice).small(), cx)),
                    )
                    .child(
                        section("Turn-taking", cx)
                            .child(row(
                                "Semantic end-of-turn",
                                Switch::new("semantic").checked(settings.turn.semantic).label("Ask the fast model whether you've finished").on_click(cx.listener(|_, on: &bool, _, cx| {
                                    cx.global_mut::<AppState>().settings.turn.semantic = *on;
                                    save_settings(cx);
                                    cx.notify();
                                })),
                                cx,
                            ))
                            .child(row(
                                "Keeps the conversation going",
                                Switch::new("proactive").checked(settings.turn.proactive).label("Greets you at the start and follows up when you go quiet").on_click(cx.listener(|_, on: &bool, _, cx| {
                                    cx.global_mut::<AppState>().settings.turn.proactive = *on;
                                    save_settings(cx);
                                    cx.notify();
                                })),
                                cx,
                            ))
                            .child(row("Follow up after quiet (s)", Input::new(&self.idle_secs).small(), cx))
                            .child(row("Hold after “incomplete” (ms)", Input::new(&self.hold_ms).small(), cx))
                            .child(row("Interrupt commit (ms)", Input::new(&self.commit_ms).small(), cx))
                            .child(row(
                                "Interjections",
                                Switch::new("interj").checked(settings.turn.interjections).label("Let the fast model react to short remarks").on_click(cx.listener(|_, on: &bool, _, cx| {
                                    cx.global_mut::<AppState>().settings.turn.interjections = *on;
                                    save_settings(cx);
                                    cx.notify();
                                })),
                                cx,
                            )),
                    )
                    .child(
                        section("Audio", cx)
                            .child(row("Microphone", Select::new(&self.input_dev).small(), cx))
                            .child(row("Speaker", Select::new(&self.output_dev).small(), cx))
                            .child(row("VAD silence (ms)", Input::new(&self.silence_ms).small(), cx))
                            .child(row(
                                "Noise suppression",
                                Switch::new("ns").checked(settings.audio.noise_suppression).on_click(cx.listener(|_, on: &bool, _, cx| {
                                    cx.global_mut::<AppState>().settings.audio.noise_suppression = *on;
                                    save_settings(cx);
                                    cx.notify();
                                })),
                                cx,
                            ))
                            .child(row(
                                "Auto gain",
                                Switch::new("agc").checked(settings.audio.agc).on_click(cx.listener(|_, on: &bool, _, cx| {
                                    cx.global_mut::<AppState>().settings.audio.agc = *on;
                                    save_settings(cx);
                                    cx.notify();
                                })),
                                cx,
                            ))
                            .child(div().text_xs().text_color(c(TEXT_3)).child("Echo cancellation is always on. Device changes apply to the next call.")),
                    )
                    .child(self.voice.clone()),
            )
    }
}
