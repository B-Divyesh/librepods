//! Vector product illustrations: AirPods Pro, open-fit AirPods (3/4),
//! classic AirPods (1/2) with their cases, and AirPods Max.
//!
//! Own geometry, no Apple artwork. The earbud-as-layered-shapes approach
//! (body, stem, tip, sensor, mic cap, each with its own shading, plus
//! soft contact shadows) follows podctl's `assets.rs`/`render.rs`
//! (https://github.com/Rockykln/podctl, MIT OR Apache-2.0,
//! © 2026 pods contributors).
//!
//! Every part is authored in its own unit space and placed with a
//! transform; gradients live in the same space, so they follow along.

use tiny_skia::{PathBuilder, PixmapMut, Transform};

use crate::draw::*;
use crate::protocol::Product;

/// Case status light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Led {
    Off,
    Charging,
    Full,
}

/// White plastic palette.
mod pal {
    use crate::draw::{grey, rgb};
    use tiny_skia::Color;
    pub fn hi() -> Color {
        grey(255)
    }
    pub fn body() -> Color {
        rgb(243, 243, 245)
    }
    pub fn mid() -> Color {
        rgb(226, 227, 231)
    }
    pub fn shade() -> Color {
        rgb(197, 198, 205)
    }
    pub fn deep() -> Color {
        rgb(160, 161, 169)
    }
    pub fn vent() -> Color {
        rgb(62, 64, 70)
    }
    pub fn mesh() -> Color {
        rgb(38, 40, 44)
    }
}

/// Draw `product` fitted and centred inside `dst` (logical x, y, w, h).
pub fn draw(pm: &mut PixmapMut, scale: f32, product: Product, led: Led, dst: (f32, f32, f32, f32)) {
    let (cw, ch) = comp_size(product);
    let k = (dst.2 / cw).min(dst.3 / ch);
    let ox = dst.0 + (dst.2 - cw * k) * 0.5;
    let oy = dst.1 + (dst.3 - ch * k) * 0.5;
    let base = Transform::from_scale(scale, scale).pre_translate(ox, oy).pre_scale(k, k);
    match product {
        Product::Pro => pro(pm, base, led),
        Product::Open => open(pm, base, led),
        Product::Classic => classic(pm, base, led),
        Product::Max => max(pm, base),
    }
}

fn comp_size(p: Product) -> (f32, f32) {
    match p {
        Product::Pro => (292.0, 118.0),
        Product::Open => (292.0, 118.0),
        Product::Classic => (292.0, 118.0),
        Product::Max => (200.0, 118.0),
    }
}

fn place(base: Transform, x: f32, y: f32, s: f32) -> Transform {
    base.pre_translate(x, y).pre_scale(s, s)
}

/// Mirror around the object's local x = 0: the object then extends to the
/// left of `x`.
fn place_mirrored(base: Transform, x: f32, y: f32, s: f32) -> Transform {
    base.pre_translate(x, y).pre_scale(-s, s)
}

fn contact_shadow(pm: &mut PixmapMut, ts: Transform, cx: f32, cy: f32, rx: f32, squash: f32, a: f32) {
    let sh = rad((cx, cy), rx, squash, &[(0.0, black(a)), (0.55, black(a * 0.45)), (1.0, black(0.0))]);
    fill(pm, &rect(cx - rx, cy - rx * squash, 2.0 * rx, 2.0 * rx * squash), &paint(sh, black(0.0)), ts);
}

// ───────────────────────────── AirPods Pro ──────────────────────────────

fn pro(pm: &mut PixmapMut, base: Transform, led: Led) {
    let s = 0.86;
    let y = 12.0;
    // Left bud, tip pointing right (towards its twin).
    let l = place(base, -4.0, y, s);
    // Right bud mirrored so the tips face each other with a small gap.
    let r = place_mirrored(base, 140.0, y, s);
    contact_shadow(pm, l, 33.0, 107.0, 17.0, 0.2, 0.38);
    contact_shadow(pm, r, 33.0, 107.0, 17.0, 0.2, 0.38);
    pro_bud(pm, l);
    pro_bud(pm, r);

    let c = place(base, 152.0, 9.0, 1.0);
    contact_shadow(pm, c, 68.0, 102.5, 66.0, 0.1, 0.5);
    case_front(pm, c, 136.0, 101.0, 36.0, 30.0, led);
}

