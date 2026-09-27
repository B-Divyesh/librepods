//! Fonts, shaping and glyph rasterisation.
//!
//! The card is set in Inter, like the rest of the desktop (mako, Waybar
//! tooltips); SF Pro, Noto Sans and finally the Noto Sans Regular embedded
//! in the binary (taken from podctl, Apache-2.0, see
//! assets/fonts/LICENSE-NotoSans.txt) are fallbacks. Icons (the charging
//! bolt) come from JetBrainsMono Nerd Font Propo, the desktop's icon font,
//! when installed. Font files are found with `fc-list` and memory-mapped,
//! so a 13 MB `Inter.ttc` costs only the pages actually touched.
//!
//! Text is shaped with rustybuzz (kerning, contextual forms) and each glyph
//! outline is rasterised with exact area coverage at its sub-pixel
//! position, then blended straight into the premultiplied pixmap.

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use ab_glyph_rasterizer::{Point as RPoint, Rasterizer, point};
use rustybuzz::ttf_parser::{GlyphId, OutlineBuilder, Tag};
use rustybuzz::{Face, Feature, UnicodeBuffer, Variation};
use tiny_skia::{Color, PixmapMut};

static EMBEDDED: &[u8] = include_bytes!("../assets/fonts/NotoSans-Regular.ttf");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular = 0,
    Medium = 1,
    Semibold = 2,
    Bold = 3,
}

impl Weight {
    const ALL: [Weight; 4] = [Weight::Regular, Weight::Medium, Weight::Semibold, Weight::Bold];

    fn css(self) -> f32 {
        match self {
            Weight::Regular => 400.0,
            Weight::Medium => 500.0,
            Weight::Semibold => 600.0,
            Weight::Bold => 700.0,
        }
    }
}

struct FaceSlot {
    face: Face<'static>,
    /// Synthetic emboldening in em units when the family lacks the weight.
    embolden: f32,
}

pub struct Fonts {
    slots: Vec<FaceSlot>,
    pub family: String,
}

static FONTS: OnceLock<Fonts> = OnceLock::new();

/// Load fonts, preferring `family` when given. Later calls are no-ops.
pub fn init(family: Option<&str>) -> &'static Fonts {
    FONTS.get_or_init(|| Fonts::load(family))
}

pub fn fonts() -> &'static Fonts {
    init(None)
}

/// fontconfig weight scale → CSS weight.
fn fc_to_css(w: f32) -> f32 {
    const MAP: [(f32, f32); 10] = [
        (0.0, 100.0),
        (40.0, 200.0),
        (50.0, 300.0),
        (75.0, 350.0),
        (80.0, 400.0),
        (100.0, 500.0),
        (180.0, 600.0),
        (200.0, 700.0),
        (205.0, 800.0),
        (210.0, 900.0),
    ];
    for win in MAP.windows(2) {
        let ((a, ca), (b, cb)) = (win[0], win[1]);
        if w <= b {
            let t = ((w - a) / (b - a)).clamp(0.0, 1.0);
            return ca + t * (cb - ca);
        }
    }
    900.0
}

#[derive(Clone, Debug)]
struct Candidate {
    file: String,
    index: u32,
    /// CSS weight range (equal ends for static faces).
    wmin: f32,
    wmax: f32,
}

fn list_family(family: &str) -> Vec<Candidate> {
    let out = Command::new("fc-list")
        .arg(format!("{family}:slant=0"))
        .arg("--format")
        .arg("%{family}\t%{weight}\t%{index}\t%{file}\n")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut v = Vec::new();
    for line in text.lines() {
        let mut it = line.split('\t');
        let (Some(fams), Some(w), Some(idx), Some(file)) = (it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        // fc-list matches loosely; insist on the exact family.
        if !fams.split(',').any(|f| f.trim().eq_ignore_ascii_case(family)) {
            continue;
        }
        let lower = file.to_ascii_lowercase();
        if !(lower.ends_with(".ttf") || lower.ends_with(".otf") || lower.ends_with(".ttc") || lower.ends_with(".otc")) {
            continue;
        }
        let (wmin, wmax) = if let Some(r) = w.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            let mut p = r.split_whitespace().filter_map(|x| x.parse::<f32>().ok());
            let a = p.next().unwrap_or(80.0);
            let b = p.next().unwrap_or(a);
            (fc_to_css(a), fc_to_css(b))
        } else {
            let c = fc_to_css(w.trim().parse::<f32>().unwrap_or(80.0));
            (c, c)
        };
        let Ok(index) = idx.trim().parse::<u32>() else { continue };
        v.push(Candidate {
            file: file.to_string(),
            index,
            wmin,
            wmax,
        });
    }
    v
}

