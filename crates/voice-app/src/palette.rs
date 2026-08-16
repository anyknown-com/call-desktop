//! Vercel-style theme: pure black ground, near-black panels, hairline greys, high-contrast text,
//! one blue accent, mono numerals.

use gpui::{rgb, Hsla, Rgba};

pub const BG: u32 = 0x000000;
pub const PANEL: u32 = 0x0A0A0A;
pub const ELEVATED: u32 = 0x111111;
pub const HOVER: u32 = 0x1A1A1A;
pub const BORDER: u32 = 0x262626;
pub const BORDER_STRONG: u32 = 0x333333;
pub const TEXT: u32 = 0xEDEDED;
pub const TEXT_2: u32 = 0xA1A1A1;
pub const TEXT_3: u32 = 0x666666;
pub const ACCENT: u32 = 0x0070F3;
pub const ACCENT_TEXT: u32 = 0xFFFFFF;
pub const SUCCESS: u32 = 0x45A557;
pub const WARN: u32 = 0xF5A623;
pub const DANGER: u32 = 0xE5484D;
pub const PURPLE: u32 = 0x8E4EC6;

pub const MONO: &str = "Geist Mono";
pub const SANS: &str = "Geist";

pub fn c(v: u32) -> Hsla {
    let r: Rgba = rgb(v);
    r.into()
}

pub fn with_alpha(v: u32, a: f32) -> Hsla {
    let mut h = c(v);
    h.a = a;
    h
}
