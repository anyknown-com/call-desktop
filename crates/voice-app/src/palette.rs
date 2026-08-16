//! The app's own look: a dim studio. Graphite-blue ground, ivory ink, and one live colour — the
//! "on-air" light — that follows the call state. Everything else stays quiet.

use gpui::{rgb, Hsla, Rgba};

pub const INK: u32 = 0x12141A; // window ground
pub const PANEL: u32 = 0x191C24; // cards, dock
pub const HAIRLINE: u32 = 0x262A34;
pub const IVORY: u32 = 0xEDE7DA; // primary text
pub const IVORY_DIM: u32 = 0xB6B1A6;
pub const MUTED: u32 = 0x7C8190;
pub const SAGE: u32 = 0x8CBF9F; // listening
pub const AMBER: u32 = 0xE9A94F; // speaking (on air)
pub const LAVENDER: u32 = 0xA99EE3; // thinking / transcribing
pub const CORAL: u32 = 0xE0705F; // you're speaking / hang up
pub const IDLE: u32 = 0x4B505C;

pub fn c(v: u32) -> Hsla {
    let r: Rgba = rgb(v);
    r.into()
}

pub fn with_alpha(v: u32, a: f32) -> Hsla {
    let mut h = c(v);
    h.a = a;
    h
}

pub const DISPLAY_FONT: &str = "Baskerville";