/// One AirPods Pro earbud in a ~84×108 unit box, tip pointing +x.
fn pro_bud(pm: &mut PixmapMut, ts: Transform) {
    // Stem, tucked under the head.
    let stem = tapered_stem(25.0, 43.0, 25.5, 40.8, 40.0, 106.0, 7.0);
    let stem_paint = paint(
        lin(
            (24.0, 0.0),
            (43.0, 0.0),
            &[
                (0.0, rgb(206, 207, 213)),
                (0.28, pal::hi()),
                (0.55, pal::body()),
                (1.0, rgb(184, 185, 192)),
            ],
        ),
        pal::body(),
    );
    fill(pm, &stem, &stem_paint, ts);
    // Microphone end: a darker cap on the last few units.
    let clip_cap = mask(pm, &rect(0.0, 99.5, 90.0, 20.0), ts);
    let cap = paint(
        lin(
            (24.0, 0.0),
            (43.0, 0.0),
            &[(0.0, rgb(150, 152, 159)), (0.35, rgb(190, 192, 198)), (1.0, rgb(132, 134, 141))],
        ),
        pal::deep(),
    );
    fill_in(pm, &stem, &cap, ts, &clip_cap);
    // Force sensor: a faint recessed flat on the stem.
    fill(pm, &rrect(31.2, 65.0, 3.4, 24.0, 1.7), &solid(black(0.075)), ts);
    stroke(pm, &line(35.4, 66.5, 35.2, 87.5), 0.8, &solid(white(0.75)), ts);
    // Occlusion where the head overhangs the stem.
    let occ = rad(
        (33.5, 57.0),
        15.0,
        0.45,
        &[(0.0, black(0.22)), (1.0, black(0.0))],
    );
    fill(pm, &rect(18.0, 50.0, 32.0, 14.0), &paint(occ, black(0.0)), ts);

    // Head.
    let head = ellipse(36.0, 33.5, 29.0, 27.0, -8.0);
    let head_paint = paint(
        rad(
            (41.0, 21.0),
            40.0,
            1.0,
            &[
                (0.0, pal::hi()),
                (0.45, pal::body()),
                (0.78, pal::mid()),
                (1.0, rgb(186, 187, 194)),
            ],
        ),
        pal::body(),
    );
    fill(pm, &head, &head_paint, ts);
    // Top vent.
    fill(pm, &ellipse(21.0, 15.5, 6.2, 1.7, -40.0), &solid(with_alpha(pal::vent(), 0.9)), ts);
    // Front vent (the little mesh at the back of the Pro head).
    fill(pm, &ellipse(12.5, 36.0, 1.5, 4.6, 8.0), &solid(with_alpha(pal::vent(), 0.75)), ts);

    // Silicone ear tip: matte, a touch greyer than the gloss body.
    let tip = ellipse(67.0, 27.0, 13.0, 18.5, -22.0);
    let tip_paint = paint(
        rad(
            (63.0, 17.0),
            24.0,
            1.0,
            &[(0.0, rgb(238, 239, 242)), (0.6, rgb(220, 221, 226)), (1.0, rgb(180, 181, 188))],
        ),
        pal::mid(),
    );
    // Shadow the tip casts on the head.
    let tip_shadow = ellipse(63.0, 29.0, 13.0, 18.5, -22.0);
    let head_clip = mask(pm, &head, ts);
    fill_in(pm, &tip_shadow, &solid(black(0.10)), ts, &head_clip);
    fill(pm, &tip, &tip_paint, ts);
    stroke(pm, &tip, 0.9, &solid(black(0.08)), ts);
    // Sound outlet with its mesh.
    fill(pm, &ellipse(76.6, 22.5, 3.9, 10.5, -22.0), &solid(rgb(96, 98, 105)), ts);
    fill(pm, &ellipse(77.2, 22.3, 2.4, 8.0, -22.0), &solid(pal::mesh()), ts);

    // Specular glint on the gloss.
    let glint = rad((45.0, 17.0), 11.0, 0.55, &[(0.0, white(0.95)), (1.0, white(0.0))]);
    fill(pm, &rect(30.0, 8.0, 30.0, 18.0), &paint(glint, white(0.0)), ts);
}

