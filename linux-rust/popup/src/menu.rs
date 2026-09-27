//! The bar dropdown: a small macOS-style AirPods menu hanging under the
//! Waybar icon (header, batteries, listening mode, Conversation
//! Awareness, then plain menu rows).
//!
//! Layout and hit-testing live here, in panel-local logical px. The panel
//! sits in a slightly larger canvas (room for its shadow and the drop-in),
//! which the Wayland side maps as a subsurface of a full-output
//! transparent layer.

use std::hash::{DefaultHasher, Hash, Hasher};

use tiny_skia::{Path, PathBuilder, Pixmap, PixmapMut, Transform};

use crate::draw::*;
use crate::protocol::{Batt, Device, LinkState, Mode, Product};
use crate::render::{Format, Layer, arc, blend_over, copy_frame};
use crate::text::{self, Align, Style, Weight};
use crate::theme::*;

pub const PANEL_W: f32 = 340.0;
/// Bar: 32 px tall with a 6 px top margin, plus a little air.
pub const PANEL_TOP: f32 = 44.0;
/// Minimum distance from the output's left/right edges.
pub const EDGE: f32 = 10.0;
/// Canvas room around the panel for the shadow (and the 6 px drop).
pub const SP: f32 = 22.0;
const RADIUS: f32 = 16.0;
const PX: f32 = 14.0;
/// Slide distance of the drop-in.
pub const DROP: f32 = 6.0;

const ICON_C: (f32, f32) = (PX + 18.0, 14.0 + 18.0);
const ICON_R: f32 = 18.0;
const TEXT_X: f32 = 60.0;
const NAME_Y: f32 = 29.5;
const STATUS_Y: f32 = 47.0;
const BATT_CY: f32 = 84.0;
const BATT_R: f32 = 11.0;
const SECTION_Y: f32 = 128.0;
const MODE_CY: f32 = 159.0;
const MODE_R: f32 = 22.0;
const MODE_LABEL_Y: f32 = 197.0;
const CA_TOP: f32 = 222.0;
const CA_H: f32 = 32.0;
const DIVIDER_Y: f32 = 263.0;
const ROW_TOP: f32 = 269.0;
const ROW_H: f32 = 28.0;
pub const PANEL_H: f32 = ROW_TOP + 3.0 * ROW_H + 7.0;

/// Left edge of the panel on an output `out_w` wide: centred under the
/// clicked icon, kept `EDGE` px from both sides; no anchor → right edge.
pub fn panel_x(anchor_x: Option<f32>, out_w: f32) -> f32 {
    let max = (out_w - EDGE - PANEL_W).max(EDGE);
    let x = match anchor_x {
        Some(a) if a.is_finite() => a - PANEL_W * 0.5,
        _ => max,
    };
    x.clamp(EDGE, max).round()
}

/// Turn the daemon's `anchor_x` into output-local px. It may come in
/// global layout coordinates; on a second monitor that is past the
/// output's width, so subtract the output's position.
pub fn local_anchor(anchor_x: Option<f32>, out_x: i32, out_w: f32) -> Option<f32> {
    anchor_x.map(|a| if a >= out_w && out_x > 0 { a - out_x as f32 } else { a })
}

pub fn canvas_size() -> (u32, u32) {
    ((PANEL_W + 2.0 * SP) as u32, (PANEL_H + 2.0 * SP) as u32)
}

/// Canvas (subsurface) position within the menu surface.
pub fn canvas_pos(panel_x: f32) -> (i32, i32) {
    ((panel_x - SP) as i32, (PANEL_TOP - SP) as i32)
}

