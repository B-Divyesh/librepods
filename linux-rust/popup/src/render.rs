//! The card: layout, hit-testing and rendering.
//!
//! Adapted from podctl's `src/bin/popup/render.rs`
//! (https://github.com/Rockykln/podctl, MIT OR Apache-2.0,
//! © 2026 pods contributors): the tiny-skia card with a gradient body and
//! hairline highlight, battery donuts with a colour ramp, and fitted
//! titles. Reworked into the iOS layout (title, illustration, ring row,
//! action button), rendered at any output scale, and split into cached
//! layers so animation frames only redraw what moves.
//!
//! All geometry is in logical pixels; `scale` maps to buffer pixels.

use std::hash::{DefaultHasher, Hash, Hasher};

use tiny_skia::{
    Color, Mask, Path, PathBuilder, Pixmap, PixmapMut, PixmapPaint, Transform,
};

use crate::art::{self, Led};
use crate::draw::*;
use crate::protocol::{Batt, Product, RingId};
use crate::text::{self, Align, Style, Weight};

pub const CARD_W: f32 = 360.0;
pub const CARD_H: f32 = 356.0;
/// Transparent room around the card for its shadow (left, right, top).
pub const PAD: f32 = 36.0;
/// Card bottom → surface bottom. The surface sits on the bottom edge of
/// the usable area, so this is the visible margin above the bar/edge.
pub const GAP: f32 = 24.0;
/// Matches the desktop's rounded boxes (Waybar, mako).
const RADIUS: f32 = 22.0;

const TITLE_Y: f32 = 47.0;
const CLOSE_C: (f32, f32) = (CARD_W - 30.0, 30.0);
const CLOSE_R: f32 = 14.0;
const ART: (f32, f32, f32, f32) = (32.0, 70.0, 296.0, 118.0);
const RING_Y: f32 = 232.0;
const RING_R: f32 = 22.5;
const RING_W: f32 = 5.0;
const RING_DX: f32 = 96.0;
/// Angular gap at 12 o'clock that holds the charging bolt.
const NOTCH_DEG: f32 = 46.0;
const LABEL_Y: f32 = RING_Y + RING_R + 21.0;
const BTN: (f32, f32, f32, f32) = (20.0, CARD_H - 20.0 - 48.0, CARD_W - 40.0, 48.0);

pub fn surface_size() -> (u32, u32) {
    ((CARD_W + 2.0 * PAD) as u32, (PAD + CARD_H + GAP) as u32)
}