fn tapered_stem(xtl: f32, xtr: f32, xbl: f32, xbr: f32, yt: f32, yb: f32, r: f32) -> tiny_skia::Path {
    const K: f32 = 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(xtl, yt);
    pb.line_to(xtr, yt);
    pb.line_to(xbr, yb - r);
    pb.cubic_to(xbr, yb - r + r * K, xbr - r + r * K, yb, xbr - r, yb);
    pb.line_to(xbl + r, yb);
    pb.cubic_to(xbl + r - r * K, yb, xbl, yb - r + r * K, xbl, yb - r);
    pb.close();
    pb.finish().expect("stem")
}

/// A charging case seen from the front: continuous-cornered body, lid
/// seam, status light. Used landscape (Pro, 3/4) and portrait (1/2).
fn case_front(pm: &mut PixmapMut, ts: Transform, w: f32, h: f32, r: f32, seam: f32, led: Led) {
    let body = squircle(0.0, 0.0, w, h, r);
    let clip = mask(pm, &body, ts);
    let base = paint(
        lin(
            (0.0, 0.0),
            (0.0, h),
            &[
                (0.0, rgb(252, 252, 253)),
                (0.45, rgb(242, 242, 245)),
                (1.0, rgb(207, 208, 214)),
            ],
        ),
        pal::body(),
    );
    fill(pm, &body, &base, ts);
    // Rounded sides fall off into shade.
    let sides = paint(
        lin(
            (0.0, 0.0),
            (w, 0.0),
            &[
                (0.0, black(0.17)),
                (0.09, black(0.06)),
                (0.26, black(0.0)),
                (0.74, black(0.0)),
                (0.91, black(0.06)),
                (1.0, black(0.17)),
            ],
        ),
        black(0.0),
    );
    fill(pm, &body, &sides, ts);
    // Soft top sheen and a faint rim light along the bottom.
    let sheen = rad((w * 0.5, 5.0), w * 0.46, 0.24, &[(0.0, white(0.85)), (1.0, white(0.0))]);
    fill_in(pm, &rect(0.0, 0.0, w, h * 0.3), &paint(sheen, white(0.0)), ts, &clip);
    let rim = rad((w * 0.5, h - 1.0), w * 0.42, 0.12, &[(0.0, white(0.35)), (1.0, white(0.0))]);
    fill_in(pm, &rect(0.0, h * 0.8, w, h * 0.2), &paint(rim, white(0.0)), ts, &clip);

    // Lid seam: a dark hairline with a highlight under it.
    fill_in(pm, &rect(0.0, seam - 0.5, w, 1.0), &solid(rgba(128, 130, 139, 0.55)), ts, &clip);
    fill_in(pm, &rect(0.0, seam + 0.5, w, 0.9), &solid(white(0.9)), ts, &clip);
    // Shade just above the seam: the lid's lip curls in.
    let lip = lin((0.0, seam - 5.0), (0.0, seam), &[(0.0, black(0.0)), (1.0, black(0.06))]);
    fill_in(pm, &rect(0.0, seam - 5.0, w, 5.0), &paint(lip, black(0.0)), ts, &clip);

    // Status light.
    let (lx, ly) = (w * 0.5, seam + (h - seam) * 0.3);
    match led {
        Led::Off => {
            fill(pm, &circle(lx, ly, 1.9), &solid(rgba(150, 152, 160, 0.55)), ts);
        }
        Led::Charging | Led::Full => {
            // Ember green / amber, matching the rest of the card.
            let c = if led == Led::Full { rgb(0xa8, 0xc6, 0x86) } else { rgb(0xe8, 0x9b, 0x5e) };
            let glow = rad((lx, ly), 9.0, 1.0, &[(0.0, with_alpha(c, 0.55)), (1.0, with_alpha(c, 0.0))]);
            fill(pm, &rect(lx - 9.0, ly - 9.0, 18.0, 18.0), &paint(glow, black(0.0)), ts);
            fill(pm, &circle(lx, ly, 2.2), &solid(c), ts);
            fill(pm, &circle(lx - 0.5, ly - 0.6, 0.8), &solid(white(0.8)), ts);
        }
    }
    // Inner edge definition.
    stroke_in(pm, &body, 1.6, &solid(black(0.10)), ts, &clip);
}