/// Map a font file read-only for the life of the process.
fn map_file(path: &str) -> Option<&'static [u8]> {
    let f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len() as usize;
    if len == 0 {
        return None;
    }
    // SAFETY: read-only private mapping of a regular file; it is never
    // unmapped, so the 'static slice stays valid. Font files are not
    // rewritten in place by package managers (they rename over them).
    unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            f.as_raw_fd(),
            0,
        );
        if p == libc::MAP_FAILED {
            return None;
        }
        Some(std::slice::from_raw_parts(p as *const u8, len))
    }
}

impl Fonts {
    fn load(prefer: Option<&str>) -> Fonts {
        let mut families: Vec<&str> = Vec::new();
        if let Some(p) = prefer {
            families.push(p);
        }
        families.extend(["Inter", "SF Pro Display", "SF Pro Text", "SF Pro", "Noto Sans", "Cantarell"]);

        for fam in families {
            let cands = list_family(fam);
            if cands.is_empty() {
                continue;
            }
            if let Some(f) = Self::from_candidates(fam, &cands) {
                return f;
            }
        }
        Self::embedded()
    }

    fn from_candidates(family: &str, cands: &[Candidate]) -> Option<Fonts> {
        let mut maps: HashMap<String, Option<&'static [u8]>> = HashMap::new();
        let mut slots = Vec::with_capacity(4);
        for w in Weight::ALL {
            let want = w.css();
            // Nearest weight; a variable face covering `want` is exact.
            let best = cands.iter().min_by(|a, b| {
                let da = dist(a, want);
                let db = dist(b, want);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })?;
            let data = *maps.entry(best.file.clone()).or_insert_with(|| map_file(&best.file));
            let data = data?;
            let mut face = Face::from_slice(data, best.index & 0xFFFF)?;
            let variable = best.wmax > best.wmin || face.is_variable();
            let got = if variable {
                let v = want.clamp(best.wmin.min(want), best.wmax.max(best.wmin));
                face.set_variations(&[Variation {
                    tag: Tag::from_bytes(b"wght"),
                    value: v,
                }]);
                v
            } else {
                best.wmin
            };
            let embolden = if want - got >= 150.0 { 0.028 } else { 0.0 };
            slots.push(FaceSlot { face, embolden });
        }
        Some(Fonts {
            slots,
            family: family.to_string(),
        })
    }

    fn embedded() -> Fonts {
        let slots = Weight::ALL
            .iter()
            .map(|w| FaceSlot {
                face: Face::from_slice(EMBEDDED, 0).expect("embedded font parses"),
                embolden: match w {
                    Weight::Regular => 0.0,
                    Weight::Medium => 0.012,
                    Weight::Semibold => 0.024,
                    Weight::Bold => 0.032,
                },
            })
            .collect();
        Fonts {
            slots,
            family: "Noto Sans (embedded)".into(),
        }
    }

    fn slot(&self, w: Weight) -> &FaceSlot {
        &self.slots[w as usize]
    }
}

fn dist(c: &Candidate, want: f32) -> f32 {
    if want >= c.wmin && want <= c.wmax {
        0.0
    } else {
        // Prefer going heavier over lighter for the same distance: a
        // semibold title set in bold reads better than in regular.
        let d = (c.wmin - want).abs().min((c.wmax - want).abs());
        if c.wmax < want { d + 1.0 } else { d }
    }
}

static ICONS: OnceLock<Option<Face<'static>>> = OnceLock::new();

