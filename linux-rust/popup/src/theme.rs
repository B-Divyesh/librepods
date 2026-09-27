//! The user's "Ember" desktop theme, shared with Waybar, mako and the
//! LibrePods window. Used by both the card and the menu.

use tiny_skia::Color;

use crate::draw::{rgb, rgba};

/// #1a141b — card background and text on amber.
pub fn ember_dark() -> Color {
    rgb(0x1a, 0x14, 0x1b)
}
/// #f0e4d4
pub fn text_primary() -> Color {
    rgb(0xf0, 0xe4, 0xd4)
}
/// #bfa998
pub fn text_secondary() -> Color {
    rgb(0xbf, 0xa9, 0x98)
}
/// #7d6a66
pub fn text_tertiary() -> Color {
    rgb(0x7d, 0x6a, 0x66)
}
/// rgba(240,228,212,a) — neutral fills and ring tracks.
pub fn cream(a: f32) -> Color {
    rgba(0xf0, 0xe4, 0xd4, a)
}
/// #e89b5e — accent.
pub fn amber() -> Color {
    rgb(0xe8, 0x9b, 0x5e)
}
/// #a8c686
pub fn green() -> Color {
    rgb(0xa8, 0xc6, 0x86)
}
/// #efcf9f
pub fn gold() -> Color {
    rgb(0xef, 0xcf, 0x9f)
}
/// #e5534b
pub fn red() -> Color {
    rgb(0xe5, 0x53, 0x4b)
}
/// #8cb3d9
pub fn blue() -> Color {
    rgb(0x8c, 0xb3, 0xd9)
}

/// Same thresholds as the Waybar battery module.
pub fn ring_color(level: Option<u8>) -> Color {
    match level {
        Some(l) if l <= 10 => red(),
        Some(l) if l <= 20 => gold(),
        _ => green(),
    }
}

