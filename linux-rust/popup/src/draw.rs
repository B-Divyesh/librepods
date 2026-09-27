//! Small tiny-skia conveniences shared by the card and the illustrations.
//!
//! The fill/gradient helpers follow podctl's `render.rs`
//! (https://github.com/Rockykln/podctl, MIT OR Apache-2.0,
//! © 2026 pods contributors); the continuous-corner rectangle and the
//! blur are new.

use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LinearGradient, Mask, Paint, Path, PathBuilder, PixmapMut, Point,
    RadialGradient, Rect, Shader, SpreadMode, Stroke, Transform,
};

pub fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::from_rgba8(r, g, b, (a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

pub fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgba8(r, g, b, 255)
}

pub fn grey(v: u8) -> Color {
    rgb(v, v, v)
}

pub fn white(a: f32) -> Color {
    rgba(255, 255, 255, a)
}

pub fn black(a: f32) -> Color {
    rgba(0, 0, 0, a)
}

pub fn with_alpha(c: Color, a: f32) -> Color {
    let mut c = c;
    c.set_alpha(a.clamp(0.0, 1.0));
    c
}

fn stops(s: &[(f32, Color)]) -> Vec<GradientStop> {
    s.iter().map(|&(p, c)| GradientStop::new(p, c)).collect()
}

pub fn lin(p0: (f32, f32), p1: (f32, f32), s: &[(f32, Color)]) -> Option<Shader<'static>> {
    LinearGradient::new(
        Point::from_xy(p0.0, p0.1),
        Point::from_xy(p1.0, p1.1),
        stops(s),
        SpreadMode::Pad,
        Transform::identity(),
    )
}

/// Radial gradient centred on `c`. `squash` < 1 flattens it vertically
/// into an ellipse (useful for contact shadows and sheens).
pub fn rad(c: (f32, f32), r: f32, squash: f32, s: &[(f32, Color)]) -> Option<Shader<'static>> {
    RadialGradient::new(
        Point::from_xy(0.0, 0.0),
        0.0,
        Point::from_xy(0.0, 0.0),
        r.max(0.01),
        stops(s),
        SpreadMode::Pad,
        Transform::from_row(1.0, 0.0, 0.0, squash.max(0.01), c.0, c.1),
    )
}

pub fn paint(shader: Option<Shader<'static>>, fallback: Color) -> Paint<'static> {
    let mut p = Paint {
        anti_alias: true,
        ..Default::default()
    };
    match shader {
        Some(s) => p.shader = s,
        None => p.set_color(fallback),
    }
    p
}

pub fn solid(c: Color) -> Paint<'static> {
    paint(None, c)
}

pub fn fill(pm: &mut PixmapMut, path: &Path, p: &Paint, ts: Transform) {
    pm.fill_path(path, p, FillRule::Winding, ts, None);
}

pub fn fill_in(pm: &mut PixmapMut, path: &Path, p: &Paint, ts: Transform, clip: &Mask) {
    pm.fill_path(path, p, FillRule::Winding, ts, Some(clip));
}

pub fn stroke(pm: &mut PixmapMut, path: &Path, width: f32, p: &Paint, ts: Transform) {
    let s = Stroke {
        width,
        line_cap: LineCap::Round,
        ..Default::default()
    };
    pm.stroke_path(path, p, &s, ts, None);
}

pub fn stroke_in(pm: &mut PixmapMut, path: &Path, width: f32, p: &Paint, ts: Transform, clip: &Mask) {
    let s = Stroke {
        width,
        line_cap: LineCap::Round,
        ..Default::default()
    };
    pm.stroke_path(path, p, &s, ts, Some(clip));
}

/// Anti-aliased clip mask of `path` for a pixmap of `pm`'s size.
pub fn mask(pm: &PixmapMut, path: &Path, ts: Transform) -> Mask {
    let mut m = Mask::new(pm.width(), pm.height()).expect("mask alloc");
    m.fill_path(path, FillRule::Winding, true, ts);
    m
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Path {
    PathBuilder::from_rect(Rect::from_xywh(x, y, w.max(0.01), h.max(0.01)).expect("rect"))
}

pub fn circle(cx: f32, cy: f32, r: f32) -> Path {
    PathBuilder::from_circle(cx, cy, r.max(0.01)).expect("circle")
}

/// Ellipse centred on (cx, cy), rotated by `deg` degrees clockwise.
pub fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32, deg: f32) -> Path {
    let p = PathBuilder::from_oval(Rect::from_xywh(cx - rx, cy - ry, 2.0 * rx, 2.0 * ry).expect("oval"))
        .expect("oval path");
    if deg == 0.0 {
        p
    } else {
        p.transform(Transform::from_rotate_at(deg, cx, cy)).expect("rotate")
    }
}

pub fn line(x0: f32, y0: f32, x1: f32, y1: f32) -> Path {
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    pb.line_to(x1, y1);
    pb.finish().expect("line")
}