/// The desktop's icon font (Nerd Font glyphs), if installed.
fn icon_face() -> Option<&'static Face<'static>> {
    ICONS
        .get_or_init(|| {
            const FAMILY: &str = "JetBrainsMono Nerd Font Propo";
            let from_fc = list_family(FAMILY)
                .into_iter()
                .min_by(|a, b| dist(a, 400.0).partial_cmp(&dist(b, 400.0)).unwrap_or(std::cmp::Ordering::Equal));
            let (file, index) = match from_fc {
                Some(c) => (c.file, c.index & 0xFFFF),
                None => ("/usr/share/fonts/TTF/JetBrainsMonoNerdFontPropo-Regular.ttf".to_string(), 0),
            };
            Face::from_slice(map_file(&file)?, index)
        })
        .as_ref()
}

/// Draw icon `ch` from the icon font, its ink box `h` logical px tall and
/// centred on (cx, cy). False if the font or glyph is missing, so the
/// caller can fall back to a vector shape.
pub fn draw_icon(pm: &mut PixmapMut, scale: f32, ch: char, cx: f32, cy: f32, h: f32, color: Color) -> bool {
    let Some(face) = icon_face() else { return false };
    let Some(gid) = face.glyph_index(ch) else { return false };
    let Some(bb) = face.glyph_bounding_box(gid) else { return false };
    let ink_h = (bb.y_max - bb.y_min).max(1) as f32;
    let k = h * scale / ink_h;
    let ox = cx * scale - (bb.x_min as f32 + bb.x_max as f32) * 0.5 * k;
    let oy = cy * scale + (bb.y_min as f32 + bb.y_max as f32) * 0.5 * k;
    match raster_glyph(face, gid, k, ox, oy, 0.0) {
        Some(g) => {
            blend_coverage(pm, &g, color);
            true
        }
        None => false,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub size: f32,
    pub weight: Weight,
    pub color: Color,
    /// Extra advance per glyph, in px.
    pub tracking: f32,
}

impl Style {
    pub fn new(size: f32, weight: Weight, color: Color) -> Self {
        Style {
            size,
            weight,
            color,
            tracking: 0.0,
        }
    }
    pub fn tracking(mut self, t: f32) -> Self {
        self.tracking = t;
        self
    }
}

pub struct Shaped {
    /// (glyph, x, y) in px relative to the pen origin, y down.
    glyphs: Vec<(u16, f32, f32)>,
    pub width: f32,
    weight: Weight,
    size: f32,
}

pub fn shape(text: &str, style: &Style) -> Shaped {
    let slot = fonts().slot(style.weight);
    let face = &slot.face;
    let mut buf = UnicodeBuffer::new();
    buf.push_str(text);
    // Tabular figures keep "100%" and "88%" visually centred alike.
    let feats = [Feature::new(Tag::from_bytes(b"tnum"), 1, ..)];
    let out = rustybuzz::shape(face, &feats, buf);
    let k = style.size / face.units_per_em() as f32;
    let bold_extra = slot.embolden * style.size;
    let mut pen = 0.0;
    let mut glyphs = Vec::with_capacity(out.len());
    for (info, pos) in out.glyph_infos().iter().zip(out.glyph_positions()) {
        glyphs.push((
            info.glyph_id as u16,
            pen + pos.x_offset as f32 * k,
            -(pos.y_offset as f32) * k,
        ));
        pen += pos.x_advance as f32 * k + style.tracking + bold_extra;
    }
    let width = if glyphs.is_empty() {
        0.0
    } else {
        pen - style.tracking
    };
    Shaped {
        glyphs,
        width,
        weight: style.weight,
        size: style.size,
    }
}

pub fn measure(text: &str, style: &Style) -> f32 {
    shape(text, style).width
}

/// Height of a capital letter, for optical vertical centring.
pub fn cap_height(style: &Style) -> f32 {
    let face = &fonts().slot(style.weight).face;
    let cap = face
        .capital_height()
        .map(|c| c as f32)
        .unwrap_or(face.units_per_em() as f32 * 0.7);
    cap * style.size / face.units_per_em() as f32
}

/// Shrink towards `min` size, then ellipsize, so `text` fits `max_w`.
/// (Adapted from podctl's `fit`.)
pub fn fit(text: &str, max_w: f32, style: &Style, min: f32) -> (String, Style) {
    let mut st = *style;
    while st.size > min && measure(text, &st) > max_w {
        st.size -= 0.5;
    }
    if measure(text, &st) <= max_w {
        return (text.to_string(), st);
    }
    let chars: Vec<char> = text.chars().collect();
    for n in (1..chars.len()).rev() {
        let s: String = chars[..n].iter().collect::<String>().trim_end().to_string() + "…";
        if measure(&s, &st) <= max_w {
            return (s, st);
        }
    }
    ("…".into(), st)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
}

/// Draw `text` with its baseline at `y`. Coordinates are logical; `scale`
/// maps them to pixmap pixels. Returns the logical width.
pub fn draw(pm: &mut PixmapMut, scale: f32, text: &str, x: f32, y: f32, align: Align, style: &Style) -> f32 {
    let sh = shape(text, style);
    let x0 = match align {
        Align::Left => x,
        Align::Center => x - sh.width * 0.5,
    };
    draw_shaped(pm, scale, &sh, x0, y, style.color);
    sh.width
}

pub fn draw_shaped(pm: &mut PixmapMut, scale: f32, sh: &Shaped, x: f32, y: f32, color: Color) {
    let slot = fonts().slot(sh.weight);
    let face = &slot.face;
    let k = sh.size * scale / face.units_per_em() as f32;
    let bold_px = slot.embolden * sh.size * scale;
    for &(gid, gx, gy) in &sh.glyphs {
        if gid == 0 {
            // .notdef: a glyph the font lacks (emoji in a device name).
            // A gap reads better than a tofu box.
            continue;
        }
        let ox = (x + gx) * scale;
        let oy = (y + gy) * scale;
        if let Some(g) = raster_glyph(face, GlyphId(gid), k, ox, oy, bold_px) {
            blend_coverage(pm, &g, color);
        }
    }
}

struct GlyphBitmap {
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    cov: Vec<f32>,
}

#[derive(Clone, Copy)]
enum Seg {
    Line(f32, f32, f32, f32),
    Quad(f32, f32, f32, f32, f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32, f32, f32),
}

#[derive(Default)]
struct Collect {
    segs: Vec<Seg>,
    start: (f32, f32),
    cur: (f32, f32),
}

impl OutlineBuilder for Collect {
    fn move_to(&mut self, x: f32, y: f32) {
        self.start = (x, y);
        self.cur = (x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.segs.push(Seg::Line(self.cur.0, self.cur.1, x, y));
        self.cur = (x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.segs.push(Seg::Quad(self.cur.0, self.cur.1, x1, y1, x, y));
        self.cur = (x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.segs.push(Seg::Cubic(self.cur.0, self.cur.1, x1, y1, x2, y2, x, y));
        self.cur = (x, y);
    }
    fn close(&mut self) {
        if self.cur != self.start {
            self.segs.push(Seg::Line(self.cur.0, self.cur.1, self.start.0, self.start.1));
        }
        self.cur = self.start;
    }
}

fn raster_glyph(face: &Face, gid: GlyphId, k: f32, ox: f32, oy: f32, bold_px: f32) -> Option<GlyphBitmap> {
    let mut c = Collect::default();
    let bb = face.outline_glyph(gid, &mut c)?;
    c.close();
    let x0 = (ox + bb.x_min as f32 * k).floor() as i32 - 1;
    let x1 = (ox + bb.x_max as f32 * k + bold_px).ceil() as i32 + 1;
    let y0 = (oy - bb.y_max as f32 * k).floor() as i32 - 1;
    let y1 = (oy - bb.y_min as f32 * k).ceil() as i32 + 1;
    let (w, h) = ((x1 - x0).max(1) as usize, (y1 - y0).max(1) as usize);
    if w > 4096 || h > 4096 {
        return None;
    }

    let raster = |dx: f32| {
        let mut r = Rasterizer::new(w, h);
        let p = |x: f32, y: f32| -> RPoint { point(ox + dx + x * k - x0 as f32, oy - y * k - y0 as f32) };
        for s in &c.segs {
            match *s {
                Seg::Line(a, b, x, y) => r.draw_line(p(a, b), p(x, y)),
                Seg::Quad(a, b, c1, d1, x, y) => r.draw_quad(p(a, b), p(c1, d1), p(x, y)),
                Seg::Cubic(a, b, c1, d1, e, f, x, y) => r.draw_cubic(p(a, b), p(c1, d1), p(e, f), p(x, y)),
            }
        }
        let mut cov = vec![0.0f32; w * h];
        r.for_each_pixel(|i, a| cov[i] = a.min(1.0));
        cov
    };

    let mut cov = raster(0.0);
    if bold_px > 0.05 {
        // Synthetic bold: union of the outline and a copy nudged right.
        let steps = (bold_px / 0.5).ceil().max(1.0) as usize;
        for i in 1..=steps {
            let extra = raster(bold_px * i as f32 / steps as f32);
            for (a, b) in cov.iter_mut().zip(extra) {
                *a = a.max(b);
            }
        }
    }
    Some(GlyphBitmap {
        x: x0,
        y: y0,
        w,
        h,
        cov,
    })
}

/// Source-over blend of `color` through a coverage mask into a
/// premultiplied RGBA pixmap. A mild gamma lift on coverage keeps light
/// text on the dark card from looking thin (what macOS calls font
/// smoothing, minus the colour fringes).
fn blend_coverage(pm: &mut PixmapMut, g: &GlyphBitmap, color: Color) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    let c = color.premultiply();
    let (cr, cg, cb, ca) = (c.red(), c.green(), c.blue(), c.alpha());
    let lum = 0.2126 * color.red() + 0.7152 * color.green() + 0.0722 * color.blue();
    let gamma = if lum > 0.5 { 0.80 } else { 1.0 };
    for gy in 0..g.h {
        let py = g.y + gy as i32;
        if py < 0 || py >= ph {
            continue;
        }
        for gx in 0..g.w {
            let px = g.x + gx as i32;
            if px < 0 || px >= pw {
                continue;
            }
            let cov = g.cov[gy * g.w + gx];
            if cov <= 0.002 {
                continue;
            }
            let cov = if gamma != 1.0 { cov.powf(gamma) } else { cov };
            let a = ca * cov;
            let inv = 1.0 - a;
            let i = ((py * pw + px) * 4) as usize;
            let d = &mut data[i..i + 4];
            d[0] = (cr * cov * 255.0 + d[0] as f32 * inv + 0.5) as u8;
            d[1] = (cg * cov * 255.0 + d[1] as f32 * inv + 0.5) as u8;
            d[2] = (cb * cov * 255.0 + d[2] as f32 * inv + 0.5) as u8;
            d[3] = (a * 255.0 + d[3] as f32 * inv + 0.5) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(size: f32) -> Style {
        Style::new(size, Weight::Semibold, Color::WHITE)
    }

    #[test]
    fn short_name_keeps_ideal_size() {
        let (s, style) = fit("AirPods Pro", 240.0, &st(19.0), 14.0);
        assert_eq!(s, "AirPods Pro");
        assert_eq!(style.size, 19.0);
    }

    #[test]
    fn long_name_never_overflows() {
        let long = "Somebody's Extremely Long AirPods Pro (2nd generation, USB-C)";
        let (s, style) = fit(long, 200.0, &st(19.0), 14.0);
        assert!(measure(&s, &style) <= 200.0);
        assert!(s.ends_with('…'));
        assert!(style.size >= 14.0);
    }

    #[test]
    fn text_puts_ink_down() {
        let mut pm = tiny_skia::Pixmap::new(120, 40).unwrap();
        draw(&mut pm.as_mut(), 1.0, "Case 55%", 4.0, 28.0, Align::Left, &st(16.0));
        let inked = pm.data().chunks(4).filter(|p| p[3] > 128).count();
        assert!(inked > 60, "only {inked} px inked");
    }
}