// ──────────────────────────── AirPods 3 / 4 ─────────────────────────────

fn open(pm: &mut PixmapMut, base: Transform, led: Led) {
    let s = 0.86;
    let y = 12.0;
    let l = place(base, 0.0, y, s);
    let r = place_mirrored(base, 138.0, y, s);
    contact_shadow(pm, l, 31.0, 101.0, 15.0, 0.22, 0.38);
    contact_shadow(pm, r, 31.0, 101.0, 15.0, 0.22, 0.38);
    open_bud(pm, l);
    open_bud(pm, r);

    let c = place(base, 166.0, 12.0, 1.0);
    contact_shadow(pm, c, 58.0, 98.5, 55.0, 0.11, 0.5);
    case_front(pm, c, 116.0, 97.0, 40.0, 28.0, led);
}

/// Open-fit bud (AirPods 3/4): bulbous head, speaker facing +x, short stem.
fn open_bud(pm: &mut PixmapMut, ts: Transform) {
    let stem = tapered_stem(23.5, 40.5, 24.5, 38.0, 42.0, 100.0, 6.8);
    let stem_paint = paint(
        lin(
            (23.0, 0.0),
            (41.0, 0.0),
            &[
                (0.0, rgb(204, 205, 211)),
                (0.3, pal::hi()),
                (0.6, pal::body()),
                (1.0, rgb(182, 183, 190)),
            ],
        ),
        pal::body(),
    );
    fill(pm, &stem, &stem_paint, ts);
    let clip_cap = mask(pm, &rect(0.0, 94.0, 80.0, 10.0), ts);
    fill_in(pm, &stem, &solid(rgb(122, 124, 131)), ts, &clip_cap);
    fill(pm, &rrect(28.0, 95.3, 8.0, 2.4, 1.2), &solid(pal::mesh()), ts);
    let occ = rad((32.0, 55.0), 14.0, 0.45, &[(0.0, black(0.2)), (1.0, black(0.0))]);
    fill(pm, &rect(16.0, 48.0, 32.0, 14.0), &paint(occ, black(0.0)), ts);

    let head = ellipse(33.0, 30.5, 27.5, 26.5, -10.0);
    let head_paint = paint(
        rad(
            (37.0, 18.0),
            38.0,
            1.0,
            &[
                (0.0, pal::hi()),
                (0.45, pal::body()),
                (0.8, pal::mid()),
                (1.0, rgb(184, 185, 192)),
            ],
        ),
        pal::body(),
    );
    fill(pm, &head, &head_paint, ts);
    // Speaker mesh on the inner face.
    let clip_head = mask(pm, &head, ts);
    fill_in(pm, &ellipse(53.0, 24.0, 8.5, 13.0, -24.0), &solid(rgb(92, 94, 101)), ts, &clip_head);
    fill_in(pm, &ellipse(54.0, 23.8, 6.2, 10.5, -24.0), &solid(pal::mesh()), ts, &clip_head);
    fill(pm, &ellipse(18.0, 14.0, 5.0, 1.5, -38.0), &solid(with_alpha(pal::vent(), 0.85)), ts);
    let glint = rad((38.0, 14.0), 10.0, 0.55, &[(0.0, white(0.95)), (1.0, white(0.0))]);
    fill(pm, &rect(24.0, 6.0, 28.0, 16.0), &paint(glint, white(0.0)), ts);
}

// ──────────────────────────── AirPods 1 / 2 ─────────────────────────────

