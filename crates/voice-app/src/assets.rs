//! Inline SVG icons (lucide-style, stroke = currentColor) served to GPUI as an asset source.
//! Includes the handful gpui-component's own widgets ask for.

use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

pub struct Assets;

const fn svg(body: &'static str) -> &'static str {
    body
}

macro_rules! icon {
    ($body:expr) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"#,
            $body,
            "</svg>"
        )
    };
}

fn lookup(path: &str) -> Option<&'static str> {
    Some(match path {
        "icons/mic.svg" => icon!(r#"<path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" x2="12" y1="19" y2="22"/>"#),
        "icons/mic-off.svg" => icon!(r#"<line x1="2" x2="22" y1="2" y2="22"/><path d="M18.89 13.23A7.12 7.12 0 0 0 19 12v-2"/><path d="M5 10v2a7 7 0 0 0 12 5"/><path d="M15 9.34V5a3 3 0 0 0-5.68-1.33"/><path d="M9 9v3a3 3 0 0 0 5.12 2.12"/><line x1="12" x2="12" y1="19" y2="22"/>"#),
        "icons/volume-2.svg" => icon!(r#"<polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/>"#),
        "icons/volume-x.svg" => icon!(r#"<polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><line x1="22" x2="16" y1="9" y2="15"/><line x1="16" x2="22" y1="9" y2="15"/>"#),
        "icons/audio-lines.svg" => icon!(r#"<path d="M2 10v3"/><path d="M6 6v11"/><path d="M10 3v18"/><path d="M14 8v7"/><path d="M18 5v13"/><path d="M22 10v3"/>"#),
        "icons/phone.svg" => icon!(r#"<path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6 19.79 19.79 0 0 1-3.07-8.67A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91a16 16 0 0 0 6 6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7A2 2 0 0 1 22 16.92z"/>"#),
        "icons/phone-off.svg" => icon!(r#"<path d="M10.68 13.31a16 16 0 0 0 3.41 2.6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7 2 2 0 0 1 1.72 2v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.42 19.42 0 0 1-3.33-2.67m-2.67-3.34a19.79 19.79 0 0 1-3.07-8.63A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91"/><line x1="22" x2="2" y1="2" y2="22"/>"#),
        "icons/settings.svg" => icon!(r#"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>"#),
        "icons/plus.svg" => icon!(r#"<path d="M5 12h14"/><path d="M12 5v14"/>"#),
        "icons/clock.svg" => icon!(r#"<circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/>"#),
        "icons/folder.svg" => icon!(r#"<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>"#),
        "icons/file-text.svg" => icon!(r#"<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/><path d="M10 9H8"/><path d="M16 13H8"/><path d="M16 17H8"/>"#),
        "icons/arrow-left.svg" => icon!(r#"<path d="m12 19-7-7 7-7"/><path d="M19 12H5"/>"#),
        "icons/waveform.svg" => icon!(r#"<path d="M2 12h2"/><path d="M6 8v8"/><path d="M10 4v16"/><path d="M14 7v10"/><path d="M18 10v4"/><path d="M22 12h-2"/>"#),
        // used by gpui-component widgets
        "icons/chevron-down.svg" => icon!(r#"<path d="m6 9 6 6 6-6"/>"#),
        "icons/chevron-right.svg" => icon!(r#"<path d="m9 18 6-6-6-6"/>"#),
        "icons/chevron-up.svg" => icon!(r#"<path d="m18 15-6-6-6 6"/>"#),
        "icons/chevrons-up-down.svg" => icon!(r#"<path d="m7 15 5 5 5-5"/><path d="m7 9 5-5 5 5"/>"#),
        "icons/check.svg" => icon!(r#"<path d="M20 6 9 17l-5-5"/>"#),
        "icons/close.svg" | "icons/x.svg" => icon!(r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#),
        "icons/search.svg" => icon!(r#"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>"#),
        "icons/loader-circle.svg" => icon!(r#"<path d="M21 12a9 9 0 1 1-6.219-8.56"/>"#),
        "icons/eye.svg" => icon!(r#"<path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7Z"/><circle cx="12" cy="12" r="3"/>"#),
        "icons/eye-off.svg" => icon!(r#"<path d="M9.88 9.88a3 3 0 1 0 4.24 4.24"/><path d="M10.73 5.08A10.43 10.43 0 0 1 12 5c7 0 10 7 10 7a13.16 13.16 0 0 1-1.67 2.68"/><path d="M6.61 6.61A13.526 13.526 0 0 0 2 12s3 7 10 7a9.74 9.74 0 0 0 5.39-1.61"/><line x1="2" x2="22" y1="2" y2="22"/>"#),
        _ => return None,
    })
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(lookup(path).map(|s| Cow::Borrowed(svg(s).as_bytes())))
    }
    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}