/// Clickable part of the canvas: the panel itself (its shadow is
/// "outside", so a click there closes the menu).
pub fn canvas_input() -> (i32, i32, i32, i32) {
    (SP as i32, SP as i32, PANEL_W as i32, PANEL_H as i32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Row {
    /// Connect / Disconnect.
    Link,
    Bluetooth,
    Settings,
}

pub const ROWS: [Row; 3] = [Row::Link, Row::Bluetooth, Row::Settings];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuHit {
    /// Outside the panel: closes the menu.
    Outside,
    /// Inside, but nothing to click.
    Inert,
    Mode(Mode),
    Ca,
    Row(Row),
}

impl MenuHit {
    pub fn clickable(self) -> bool {
        matches!(self, MenuHit::Mode(_) | MenuHit::Ca | MenuHit::Row(_))
    }
}

#[derive(Clone, Debug, PartialEq, Hash)]
pub struct MenuView {
    pub title: String,
    pub max: bool,
    pub link: LinkState,
    pub batts: Vec<(&'static str, Batt)>,
    pub modes: Vec<Mode>,
    pub mode: Option<Mode>,
    pub ca: bool,
    /// Connected: mode and CA controls are live.
    pub enabled: bool,
    pub hover: MenuHit,
    pub pressed: Option<MenuHit>,
}

impl MenuView {
    pub fn new(dev: &Device, hover: MenuHit, pressed: Option<MenuHit>) -> Self {
        let max = dev.product() == Product::Max || dev.headphone.is_some();
        let batts = if max {
            vec![("Battery", dev.headphone.or(dev.left).or(dev.right).unwrap_or_default())]
        } else {
            vec![
                ("Left", dev.left.unwrap_or_default()),
                ("Right", dev.right.unwrap_or_default()),
                ("Case", dev.case.unwrap_or_default()),
            ]
        };
        MenuView {
            title: dev.title(),
            max,
            link: dev.link_state(),
            batts,
            modes: dev.modes(),
            mode: dev.mode(),
            ca: dev.conversational_awareness == Some(true),
            enabled: dev.connected,
            hover,
            pressed,
        }
    }

    fn row_label(&self, r: Row) -> &'static str {
        match r {
            Row::Link if self.link == LinkState::Connected => "Disconnect",
            Row::Link => "Connect",
            Row::Bluetooth => "Bluetooth Settings…",
            Row::Settings => "AirPods Settings…",
        }
    }
}

fn mode_col(n: usize) -> f32 {
    (PANEL_W - 2.0 * PX) / n.max(1) as f32
}

fn mode_cx(i: usize, n: usize) -> f32 {
    PX + mode_col(n) * (i as f32 + 0.5)
}

fn row_top(i: usize) -> f32 {
    ROW_TOP + ROW_H * i as f32
}

/// What is under panel-local point (x, y)?
pub fn hit(v: &MenuView, x: f32, y: f32) -> MenuHit {
    if !(0.0..PANEL_W).contains(&x) || !(0.0..PANEL_H).contains(&y) {
        return MenuHit::Outside;
    }
    if v.enabled {
        let n = v.modes.len();
        if (MODE_CY - MODE_R - 4.0..MODE_LABEL_Y + 16.0).contains(&y) && (PX..PANEL_W - PX).contains(&x) {
            let i = ((x - PX) / mode_col(n)).floor() as usize;
            return v.modes.get(i).map_or(MenuHit::Inert, |m| MenuHit::Mode(*m));
        }
        if (CA_TOP..CA_TOP + CA_H).contains(&y) && (6.0..PANEL_W - 6.0).contains(&x) {
            return MenuHit::Ca;
        }
    }
    if (6.0..PANEL_W - 6.0).contains(&x) {
        for (i, r) in ROWS.iter().enumerate() {
            if (row_top(i)..row_top(i) + ROW_H).contains(&y) {
                return MenuHit::Row(*r);
            }
        }
    }
    MenuHit::Inert
}

// ─────────────────────────────── drawing ────────────────────────────────

const GLYPH_EARBUDS: char = '\u{F184F}';
const GLYPH_HEADPHONES: char = '\u{F02CB}';
const GLYPH_BOLT: char = '\u{F140B}';

fn mode_glyph(m: Mode) -> Option<char> {
    match m {
        Mode::Off => None,
        Mode::Anc => Some('\u{F0A45}'),
        Mode::Transparency => Some('\u{F07C5}'),
        Mode::Adaptive => Some('\u{F0674}'),
    }
}

fn panel_path() -> Path {
    rrect(0.0, 0.0, PANEL_W, PANEL_H, RADIUS)
}

/// Split a label into at most two lines that fit `max_w`.
fn wrap2(text: &str, max_w: f32, st: &Style) -> Vec<String> {
    if text::measure(text, st) <= max_w {
        return vec![text.to_string()];
    }
    let words: Vec<&str> = text.split(' ').collect();
    for k in (1..words.len()).rev() {
        let (a, b) = (words[..k].join(" "), words[k..].join(" "));
        if text::measure(&a, st) <= max_w && text::measure(&b, st) <= max_w {
            return vec![a, b];
        }
    }
    vec![text::fit(text, max_w, st, st.size).0]
}

fn level_color(level: Option<u8>) -> tiny_skia::Color {
    match level {
        Some(l) if l <= 10 => red(),
        Some(l) if l <= 20 => gold(),
        _ => green(),
    }
}

fn draw_panel(pm: &mut PixmapMut, s: f32, v: &MenuView) {
    let ts = Transform::from_scale(s, s);
    let body = panel_path();
    // Subsurfaces aren't blurred by Hyprland, so stay nearly opaque (a solid macOS menu look).
    fill(pm, &body, &solid(with_alpha(ember_dark(), 0.97)), ts);
    let clip = mask(pm, &body, ts);
    stroke_in(pm, &body, 2.0, &solid(rgba(232, 155, 94, 0.35)), ts, &clip);

    // Header.
    fill(pm, &circle(ICON_C.0, ICON_C.1, ICON_R), &solid(cream(0.08)), ts);
    let glyph = if v.max { GLYPH_HEADPHONES } else { GLYPH_EARBUDS };
    text::draw_icon(pm, s, glyph, ICON_C.0, ICON_C.1, 17.0, text_primary());
    let name = Style::new(15.0, Weight::Semibold, text_primary()).tracking(-0.1);
    let (title, name) = text::fit(&v.title, PANEL_W - TEXT_X - PX, &name, 12.0);
    text::draw(pm, s, &title, TEXT_X, NAME_Y, Align::Left, &name);
    let (status, color) = match v.link {
        LinkState::Connected => ("Connected", blue()),
        LinkState::Elsewhere => ("On another device", text_tertiary()),
        LinkState::NotConnected => ("Not connected", text_tertiary()),
    };
    text::draw(pm, s, status, TEXT_X, STATUS_Y, Align::Left, &Style::new(12.0, Weight::Medium, color));

    // Batteries.
    let n = v.batts.len();
    let col = (PANEL_W - 2.0 * PX) / 3.0;
    let start = PX + (PANEL_W - 2.0 * PX - col * n as f32) * if n < 3 { 0.0 } else { 0.5 };
    for (i, (label, b)) in v.batts.iter().enumerate() {
        let x0 = start + col * i as f32;
        let cx = x0 + BATT_R + 2.0;
        if let Some(track) = arc(cx, BATT_CY, BATT_R, -90.0, 270.0) {
            stroke_butt(pm, &track, 3.0, cream(0.12), ts);
        }
        if let Some(l) = b.level
            && l > 0
            && let Some(p) = arc(cx, BATT_CY, BATT_R, -90.0, -90.0 + 360.0 * l as f32 / 100.0)
        {
            stroke(pm, &p, 3.0, &solid(level_color(Some(l))), ts);
        }
        if b.charging && !text::draw_icon(pm, s, GLYPH_BOLT, cx, BATT_CY, 10.0, green()) {
            fill(pm, &crate::render::bolt(cx, BATT_CY, 10.0), &solid(green()), ts);
        }
        let tx = cx + BATT_R + 8.0;
        text::draw(pm, s, label, tx, BATT_CY - 3.0, Align::Left, &Style::new(11.0, Weight::Medium, text_secondary()));
        match b.level {
            Some(l) => {
                let st = Style::new(13.0, Weight::Semibold, text_primary());
                text::draw(pm, s, &format!("{l}%"), tx, BATT_CY + 12.5, Align::Left, &st);
            }
            None => {
                let st = Style::new(13.0, Weight::Semibold, text_tertiary());
                text::draw(pm, s, "—", tx, BATT_CY + 12.5, Align::Left, &st);
            }
        }
    }

    // Listening mode.
    let (section, sc) = if v.enabled {
        ("Listening Mode", text_tertiary())
    } else {
        ("Connect to change listening mode", text_tertiary())
    };
    text::draw(pm, s, section, PX, SECTION_Y, Align::Left, &Style::new(11.0, Weight::Semibold, sc));
    let dim = if v.enabled { 1.0 } else { 0.4 };
    let n = v.modes.len();
    for (i, m) in v.modes.iter().enumerate() {
        let cx = mode_cx(i, n);
        let selected = v.mode == Some(*m);
        let state = press_of(v, MenuHit::Mode(*m));
        let bg = if selected {
            match state {
                Press::Down => rgb(0xd6, 0x88, 0x4c),
                Press::Hover => rgb(0xef, 0xab, 0x75),
                Press::Idle => amber(),
            }
        } else {
            cream(match state {
                Press::Down => 0.20,
                Press::Hover => 0.14,
                Press::Idle => 0.08,
            })
        };
        let fg = if selected { ember_dark() } else { text_primary() };
        fill(pm, &circle(cx, MODE_CY, MODE_R), &solid(dimmed(bg, dim)), ts);
        match mode_glyph(*m) {
            Some(g) => {
                text::draw_icon(pm, s, g, cx, MODE_CY, 20.0, dimmed(fg, dim));
            }
            None => off_glyph(pm, ts, cx, MODE_CY, dimmed(fg, dim)),
        }
        let st = Style::new(10.5, Weight::Medium, dimmed(if selected { text_primary() } else { text_secondary() }, dim));
        for (k, line) in wrap2(m.label(), mode_col(n) - 4.0, &st).iter().enumerate() {
            text::draw(pm, s, line, cx, MODE_LABEL_Y + 12.5 * k as f32, Align::Center, &st);
        }
    }

    // Conversation Awareness.
    if v.enabled && press_of(v, MenuHit::Ca) != Press::Idle {
        let a = if press_of(v, MenuHit::Ca) == Press::Down { 0.12 } else { 0.07 };
        fill(pm, &rrect(6.0, CA_TOP, PANEL_W - 12.0, CA_H, 6.0), &solid(cream(a)), ts);
    }
    let st = Style::new(13.0, Weight::Medium, dimmed(text_primary(), dim));
    text::draw(pm, s, "Conversation Awareness", PX, CA_TOP + CA_H * 0.5 + 4.6, Align::Left, &st);
    let (sw, sh) = (36.0, 20.0);
    let (sx, sy) = (PANEL_W - PX - sw, CA_TOP + (CA_H - sh) * 0.5);
    let track = if v.ca { amber() } else { cream(0.16) };
    fill(pm, &rrect(sx, sy, sw, sh, sh * 0.5), &solid(dimmed(track, dim)), ts);
    let kx = if v.ca { sx + sw - sh * 0.5 } else { sx + sh * 0.5 };
    fill(pm, &circle(kx, sy + sh * 0.5 + 0.6, sh * 0.5 - 2.0), &solid(dimmed(black(0.25), dim)), ts);
    fill(pm, &circle(kx, sy + sh * 0.5, sh * 0.5 - 2.0), &solid(dimmed(text_primary(), dim)), ts);

    // Divider and menu rows.
    fill(pm, &rect(PX, DIVIDER_Y, PANEL_W - 2.0 * PX, 1.0), &solid(cream(0.10)), ts);
    for (i, r) in ROWS.iter().enumerate() {
        let top = row_top(i);
        match press_of(v, MenuHit::Row(*r)) {
            Press::Idle => {}
            p => {
                let a = if p == Press::Down { 0.13 } else { 0.08 };
                fill(pm, &rrect(6.0, top, PANEL_W - 12.0, ROW_H, 6.0), &solid(cream(a)), ts);
            }
        }
        let st = Style::new(13.0, Weight::Regular, text_primary());
        text::draw(pm, s, v.row_label(*r), 18.0, top + ROW_H * 0.5 + 4.6, Align::Left, &st);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Press {
    Idle,
    Hover,
    Down,
}

fn press_of(v: &MenuView, target: MenuHit) -> Press {
    if v.hover != target {
        Press::Idle
    } else if v.pressed == Some(target) {
        Press::Down
    } else {
        Press::Hover
    }
}

fn dimmed(c: tiny_skia::Color, k: f32) -> tiny_skia::Color {
    with_alpha(c, c.alpha() * k)
}

fn stroke_butt(pm: &mut PixmapMut, p: &Path, w: f32, c: tiny_skia::Color, ts: Transform) {
    let st = tiny_skia::Stroke {
        width: w,
        ..Default::default()
    };
    pm.stroke_path(p, &solid(c), &st, ts, None);
}

/// "Off": a circle with a slash, the usual none-symbol.
fn off_glyph(pm: &mut PixmapMut, ts: Transform, cx: f32, cy: f32, c: tiny_skia::Color) {
    stroke(pm, &circle(cx, cy, 7.5), 2.0, &solid(c), ts);
    let mut pb = PathBuilder::new();
    pb.move_to(cx - 5.3, cy + 5.3);
    pb.line_to(cx + 5.3, cy - 5.3);
    stroke(pm, &pb.finish().expect("slash"), 2.0, &solid(c), ts);
}

// ─────────────────────────────── renderer ───────────────────────────────

#[derive(Default)]
pub struct MenuRenderer {
    shadow: Option<(u64, Vec<u8>)>,
    layer: Option<(u64, Pixmap)>,
}

fn origin_px(scale: f32) -> i32 {
    (SP * scale).round() as i32
}

impl MenuRenderer {
    /// Render the canvas (`w`×`h` px) at drop-in progress `vis` (0 hidden …
    /// 1 at rest): the panel fades in while dropping `DROP` px.
    #[allow(clippy::too_many_arguments)]
    pub fn render(&mut self, out: &mut [u8], w: u32, h: u32, fmt: Format, scale: f32, v: &MenuView, vis: f32) {
        let sk = ((w as u64) << 40) ^ ((h as u64) << 20) ^ scale.to_bits() as u64;
        if self.shadow.as_ref().is_none_or(|(k, _)| *k != sk) {
            let (cw, ch) = canvas_size();
            let o = origin_px(scale) as f32;
            let shape = panel_path().transform(Transform::from_translate(SP, SP)).expect("path");
            let sh = soft_shadow(
                (cw, ch),
                &shape,
                &[(5.0, 9.0, 0.30), (1.0, 1.0, 0.12)],
                (w, h),
                &panel_path(),
                Transform::from_translate(o, o).pre_scale(scale, scale),
            );
            self.shadow = Some((sk, sh));
            self.layer = None;
        }
        let key = {
            let mut hs = DefaultHasher::new();
            v.hash(&mut hs);
            sk.hash(&mut hs);
            hs.finish()
        };
        if self.layer.as_ref().is_none_or(|(k, _)| *k != key) {
            let mut layer = Pixmap::new(w, h).expect("menu layer");
            let shadow = &self.shadow.as_ref().expect("shadow").1;
            for (px, &a) in layer.data_mut().chunks_exact_mut(4).zip(shadow) {
                px[3] = a;
            }
            let (pw, ph) = ((PANEL_W * scale).ceil() as u32, (PANEL_H * scale).ceil() as u32);
            let mut panel = Pixmap::new(pw, ph).expect("panel");
            draw_panel(&mut panel.as_mut(), scale, v);
            let o = origin_px(scale);
            let src = Layer {
                data: panel.data(),
                w: pw as usize,
                h: ph as usize,
            };
            blend_over::<false>(layer.data_mut(), w as usize, h as usize, &src, o, o, 256);
            self.layer = Some((key, layer));
        }
        let layer = &self.layer.as_ref().expect("layer").1;
        let dy = (-(1.0 - vis.clamp(0.0, 1.0)) * DROP * scale).round() as i32;
        let alpha = (vis.clamp(0.0, 1.0) * 256.0).round() as u32;
        copy_frame(out, layer.data(), w as usize, h as usize, dy, alpha, fmt == Format::Bgra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::demo_device;

    fn view(connected: bool) -> MenuView {
        let mut d = demo_device(None);
        d.connected = connected;
        d.allow_off = Some(true);
        MenuView::new(&d, MenuHit::Outside, None)
    }

    #[test]
    fn placement_centres_and_clamps() {
        let w = 1920.0;
        // Centred under the icon.
        assert_eq!(panel_x(Some(1000.0), w), 1000.0 - PANEL_W / 2.0);
        // Near the right edge: 10 px from it.
        assert_eq!(panel_x(Some(1915.0), w), w - EDGE - PANEL_W);
        // Near the left edge: 10 px from it.
        assert_eq!(panel_x(Some(5.0), w), EDGE);
        // No anchor: right edge.
        assert_eq!(panel_x(None, w), w - EDGE - PANEL_W);
        assert_eq!(panel_x(Some(f32::NAN), w), w - EDGE - PANEL_W);
        // An output narrower than the panel: pinned left, never negative.
        assert_eq!(panel_x(Some(100.0), 300.0), EDGE);
        // Canvas includes the shadow room.
        let (cx, cy) = canvas_pos(500.0);
        assert_eq!((cx, cy), (500 - SP as i32, (PANEL_TOP - SP) as i32));
    }

    #[test]
    fn global_anchor_on_second_monitor() {
        assert_eq!(local_anchor(Some(2500.0), 1920, 1920.0), Some(580.0));
        assert_eq!(local_anchor(Some(580.0), 1920, 1920.0), Some(580.0));
        assert_eq!(local_anchor(Some(1700.0), 0, 1920.0), Some(1700.0));
        assert_eq!(local_anchor(None, 1920, 1920.0), None);
    }

    #[test]
    fn hit_testing() {
        let v = view(true);
        assert_eq!(v.modes.len(), 4);
        assert_eq!(hit(&v, -1.0, 10.0), MenuHit::Outside);
        assert_eq!(hit(&v, 10.0, PANEL_H + 1.0), MenuHit::Outside);
        assert_eq!(hit(&v, 100.0, NAME_Y), MenuHit::Inert);
        for (i, m) in v.modes.iter().enumerate() {
            assert_eq!(hit(&v, mode_cx(i, 4), MODE_CY), MenuHit::Mode(*m));
            assert_eq!(hit(&v, mode_cx(i, 4), MODE_LABEL_Y + 5.0), MenuHit::Mode(*m), "labels click too");
        }
        assert_eq!(hit(&v, PANEL_W - 30.0, CA_TOP + CA_H * 0.5), MenuHit::Ca);
        for (i, r) in ROWS.iter().enumerate() {
            assert_eq!(hit(&v, PANEL_W * 0.5, row_top(i) + ROW_H * 0.5), MenuHit::Row(*r));
        }
    }

    #[test]
    fn disconnected_controls_are_inert() {
        let v = view(false);
        assert_eq!(hit(&v, mode_cx(0, v.modes.len()), MODE_CY), MenuHit::Inert);
        assert_eq!(hit(&v, PANEL_W - 30.0, CA_TOP + CA_H * 0.5), MenuHit::Inert);
        assert_eq!(hit(&v, PANEL_W * 0.5, row_top(0) + 5.0), MenuHit::Row(Row::Link));
        assert_eq!(v.row_label(Row::Link), "Connect");
        assert_eq!(view(true).row_label(Row::Link), "Disconnect");
    }

    #[test]
    fn labels_wrap_to_two_lines() {
        let st = Style::new(10.5, Weight::Medium, text_secondary());
        assert_eq!(wrap2("Noise Cancellation", 70.0, &st), ["Noise", "Cancellation"]);
        assert_eq!(wrap2("Off", 70.0, &st), ["Off"]);
    }

    #[test]
    fn renders_and_fades() {
        let v = view(true);
        let (cw, ch) = canvas_size();
        let mut r = MenuRenderer::default();
        let s = 2.0;
        let (w, h) = ((cw as f32 * s) as u32, (ch as f32 * s) as u32);
        let mut full = vec![0u8; (w * h * 4) as usize];
        r.render(&mut full, w, h, Format::Rgba, s, &v, 1.0);
        let at = |buf: &[u8], x: f32, y: f32| buf[(((y * s) as u32 * w + (x * s) as u32) * 4 + 3) as usize];
        assert!(at(&full, SP + 40.0, SP + 60.0) > 230, "panel is (nearly) opaque");
        assert_eq!(at(&full, 1.0, 1.0), 0, "canvas corner is clear");
        let mut half = vec![0u8; full.len()];
        r.render(&mut half, w, h, Format::Rgba, s, &v, 0.5);
        assert!(at(&half, SP + 40.0, SP + 100.0) < at(&full, SP + 40.0, SP + 100.0));
    }
}