/// Circular-cornered rectangle.
pub fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    const K: f32 = 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + r * K, y, x + w, y + r - r * K, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + r * K, x + w - r + r * K, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - r * K, y + h, x, y + h - r + r * K, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - r * K, x + r - r * K, y, x + r, y);
    pb.close();
    pb.finish().expect("rrect")
}

/// Rectangle with Apple's "continuous" corners (the iOS squircle): the
/// curvature ramps up instead of jumping from straight edge to arc.
/// Coefficients as reverse-engineered by PaintCode for UIBezierPath.
pub fn squircle(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    const E: f32 = 1.528_664_8;
    let r = r.min(w * 0.5 / E).min(h * 0.5 / E).max(0.0);
    // (u, v): u back along the incoming edge from the corner, v forward
    // along the outgoing edge.
    const SEGS: [[(f32, f32); 3]; 3] = [
        [(1.088_493_2, 0.0), (0.868_406_9, 0.0), (0.631_494, 0.074_911)],
        [(0.372_823_9, 0.169_059), (0.169_059, 0.372_823_9), (0.074_911, 0.631_494)],
        [(0.0, 0.868_406_9), (0.0, 1.088_493_2), (0.0, E)],
    ];
    // Corner point, incoming direction, outgoing direction.
    let corners = [
        ((x + w, y), (1.0, 0.0), (0.0, 1.0)),
        ((x + w, y + h), (0.0, 1.0), (-1.0, 0.0)),
        ((x, y + h), (-1.0, 0.0), (0.0, -1.0)),
        ((x, y), (0.0, -1.0), (1.0, 0.0)),
    ];
    let mut pb = PathBuilder::new();
    pb.move_to(x + E * r, y);
    for ((cx, cy), (ax, ay), (bx, by)) in corners {
        let at = |(u, v): (f32, f32)| (cx - ax * u * r + bx * v * r, cy - ay * u * r + by * v * r);
        let (sx, sy) = at((E, 0.0));
        pb.line_to(sx, sy);
        for seg in SEGS {
            let (a, b, c) = (at(seg[0]), at(seg[1]), at(seg[2]));
            pb.cubic_to(a.0, a.1, b.0, b.1, c.0, c.1);
        }
    }
    pb.close();
    pb.finish().expect("squircle")
}

/// Three-pass box blur of an alpha buffer — a close Gaussian
/// approximation, used once per scale for the card's drop shadow.
pub fn blur_alpha(buf: &mut [f32], w: usize, h: usize, sigma: f32) {
    if sigma < 0.5 || w == 0 || h == 0 {
        return;
    }
    // Box width for three passes (W3C filter effects formula).
    let d = ((sigma * 3.0 * (2.0 * std::f32::consts::PI).sqrt() / 4.0) + 0.5).floor() as usize;
    let r = (d / 2).max(1);
    let n = w.max(h);
    let mut line = vec![0.0f32; n];
    let mut prefix = vec![0.0f32; n + 1];
    for _ in 0..3 {
        for y in 0..h {
            let row = &mut buf[y * w..(y + 1) * w];
            box_1d(row, r, &mut prefix);
        }
        for x in 0..w {
            for y in 0..h {
                line[y] = buf[y * w + x];
            }
            box_1d(&mut line[..h], r, &mut prefix);
            for y in 0..h {
                buf[y * w + x] = line[y];
            }
        }
    }
}

/// Box filter of radius `r` over `v` (zero outside), via prefix sums.
fn box_1d(v: &mut [f32], r: usize, prefix: &mut [f32]) {
    let n = v.len();
    prefix[0] = 0.0;
    for i in 0..n {
        prefix[i + 1] = prefix[i] + v[i];
    }
    let norm = 1.0 / (2 * r + 1) as f32;
    for (i, out) in v.iter_mut().enumerate() {
        let hi = (i + r + 1).min(n);
        let lo = i.saturating_sub(r);
        *out = (prefix[hi] - prefix[lo]) * norm;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squircle_stays_in_bounds() {
        let p = squircle(10.0, 20.0, 200.0, 100.0, 30.0);
        let b = p.bounds();
        assert!((b.left() - 10.0).abs() < 0.01 && (b.right() - 210.0).abs() < 0.01);
        assert!((b.top() - 20.0).abs() < 0.01 && (b.bottom() - 120.0).abs() < 0.01);
    }

    #[test]
    fn blur_preserves_mass() {
        let (w, h) = (64, 64);
        let mut b = vec![0.0f32; w * h];
        for y in 24..40 {
            for x in 24..40 {
                b[y * w + x] = 1.0;
            }
        }
        let before: f32 = b.iter().sum();
        blur_alpha(&mut b, w, h, 3.0);
        let after: f32 = b.iter().sum();
        assert!((before - after).abs() / before < 0.02);
        assert!(b[32 * w + 32] < 1.0 && b[32 * w + 20] > 0.0);
    }
}