/// Card rectangle in surface coordinates (the input region).
pub fn card_rect() -> (i32, i32, i32, i32) {
    (PAD as i32, PAD as i32, CARD_W as i32, CARD_H as i32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hit {
    Outside,
    Card,
    Close,
    Button,
}

/// What is under surface-local point (x, y)?
pub fn hit(sx: f32, sy: f32) -> Hit {
    let (x, y) = (sx - PAD, sy - PAD);
    if !(0.0..CARD_W).contains(&x) || !(0.0..CARD_H).contains(&y) {
        return Hit::Outside;
    }
    let (dx, dy) = (x - CLOSE_C.0, y - CLOSE_C.1);
    if dx * dx + dy * dy <= (CLOSE_R + 6.0).powi(2) {
        return Hit::Close;
    }
    let (bx, by, bw, bh) = BTN;
    if x >= bx - 2.0 && x <= bx + bw + 2.0 && y >= by - 2.0 && y <= by + bh + 2.0 {
        return Hit::Button;
    }
    Hit::Card
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Press {
    #[default]
    Idle,
    Hover,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ButtonKind {
    Connect,
    Connecting,
    Connected,
    Failed,
}

impl ButtonKind {
    pub fn clickable(self) -> bool {
        matches!(self, ButtonKind::Connect | ButtonKind::Failed)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RingView {
    pub id: RingId,
    pub batt: Batt,
    /// Animated fill, 0..=1 of the ring's span (the level once settled).
    pub fill: f32,
}

#[derive(Clone, Debug)]
pub struct View {
    pub title: String,
    pub product: Product,
    pub led: Led,
    pub rings: Vec<RingView>,
    pub button: ButtonKind,
    pub button_press: Press,
    pub close_press: Press,
    /// Spinner phase in turns; only used while connecting.
    pub spin: f32,
}

impl View {
    fn key(&self, scale: f32) -> u64 {
        let mut h = DefaultHasher::new();
        self.title.hash(&mut h);
        self.product.hash(&mut h);
        self.led.hash(&mut h);
        for r in &self.rings {
            r.id.hash(&mut h);
            r.batt.hash(&mut h);
        }
        self.button.hash(&mut h);
        self.button_press.hash(&mut h);
        self.close_press.hash(&mut h);
        scale.to_bits().hash(&mut h);
        h.finish()
    }
}

/// Where the card is in its enter/exit animation: it slides up out of
/// the gap below it while fading in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// Downward offset, logical px.
    pub dy: f32,
    pub alpha: f32,
}

impl Pose {
    pub const REST: Pose = Pose { dy: 0.0, alpha: 1.0 };

    /// `v` = 0 (hidden: down in the gap, transparent) … 1 (rest). Values
    /// above 1 (spring overshoot) nudge the card up a little.
    pub fn at(v: f32) -> Pose {
        Pose {
            dy: (1.0 - v) * GAP,
            alpha: (v * 1.6).clamp(0.0, 1.0),
        }
    }
}

// ─────────────────────────────── palette ────────────────────────────────

// The user's "Ember" desktop theme, shared with Waybar, mako and the
// LibrePods window.

/// #1a141b — card background and text on amber.
fn ember_dark() -> Color {
    rgb(0x1a, 0x14, 0x1b)
}
/// #f0e4d4
fn text_primary() -> Color {
    rgb(0xf0, 0xe4, 0xd4)
}
/// #bfa998
fn text_secondary() -> Color {
    rgb(0xbf, 0xa9, 0x98)
}
/// #7d6a66
fn text_tertiary() -> Color {
    rgb(0x7d, 0x6a, 0x66)
}
/// rgba(240,228,212,a) — neutral fills and ring tracks.
fn cream(a: f32) -> Color {
    rgba(0xf0, 0xe4, 0xd4, a)
}
/// #e89b5e — accent.
fn amber() -> Color {
    rgb(0xe8, 0x9b, 0x5e)
}
/// #a8c686
fn green() -> Color {
    rgb(0xa8, 0xc6, 0x86)
}
/// #efcf9f
fn gold() -> Color {
    rgb(0xef, 0xcf, 0x9f)
}
/// #e5534b
fn red() -> Color {
    rgb(0xe5, 0x53, 0x4b)
}
/// #8cb3d9
fn blue() -> Color {
    rgb(0x8c, 0xb3, 0xd9)
}

/// Same thresholds as the Waybar battery module.
fn ring_color(level: Option<u8>) -> Color {
    match level {
        Some(l) if l <= 10 => red(),
        Some(l) if l <= 20 => gold(),
        _ => green(),
    }
}

// ─────────────────────────────── renderer ───────────────────────────────

#[derive(Default)]
pub struct Renderer {
    /// Shadow coverage for the whole surface (black, alpha only).
    shadow: Option<(u64, Vec<u8>)>,
    art: Option<(u64, Pixmap)>,
    /// The card without its moving parts, card-sized.
    base: Option<(u64, Pixmap)>,
    /// Shadow + base composited at rest, surface-sized.
    layer: Option<(u64, Pixmap)>,
    /// Per-frame scratch for the sliding/fading frames.
    scratch: Option<Pixmap>,
}

fn card_px(scale: f32) -> (u32, u32) {
    ((CARD_W * scale).ceil() as u32, (CARD_H * scale).ceil() as u32)
}

fn origin_px(scale: f32) -> (i32, i32) {
    ((PAD * scale).round() as i32, (PAD * scale).round() as i32)
}

/// Pixel layout of the destination buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// R, G, B, A bytes (wl_shm ABGR8888 on little-endian; tiny-skia's).
    Rgba,
    /// B, G, R, A bytes (wl_shm ARGB8888 on little-endian).
    Bgra,
}

impl Renderer {
    /// Render one frame into `out` (premultiplied, `w`×`h` px, tightly
    /// packed) for a surface at `scale`.
    #[allow(clippy::too_many_arguments)]
    pub fn render(&mut self, out: &mut [u8], w: u32, h: u32, fmt: Format, scale: f32, view: &View, pose: Pose) {
        let (cw, ch) = card_px(scale);

        let art_key = {
            let mut hs = DefaultHasher::new();
            (view.product, view.led, scale.to_bits()).hash(&mut hs);
            hs.finish()
        };
        if self.art.as_ref().is_none_or(|(k, _)| *k != art_key) {
            self.art = Some((art_key, draw_art(scale, view)));
            self.base = None;
        }
        let key = view.key(scale);
        if self.base.as_ref().is_none_or(|(k, _)| *k != key) {
            let mut pm = Pixmap::new(cw, ch).expect("card pixmap");
            draw_base(&mut pm.as_mut(), scale, view, &self.art.as_ref().expect("art").1);
            self.base = Some((key, pm));
        }
        let sk = ((w as u64) << 40) ^ ((h as u64) << 20) ^ scale.to_bits() as u64;
        if self.shadow.as_ref().is_none_or(|(k, _)| *k != sk) {
            self.shadow = Some((sk, make_shadow(w, h, scale)));
        }

        // Static composite: shadow with the base card over it.
        let lk = key ^ sk.rotate_left(17);
        if self.layer.as_ref().is_none_or(|(k, _)| *k != lk) {
            let mut pm = Pixmap::new(w, h).expect("layer");
            let shadow = &self.shadow.as_ref().expect("shadow").1;
            for (px, &a) in pm.data_mut().chunks_exact_mut(4).zip(shadow) {
                px[3] = a;
            }
            let base = &self.base.as_ref().expect("base").1;
            let (ox, oy) = origin_px(scale);
            let src = Layer {
                data: base.data(),
                w: cw as usize,
                h: ch as usize,
            };
            blend_over::<false>(pm.data_mut(), w as usize, h as usize, &src, ox, oy, 256);
            self.layer = Some((lk, pm));
        }
        let layer = &self.layer.as_ref().expect("layer").1;

        // Moving parts go on top of the composite: source-over is
        // associative, so this equals drawing them on the card first.
        let origin = origin_px(scale);
        let dy = (pose.dy * scale).round() as i32;
        let alpha = (pose.alpha.clamp(0.0, 1.0) * 256.0).round() as u32;
        let swap = fmt == Format::Bgra;
        if dy == 0 && alpha >= 256 {
            // At rest: one copy, then draw straight into the buffer.
            copy_frame(out, layer.data(), w as usize, h as usize, 0, 256, swap);
            if let Some(mut pm) = PixmapMut::from_bytes(out, w, h) {
                draw_dynamic(&mut pm, scale, view, origin, swap);
            }
        } else {
            let scratch = self.scratch.get_or_insert_with(|| Pixmap::new(w, h).expect("scratch"));
            if scratch.width() != w || scratch.height() != h {
                *scratch = Pixmap::new(w, h).expect("scratch");
            }
            scratch.data_mut().copy_from_slice(layer.data());
            draw_dynamic(&mut scratch.as_mut(), scale, view, origin, false);
            copy_frame(out, scratch.data(), w as usize, h as usize, dy, alpha, swap);
        }
    }

    /// Drop cached layers (on hide: nothing to keep in memory while idle).
    pub fn clear(&mut self) {
        *self = Renderer::default();
    }
}

struct Layer<'a> {
    data: &'a [u8],
    w: usize,
    h: usize,
}

#[inline]
fn div255(x: u32) -> u32 {
    (x + 128 + ((x + 128) >> 8)) >> 8
}

/// `out` = `src` moved down by `dy` rows at opacity `alpha`/256
/// (premultiplied, so opacity is a plain scale), optionally writing BGRA.
fn copy_frame(out: &mut [u8], src: &[u8], w: usize, h: usize, dy: i32, alpha: u32, swap: bool) {
    let stride = w * 4;
    for y in 0..h {
        let row = &mut out[y * stride..(y + 1) * stride];
        let sy = y as i32 - dy;
        if sy < 0 || sy >= h as i32 {
            row.fill(0);
            continue;
        }
        let srow = &src[sy as usize * stride..(sy as usize + 1) * stride];
        match (alpha >= 256, swap) {
            (true, false) => row.copy_from_slice(srow),
            (true, true) => {
                for (d, s) in row.chunks_exact_mut(4).zip(srow.chunks_exact(4)) {
                    d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
                }
            }
            (false, false) => {
                for (d, &s) in row.iter_mut().zip(srow) {
                    *d = ((s as u32 * alpha) >> 8) as u8;
                }
            }
            (false, true) => {
                for (d, s) in row.chunks_exact_mut(4).zip(srow.chunks_exact(4)) {
                    let f = |v: u8| ((v as u32 * alpha) >> 8) as u8;
                    d.copy_from_slice(&[f(s[2]), f(s[1]), f(s[0]), f(s[3])]);
                }
            }
        }
    }
}

/// Premultiplied source-over of `src` at (ox, oy), scaled by
/// `alpha`/256, clipped to the destination. `SWAP` writes BGRA.
#[allow(clippy::too_many_arguments)]
fn blend_over<const SWAP: bool>(dst: &mut [u8], dw: usize, dh: usize, src: &Layer, ox: i32, oy: i32, alpha: u32) {
    let x0 = ox.max(0) as usize;
    let x1 = ((ox + src.w as i32).min(dw as i32)).max(0) as usize;
    if x1 <= x0 {
        return;
    }
    for sy in 0..src.h {
        let y = oy + sy as i32;
        if y < 0 || y >= dh as i32 {
            continue;
        }
        let drow = &mut dst[(y as usize * dw + x0) * 4..(y as usize * dw + x1) * 4];
        let sx0 = (x0 as i32 - ox) as usize;
        let srow = &src.data[(sy * src.w + sx0) * 4..(sy * src.w + sx0 + (x1 - x0)) * 4];
        for (d, s) in drow.chunks_exact_mut(4).zip(srow.chunks_exact(4)) {
            let (mut r, g, mut b, mut a) = (s[0] as u32, s[1] as u32, s[2] as u32, s[3] as u32);
            if a == 0 {
                continue;
            }
            let g = if alpha < 256 {
                r = (r * alpha) >> 8;
                b = (b * alpha) >> 8;
                a = (a * alpha) >> 8;
                (g * alpha) >> 8
            } else {
                g
            };
            if SWAP {
                std::mem::swap(&mut r, &mut b);
            }
            let inv = 255 - a;
            d[0] = (r + div255(d[0] as u32 * inv)) as u8;
            d[1] = (g + div255(d[1] as u32 * inv)) as u8;
            d[2] = (b + div255(d[2] as u32 * inv)) as u8;
            d[3] = (a + div255(d[3] as u32 * inv)) as u8;
        }
    }
}

fn card_path() -> Path {
    rrect(0.0, 0.0, CARD_W, CARD_H, RADIUS)
}

/// Soft two-layer drop shadow as surface-sized coverage, with the card's
/// own footprint cut out so the translucent card isn't darkened from
/// behind. Blurred at 1× (it is all low frequency) and scaled up.
fn make_shadow(w: u32, h: u32, scale: f32) -> Vec<u8> {
    let (lw, lh) = surface_size();
    let (lwu, lhu) = (lw as usize, lh as usize);
    let at1 = Transform::from_translate(PAD, PAD);

    let layer = |dy: f32, sigma: f32, alpha: f32| -> Vec<f32> {
        let mut m = Mask::new(lw, lh).expect("mask");
        m.fill_path(
            &card_path(),
            tiny_skia::FillRule::Winding,
            true,
            Transform::from_translate(0.0, dy).pre_concat(at1),
        );
        let mut buf: Vec<f32> = m.data().iter().map(|&a| a as f32 / 255.0).collect();
        blur_alpha(&mut buf, lwu, lhu, sigma);
        buf.iter_mut().for_each(|a| *a *= alpha);
        buf
    };
    // Kept below Hyprland's `ignore_alpha 0.3` for this namespace, so the
    // compositor blurs behind the card but never behind its shadow.
    let wide = layer(8.0, 14.0, 0.34);
    let tight = layer(1.0, 1.2, 0.12);

    const MAX_SHADOW: f32 = 0.27;
    let cov: Vec<f32> = wide
        .iter()
        .zip(&tight)
        .map(|(a, b)| (1.0 - (1.0 - a) * (1.0 - b)).min(MAX_SHADOW))
        .collect();

    let (ox, oy) = origin_px(scale);
    let mut cut = Mask::new(w, h).expect("mask");
    cut.fill_path(
        &card_path(),
        tiny_skia::FillRule::Winding,
        true,
        Transform::from_translate(ox as f32, oy as f32).pre_scale(scale, scale),
    );

    // Bilinear upscale of the 1× coverage to buffer pixels: column taps
    // precomputed, rows blended once, then one lerp per pixel.
    let (fx, fy) = (lw as f32 / w as f32, lh as f32 / h as f32);
    let taps: Vec<(usize, usize, f32)> = (0..w as usize)
        .map(|x| {
            let sx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (lwu - 1) as f32);
            let x0 = sx.floor() as usize;
            (x0, (x0 + 1).min(lwu - 1), sx - x0 as f32)
        })
        .collect();
    let mut row = vec![0.0f32; lwu];
    let mut out = vec![0u8; w as usize * h as usize];
    let cutd = cut.data();
    for y in 0..h as usize {
        let sy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (lhu - 1) as f32);
        let y0 = sy.floor() as usize;
        let (y1, ty) = ((y0 + 1).min(lhu - 1), sy - y0 as f32);
        let (r0, r1) = (&cov[y0 * lwu..(y0 + 1) * lwu], &cov[y1 * lwu..(y1 + 1) * lwu]);
        for ((d, a), b) in row.iter_mut().zip(r0).zip(r1) {
            *d = a + (b - a) * ty;
        }
        let orow = &mut out[y * w as usize..(y + 1) * w as usize];
        let crow = &cutd[y * w as usize..(y + 1) * w as usize];
        for ((o, &(x0, x1, tx)), &c) in orow.iter_mut().zip(&taps).zip(crow) {
            let a = row[x0] + (row[x1] - row[x0]) * tx;
            *o = (a * (255 - c) as f32).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// The illustration, rendered once per product/LED/scale.
fn draw_art(s: f32, v: &View) -> Pixmap {
    let (_, _, aw, ah) = ART;
    let mut pm = Pixmap::new((aw * s).ceil() as u32 + 2, (ah * s).ceil() as u32 + 2).expect("art");
    art::draw(&mut pm.as_mut(), s, v.product, v.led, (0.0, 0.0, aw, ah));
    pm
}

/// Everything that does not move between animation frames.
fn draw_base(pm: &mut PixmapMut, s: f32, v: &View, art_pm: &Pixmap) {
    let ts = Transform::from_scale(s, s);

    // Body: Ember dark at 92 % (Hyprland blurs what's behind), with the
    // theme's subtle 1 px amber border drawn inside the edge.
    let body = card_path();
    fill(pm, &body, &solid(with_alpha(ember_dark(), 0.92)), ts);
    let clip = mask(pm, &body, ts);
    stroke_in(pm, &body, 2.0, &solid(rgba(232, 155, 94, 0.35)), ts, &clip);

    // Title.
    let title_style = Style::new(19.0, Weight::Semibold, text_primary()).tracking(-0.2);
    let (title, st) = text::fit(&v.title, CARD_W - 2.0 * 62.0, &title_style, 14.5);
    text::draw(pm, s, &title, CARD_W * 0.5, TITLE_Y, Align::Center, &st);

    // Close button.
    let bg = match v.close_press {
        Press::Idle => cream(0.08),
        Press::Hover => cream(0.14),
        Press::Down => cream(0.20),
    };
    fill(pm, &circle(CLOSE_C.0, CLOSE_C.1, CLOSE_R), &solid(bg), ts);
    let xc = match v.close_press {
        Press::Idle => text_secondary(),
        _ => text_primary(),
    };
    let d = 4.4;
    let mut pb = PathBuilder::new();
    pb.move_to(CLOSE_C.0 - d, CLOSE_C.1 - d);
    pb.line_to(CLOSE_C.0 + d, CLOSE_C.1 + d);
    pb.move_to(CLOSE_C.0 + d, CLOSE_C.1 - d);
    pb.line_to(CLOSE_C.0 - d, CLOSE_C.1 + d);
    stroke(pm, &pb.finish().expect("x"), 2.1, &solid(xc), ts);

    // Illustration (pixel-aligned blit of the cached layer).
    pm.draw_pixmap(
        (ART.0 * s).round() as i32,
        (ART.1 * s).round() as i32,
        art_pm.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );

    // Ring row, static parts.
    for (i, r) in v.rings.iter().enumerate() {
        let cx = ring_x(i, v.rings.len());
        draw_ring_static(pm, s, cx, RING_Y, r);
    }

    draw_button_static(pm, s, v);
}

fn ring_x(i: usize, n: usize) -> f32 {
    CARD_W * 0.5 + (i as f32 - (n as f32 - 1.0) * 0.5) * RING_DX
}

/// Arc as cubic Béziers; angles in degrees, 0 = 3 o'clock, clockwise.
fn arc(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Option<Path> {
    if (a1 - a0).abs() < 0.01 {
        return None;
    }
    let n = ((a1 - a0).abs() / 45.0).ceil().max(1.0) as usize;
    let step = (a1 - a0) / n as f32;
    let k = 4.0 / 3.0 * (step.to_radians() / 4.0).tan();
    let pt = |a: f32| {
        let (sn, cs) = a.to_radians().sin_cos();
        (cx + r * cs, cy + r * sn)
    };
    let mut pb = PathBuilder::new();
    let (x0, y0) = pt(a0);
    pb.move_to(x0, y0);
    for i in 0..n {
        let s0 = a0 + step * i as f32;
        let s1 = s0 + step;
        let (p0x, p0y) = pt(s0);
        let (p3x, p3y) = pt(s1);
        let (t0s, t0c) = s0.to_radians().sin_cos();
        let (t1s, t1c) = s1.to_radians().sin_cos();
        pb.cubic_to(
            p0x - k * r * t0s,
            p0y + k * r * t0c,
            p3x + k * r * t1s,
            p3y - k * r * t1c,
            p3x,
            p3y,
        );
    }
    pb.finish()
}

/// Start and sweep of a ring's usable circle (leaving the notch free
/// while charging).
fn ring_span(charging: bool) -> (f32, f32) {
    if charging {
        (-90.0 + NOTCH_DEG * 0.5, 360.0 - NOTCH_DEG)
    } else {
        (-90.0, 360.0)
    }
}

fn bolt(cx: f32, cy: f32, h: f32) -> Path {
    let k = h / 12.0;
    let pts = [(0.9, -6.0), (-3.9, 1.1), (-0.5, 1.1), (-1.1, 6.0), (3.9, -1.4), (0.5, -1.4)];
    let mut pb = PathBuilder::new();
    for (i, (x, y)) in pts.iter().enumerate() {
        let (x, y) = (cx + x * k, cy + y * k);
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    pb.close();
    pb.finish().expect("bolt")
}

fn draw_ring_static(pm: &mut PixmapMut, s: f32, cx: f32, cy: f32, r: &RingView) {
    let ts = Transform::from_scale(s, s);
    let charging = r.batt.charging;
    let (a0, sweep) = ring_span(charging);
    if let Some(track) = arc(cx, cy, RING_R, a0, a0 + sweep) {
        let st = tiny_skia::Stroke {
            width: RING_W,
            line_cap: if charging {
                tiny_skia::LineCap::Round
            } else {
                tiny_skia::LineCap::Butt
            },
            ..Default::default()
        };
        pm.stroke_path(&track, &solid(cream(0.12)), &st, ts, None);
    }
    if charging {
        // Nerd Font bolt (U+F140B), as in the bar; vector bolt otherwise.
        if !text::draw_icon(pm, s, '\u{F140B}', cx, cy - RING_R, 13.0, green()) {
            fill(pm, &bolt(cx, cy - RING_R, 12.5), &solid(green()), ts);
        }
    }

    match r.batt.level {
        Some(l) => {
            let num = Style::new(15.0, Weight::Semibold, text_primary()).tracking(-0.3);
            let pct = Style::new(10.5, Weight::Semibold, text_secondary());
            let n = text::shape(&l.to_string(), &num);
            let p = text::shape("%", &pct);
            let w = n.width + 0.8 + p.width;
            let base = cy + text::cap_height(&num) * 0.5;
            let x0 = cx - w * 0.5;
            text::draw_shaped(pm, s, &n, x0, base, num.color);
            text::draw_shaped(pm, s, &p, x0 + n.width + 0.8, base, pct.color);
        }
        None => {
            let st = Style::new(15.0, Weight::Semibold, text_tertiary());
            let base = cy + text::cap_height(&st) * 0.5;
            // A drawn dash reads better than a glyph at this size.
            let w = 9.0;
            fill(pm, &rrect(cx - w * 0.5, base - text::cap_height(&st) * 0.5 - 1.0, w, 2.0, 1.0), &solid(st.color), ts);
        }
    }

    let label = Style::new(12.5, Weight::Medium, text_secondary());
    text::draw(pm, s, r.id.label(), cx, LABEL_Y, Align::Center, &label);
}

/// Label, icon slot and colours for the button.
struct ButtonLook {
    fill: Color,
    text: &'static str,
    style: Style,
    /// Width reserved left of the text for the spinner/check.
    icon: f32,
}

fn button_look(v: &View) -> ButtonLook {
    let semibold = |c| Style::new(16.5, Weight::Semibold, c).tracking(-0.2);
    match v.button {
        ButtonKind::Connect => ButtonLook {
            fill: match v.button_press {
                Press::Idle => amber(),
                Press::Hover => rgb(0xef, 0xab, 0x75),
                Press::Down => rgb(0xd6, 0x88, 0x4c),
            },
            text: "Connect",
            style: semibold(ember_dark()),
            icon: 0.0,
        },
        ButtonKind::Connecting => ButtonLook {
            fill: cream(0.08),
            text: "Connecting…",
            style: semibold(text_secondary()),
            icon: 26.0,
        },
        ButtonKind::Connected => ButtonLook {
            fill: cream(0.06),
            text: "Connected",
            style: semibold(blue()),
            icon: 24.0,
        },
        ButtonKind::Failed => ButtonLook {
            fill: match v.button_press {
                Press::Idle => rgba(0xe5, 0x53, 0x4b, 0.14),
                Press::Hover => rgba(0xe5, 0x53, 0x4b, 0.22),
                Press::Down => rgba(0xe5, 0x53, 0x4b, 0.30),
            },
            text: "Couldn’t Connect · Try Again",
            style: semibold(red()),
            icon: 0.0,
        },
    }
}

/// Left edge of the icon+label group, and the label's width.
fn button_layout(look: &ButtonLook) -> (f32, f32) {
    let tw = text::measure(look.text, &look.style);
    let (bx, _, bw, _) = BTN;
    (bx + (bw - tw - look.icon) * 0.5, tw)
}

fn draw_button_static(pm: &mut PixmapMut, s: f32, v: &View) {
    let ts = Transform::from_scale(s, s);
    let (bx, by, bw, bh) = BTN;
    let look = button_look(v);
    let pill = rrect(bx, by, bw, bh, bh * 0.5);
    fill(pm, &pill, &solid(look.fill), ts);
    let (x0, _) = button_layout(&look);
    let base = by + bh * 0.5 + text::cap_height(&look.style) * 0.5;
    text::draw(pm, s, look.text, x0 + look.icon, base, Align::Left, &look.style);

    if v.button == ButtonKind::Connected {
        let (cx, cy) = (x0 + 8.0, by + bh * 0.5);
        let mut pb = PathBuilder::new();
        pb.move_to(cx - 6.0, cy + 0.2);
        pb.line_to(cx - 1.9, cy + 4.2);
        pb.line_to(cx + 6.0, cy - 4.6);
        let st = tiny_skia::Stroke {
            width: 2.4,
            line_cap: tiny_skia::LineCap::Round,
            line_join: tiny_skia::LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&pb.finish().expect("check"), &solid(blue()), &st, ts, None);
    }
}

/// Per-frame parts: ring fill arcs and the spinner, drawn on the
/// surface-sized frame with the card at `origin` (px). `swap` draws for a
/// BGRA buffer by swapping the paint's red and blue.
fn draw_dynamic(pm: &mut PixmapMut, s: f32, v: &View, origin: (i32, i32), swap: bool) {
    let ts = Transform::from_translate(origin.0 as f32, origin.1 as f32).pre_scale(s, s);
    let fix = |c: Color| {
        if swap {
            Color::from_rgba(c.blue(), c.green(), c.red(), c.alpha()).unwrap_or(c)
        } else {
            c
        }
    };
    for (i, r) in v.rings.iter().enumerate() {
        let Some(level) = r.batt.level else { continue };
        let frac = r.fill.clamp(0.0, 1.0);
        if frac <= 0.001 {
            continue;
        }
        let cx = ring_x(i, v.rings.len());
        let (a0, sweep) = ring_span(r.batt.charging);
        if let Some(p) = arc(cx, RING_Y, RING_R, a0, a0 + sweep * frac) {
            let st = tiny_skia::Stroke {
                width: RING_W,
                line_cap: tiny_skia::LineCap::Round,
                ..Default::default()
            };
            pm.stroke_path(&p, &solid(fix(ring_color(Some(level)))), &st, ts, None);
        }
    }

    if v.button == ButtonKind::Connecting {
        let look = button_look(v);
        let (x0, _) = button_layout(&look);
        let (_, by, _, bh) = BTN;
        spinner(pm, ts, x0 + 8.0, by + bh * 0.5, v.spin, fix(text_secondary()));
    }
}

/// UIActivityIndicator-style spinner: eight spokes, the lit one stepping
/// round in discrete ticks (which also keeps the redraw rate low).
fn spinner(pm: &mut PixmapMut, ts: Transform, cx: f32, cy: f32, phase: f32, color: Color) {
    const N: usize = 8;
    let head = ((phase.rem_euclid(1.0)) * N as f32).floor() as usize % N;
    for i in 0..N {
        let a = (i as f32 * 360.0 / N as f32 - 90.0).to_radians();
        let (sn, cs) = a.sin_cos();
        let age = (head + N - i) % N;
        let alpha = (1.0 - age as f32 * 0.12).max(0.22);
        let p = line(cx + cs * 3.6, cy + sn * 3.6, cx + cs * 7.6, cy + sn * 7.6);
        stroke(pm, &p, 2.2, &solid(with_alpha(color, alpha)), ts);
    }
}

/// Spinner frames per second (one spoke per tick, iOS pace).
pub const SPINNER_HZ: f32 = 12.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> View {
        View {
            title: "AirPods Pro".into(),
            product: Product::Pro,
            led: Led::Charging,
            rings: vec![
                RingView {
                    id: RingId::Left,
                    batt: Batt {
                        level: Some(92),
                        charging: false,
                    },
                    fill: 1.0,
                },
                RingView {
                    id: RingId::Right,
                    batt: Batt {
                        level: Some(8),
                        charging: false,
                    },
                    fill: 1.0,
                },
                RingView {
                    id: RingId::Case,
                    batt: Batt {
                        level: Some(38),
                        charging: true,
                    },
                    fill: 1.0,
                },
            ],
            button: ButtonKind::Connect,
            button_press: Press::Idle,
            close_press: Press::Idle,
            spin: 0.0,
        }
    }

    #[test]
    fn hit_regions() {
        let (bx, by, bw, bh) = BTN;
        assert_eq!(hit(PAD + bx + bw * 0.5, PAD + by + bh * 0.5), Hit::Button);
        assert_eq!(hit(PAD + CLOSE_C.0, PAD + CLOSE_C.1), Hit::Close);
        assert_eq!(hit(PAD + CARD_W * 0.5, PAD + 120.0), Hit::Card);
        assert_eq!(hit(2.0, 2.0), Hit::Outside);
    }

    #[test]
    fn renders_at_fractional_scale() {
        let mut r = Renderer::default();
        let (w, h) = surface_size();
        for s in [1.0f32, 1.25, 2.0] {
            let (pw, ph) = ((w as f32 * s).round() as u32, (h as f32 * s).round() as u32);
            let mut out = vec![0u8; (pw * ph * 4) as usize];
            r.render(&mut out, pw, ph, Format::Rgba, s, &view(), Pose::REST);
            let (ox, oy) = origin_px(s);
            // Card centre is opaque-ish; the far corner is transparent.
            let c = ((oy as u32 + 5) * pw + ox as u32 + (CARD_W * s * 0.5) as u32) as usize * 4;
            assert!(out[c + 3] > 200);
            assert_eq!(out[3], 0);
            // Halfway through the entrance everything is fainter.
            let mut mid = vec![0u8; out.len()];
            r.render(&mut mid, pw, ph, Format::Rgba, s, &view(), Pose::at(0.3));
            assert!(mid.iter().skip(3).step_by(4).map(|&a| a as u64).sum::<u64>() < out.iter().skip(3).step_by(4).map(|&a| a as u64).sum::<u64>());
        }
    }

    #[test]
    fn bgra_frame_is_rgba_frame_swapped() {
        let mut r = Renderer::default();
        let (w, h) = surface_size();
        let mut v = view();
        v.button = ButtonKind::Connecting;
        let n = (w * h * 4) as usize;
        let (mut a, mut b) = (vec![0u8; n], vec![0u8; n]);
        for pose in [Pose::REST, Pose::at(0.5)] {
            r.render(&mut a, w, h, Format::Rgba, 1.0, &v, pose);
            r.render(&mut b, w, h, Format::Bgra, 1.0, &v, pose);
            let same = a
                .chunks_exact(4)
                .zip(b.chunks_exact(4))
                .all(|(p, q)| p[0].abs_diff(q[2]) <= 1 && p[1] == q[1] && p[2].abs_diff(q[0]) <= 1 && p[3] == q[3]);
            assert!(same, "BGRA output must be the RGBA output with R/B swapped ({pose:?})");
        }
    }

    #[test]
    fn blend_matches_source_over() {
        let src = [100u8, 50, 0, 128];
        let layer = Layer { data: &src, w: 1, h: 1 };
        let mut dst = [0u8, 0, 200, 255];
        blend_over::<false>(&mut dst, 1, 1, &layer, 0, 0, 256);
        assert_eq!(dst, [100, 50, 100, 255]);
        let mut dst = [0u8, 0, 200, 255];
        blend_over::<true>(&mut dst, 1, 1, &layer, 0, 0, 256);
        assert_eq!(dst, [0, 50, 200, 255], "swapped: src B=0 lands in byte 0");
    }

    #[test]
    fn low_ring_is_red_and_charging_ring_has_notch() {
        assert_eq!(ring_color(Some(8)), red());
        assert_eq!(ring_color(Some(10)), red());
        assert_eq!(ring_color(Some(20)), gold());
        assert_eq!(ring_color(Some(21)), green());
        assert_eq!(ring_color(None), green());
        assert_eq!(ring_span(true).1, 360.0 - NOTCH_DEG);
    }
}