fn classic(pm: &mut PixmapMut, base: Transform, led: Led) {
    let s = 0.74;
    let y = 4.0;
    let l = place(base, 22.0, y, s);
    let r = place_mirrored(base, 142.0, y, s);
    contact_shadow(pm, l, 27.0, 145.0, 15.0, 0.22, 0.4);
    contact_shadow(pm, r, 27.0, 145.0, 15.0, 0.22, 0.4);
    classic_bud(pm, l);
    classic_bud(pm, r);

    let c = place(base, 180.0, 8.0, 1.0);
    contact_shadow(pm, c, 40.0, 103.5, 40.0, 0.12, 0.5);
    case_front(pm, c, 80.0, 102.0, 24.0, 30.0, led);
}

/// Classic bud: round head with the speaker facing +x and a long stem.
fn classic_bud(pm: &mut PixmapMut, ts: Transform) {
    let stem = tapered_stem(18.5, 36.0, 18.5, 35.0, 32.0, 144.0, 8.5);
    let stem_paint = paint(
        lin(
            (18.0, 0.0),
            (36.0, 0.0),
            &[
                (0.0, rgb(204, 205, 211)),
                (0.3, pal::hi()),
                (0.6, pal::body()),
                (1.0, rgb(182, 183, 190)),
            ],
        ),
        pal::body(),
    );
    fill(pm, &stem, &stem_paint, ts);
    // Silver microphone end.
    let clip_cap = mask(pm, &rect(0.0, 134.0, 80.0, 12.0), ts);
    let cap = paint(
        lin(
            (18.0, 0.0),
            (36.0, 0.0),
            &[(0.0, rgb(150, 152, 159)), (0.35, rgb(214, 216, 222)), (1.0, rgb(138, 140, 147))],
        ),
        pal::deep(),
    );
    fill_in(pm, &stem, &cap, ts, &clip_cap);
    let occ = rad((27.0, 44.0), 12.0, 0.45, &[(0.0, black(0.2)), (1.0, black(0.0))]);
    fill(pm, &rect(14.0, 38.0, 26.0, 12.0), &paint(occ, black(0.0)), ts);

    let head = ellipse(27.0, 24.0, 21.5, 22.0, -6.0);
    let head_paint = paint(
        rad(
            (30.0, 14.0),
            31.0,
            1.0,
            &[
                (0.0, pal::hi()),
                (0.45, pal::body()),
                (0.8, pal::mid()),
                (1.0, rgb(184, 185, 192)),
            ],
        ),
        pal::body(),
    );
    fill(pm, &head, &head_paint, ts);
    let clip_head = mask(pm, &head, ts);
    fill_in(pm, &ellipse(42.5, 20.0, 7.0, 11.0, -18.0), &solid(rgb(92, 94, 101)), ts, &clip_head);
    fill_in(pm, &ellipse(43.3, 19.8, 5.0, 8.8, -18.0), &solid(pal::mesh()), ts, &clip_head);
    let glint = rad((31.0, 11.0), 8.0, 0.55, &[(0.0, white(0.95)), (1.0, white(0.0))]);
    fill(pm, &rect(20.0, 5.0, 22.0, 12.0), &paint(glint, white(0.0)), ts);
}

// ───────────────────────────── AirPods Max ──────────────────────────────

fn max(pm: &mut PixmapMut, base: Transform) {
    // Local box 172 × 116, centred in the 200-wide composition.
    let ts = base.pre_translate(14.0, 1.0);
    let w = 172.0;
    let steel = |y0: f32, y1: f32| {
        paint(
            lin(
                (0.0, y0),
                (0.0, y1),
                &[(0.0, rgb(238, 239, 242)), (0.5, rgb(204, 206, 212)), (1.0, rgb(164, 166, 174))],
            ),
            pal::mid(),
        )
    };

    contact_shadow(pm, ts, 30.0, 113.5, 24.0, 0.14, 0.45);
    contact_shadow(pm, ts, w - 30.0, 113.5, 24.0, 0.14, 0.45);

    // Knit mesh canopy slung under the frame.
    let mut canopy = PathBuilder::new();
    canopy.move_to(34.0, 52.0);
    canopy.cubic_to(33.0, 12.0, w - 33.0, 12.0, w - 34.0, 52.0);
    let canopy = canopy.finish().expect("canopy");
    stroke(
        pm,
        &canopy,
        10.0,
        &paint(
            lin((0.0, 14.0), (0.0, 52.0), &[(0.0, rgb(216, 217, 221)), (1.0, rgb(184, 185, 191))]),
            pal::mid(),
        ),
        ts,
    );
    // Stainless headband frame with a bright edge.
    let mut frame = PathBuilder::new();
    frame.move_to(25.0, 50.0);
    frame.cubic_to(21.0, 0.0, w - 21.0, 0.0, w - 25.0, 50.0);
    let frame = frame.finish().expect("frame");
    stroke(pm, &frame, 5.0, &steel(2.0, 50.0), ts);
    stroke(pm, &frame, 1.1, &solid(white(0.6)), ts.pre_translate(-0.4, -1.2));
    // Telescoping arms into the cups.
    for x in [25.0, w - 25.0] {
        fill(pm, &rrect(x - 2.3, 44.0, 4.6, 12.0, 2.2), &steel(44.0, 56.0), ts);
    }

    // Cushions (knit, grey) sit on the inner faces, behind the cups.
    for (x, inner_left) in [(40.0, true), (w - 56.0, false)] {
        let cushion = squircle(x, 55.0, 16.0, 56.0, 7.0);
        let (a, b) = if inner_left {
            (rgb(168, 169, 176), rgb(208, 209, 214))
        } else {
            (rgb(208, 209, 214), rgb(168, 169, 176))
        };
        fill(pm, &cushion, &paint(lin((x, 0.0), (x + 16.0, 0.0), &[(0.0, a), (1.0, b)]), pal::shade()), ts);
    }
    // Aluminium cups.
    for (x, right) in [(4.0, false), (w - 46.0, true)] {
        let cup = squircle(x, 50.0, 42.0, 64.0, 15.0);
        let (lo, hi) = (rgb(196, 197, 204), rgb(250, 250, 252));
        let g = if right {
            lin((x, 0.0), (x + 42.0, 0.0), &[(0.0, lo), (0.3, hi), (0.75, rgb(232, 233, 237)), (1.0, lo)])
        } else {
            lin((x, 0.0), (x + 42.0, 0.0), &[(0.0, lo), (0.25, rgb(232, 233, 237)), (0.7, hi), (1.0, lo)])
        };
        fill(pm, &cup, &paint(g, pal::body()), ts);
        let clip = mask(pm, &cup, ts);
        let v = lin(
            (0.0, 50.0),
            (0.0, 114.0),
            &[(0.0, white(0.45)), (0.3, white(0.0)), (0.8, black(0.0)), (1.0, black(0.14))],
        );
        fill_in(pm, &cup, &paint(v, black(0.0)), ts, &clip);
        stroke_in(pm, &cup, 1.4, &solid(black(0.12)), ts, &clip);
    }
    // Digital Crown and noise-control button on the right cup.
    fill(pm, &rrect(w - 16.0, 43.5, 8.0, 8.0, 2.4), &steel(43.5, 51.5), ts);
    fill(pm, &rrect(w - 16.0, 44.8, 8.0, 1.0, 0.5), &solid(white(0.6)), ts);
    fill(pm, &rrect(w - 34.0, 46.5, 10.0, 4.0, 2.0), &steel(46.5, 50.5), ts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_product_draws_inside_its_box() {
        for p in [Product::Pro, Product::Open, Product::Classic, Product::Max] {
            let mut pm = tiny_skia::Pixmap::new(340, 150).unwrap();
            draw(&mut pm.as_mut(), 1.0, p, Led::Charging, (20.0, 16.0, 300.0, 118.0));
            let w = pm.width() as usize;
            let mut ink = 0;
            for (i, px) in pm.data().chunks(4).enumerate() {
                if px[3] > 8 {
                    ink += 1;
                    let (x, y) = (i % w, i / w);
                    assert!((10..330).contains(&x) && (6..144).contains(&y), "{p:?} spills at {x},{y}");
                }
            }
            assert!(ink > 4000, "{p:?} drew too little ({ink})");
        }
    }
}
