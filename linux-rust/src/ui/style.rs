//! Apple-style building blocks (macOS System Settings / iOS Settings), tinted
//! by the active theme. Tuned for the Ember palette, but every colour is
//! derived from `theme.palette()` so the other themes still look coherent.

use crate::ui::window::Message;
use iced::font::Weight;
use iced::widget::{
    Space, button, column, container, image, pick_list, row, scrollable, slider, stack, text,
    text_input, toggler,
};
use iced::{
    Background, Border, Center, Color, Element, Font, Length, Padding, Shadow, Theme, Vector,
    gradient,
};
use std::cell::RefCell;
use std::collections::HashMap;

// ── Type ─────────────────────────────────────────────────────────────────────

pub const UI: Font = Font::with_name("Inter");
pub const MEDIUM: Font = Font {
    weight: Weight::Medium,
    ..Font::with_name("Inter")
};
pub const SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..Font::with_name("Inter")
};
pub const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::with_name("Inter")
};
/// Material Design glyphs from the Nerd Font the desktop already uses.
pub const GLYPHS: Font = Font::with_name("JetBrainsMono Nerd Font Propo");

pub const G_EARBUDS: &str = "\u{f184f}";
pub const G_HEADPHONES: &str = "\u{f02cb}";
pub const G_ANC: &str = "\u{f0a45}";
pub const G_TRANSPARENCY: &str = "\u{f07c5}";
pub const G_ADAPTIVE: &str = "\u{f0674}";
pub const G_BOLT: &str = "\u{f140b}";
pub const G_GEAR: &str = "\u{f0493}";
pub const G_BLUETOOTH: &str = "\u{f00af}";
pub const G_COPY: &str = "\u{f018f}";

// ── Colour tokens ────────────────────────────────────────────────────────────

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

fn luminance(c: Color) -> f32 {
    0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
}

pub fn is_dark(t: &Theme) -> bool {
    luminance(t.palette().background) < 0.45
}

pub fn bg(t: &Theme) -> Color {
    t.palette().background
}

/// Grouped-list card surface.
pub fn surface(t: &Theme) -> Color {
    let p = t.palette();
    if is_dark(t) {
        mix(p.background, p.text, 0.065)
    } else {
        mix(p.background, Color::WHITE, 0.75)
    }
}

pub fn surface_hover(t: &Theme) -> Color {
    let p = t.palette();
    if is_dark(t) {
        mix(p.background, p.text, 0.11)
    } else {
        mix(p.background, p.text, 0.06)
    }
}

/// Controls sitting on a card (pickers, tiles).
pub fn control(t: &Theme) -> Color {
    let p = t.palette();
    if is_dark(t) {
        mix(p.background, p.text, 0.13)
    } else {
        mix(p.background, p.text, 0.07)
    }
}

pub fn sidebar(t: &Theme) -> Color {
    let p = t.palette();
    if is_dark(t) {
        mix(p.background, Color::BLACK, 0.28)
    } else {
        mix(p.background, p.text, 0.035)
    }
}

pub fn hairline(t: &Theme) -> Color {
    t.palette().text.scale_alpha(if is_dark(t) { 0.075 } else { 0.10 })
}

pub fn secondary(t: &Theme) -> Color {
    t.palette().text.scale_alpha(0.62)
}

pub fn tertiary(t: &Theme) -> Color {
    t.palette().text.scale_alpha(0.40)
}

pub fn accent(t: &Theme) -> Color {
    t.palette().primary
}

/// Text/glyph colour on an accent fill.
pub fn on_accent(t: &Theme) -> Color {
    if luminance(t.palette().primary) > 0.5 {
        mix(t.palette().background, Color::BLACK, 0.2)
    } else {
        Color::from_rgb8(0xfb, 0xf7, 0xf2)
    }
}

/// "Connected" blue, as in the Bluetooth menu.
pub fn connected(t: &Theme) -> Color {
    if is_dark(t) {
        Color::from_rgb8(0x8c, 0xb3, 0xd9)
    } else {
        Color::from_rgb8(0x2f, 0x6b, 0xa8)
    }
}

pub fn battery_color(t: &Theme, level: u8, charging: bool) -> Color {
    let p = t.palette();
    if charging || level > 20 {
        p.success
    } else if level > 10 {
        p.warning
    } else {
        p.danger
    }
}

pub fn text_color(c: fn(&Theme) -> Color) -> impl Fn(&Theme) -> text::Style {
    move |t| text::Style { color: Some(c(t)) }
}

// ── Layout ───────────────────────────────────────────────────────────────────

/// A scrollable page: large title, optional subtitle, centred content column.
pub fn page<'a>(title: String, content: Element<'a, Message>) -> Element<'a, Message> {
    let body = column![
        text(title).font(BOLD).size(26),
        Space::new().height(Length::Fixed(18.0)),
        content,
        Space::new().height(Length::Fixed(28.0)),
    ]
    .max_width(680);
    scrollable(
        container(body)
            .padding(Padding {
                top: 26.0,
                bottom: 0.0,
                left: 28.0,
                right: 28.0,
            })
            .center_x(Length::Fill),
    )
    .id(PAGE_SCROLL)
    .style(scroll_style)
    .height(Length::Fill)
    .into()
}

/// Id of the page scrollable (used to restore/set the scroll position).
pub const PAGE_SCROLL: &str = "librepods-page";

pub fn section_header<'a>(title: &str) -> Element<'a, Message> {
    container(
        text(title.to_uppercase())
            .font(SEMIBOLD)
            .size(11)
            .style(text_color(secondary)),
    )
    .padding(Padding {
        top: 0.0,
        bottom: 7.0,
        left: 14.0,
        right: 14.0,
    })
    .into()
}

pub fn section_footer<'a>(note: String) -> Element<'a, Message> {
    container(text(note).size(11.5).style(text_color(tertiary)))
        .padding(Padding {
            top: 7.0,
            bottom: 0.0,
            left: 14.0,
            right: 14.0,
        })
        .into()
}

fn divider<'a>() -> Element<'a, Message> {
    container(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(1.0))
            .style(|t: &Theme| container::Style {
                background: Some(Background::Color(hairline(t))),
                ..Default::default()
            }),
    )
    .padding(Padding {
        top: 0.0,
        bottom: 0.0,
        left: 14.0,
        right: 0.0,
    })
    .into()
}

pub fn card_style(t: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(surface(t))),
        border: Border {
            width: 1.0,
            color: t.palette().text.scale_alpha(0.045),
            radius: 12.0.into(),
        },
        shadow: Shadow {
            color: Color::BLACK.scale_alpha(if is_dark(t) { 0.18 } else { 0.06 }),
            offset: Vector::new(0.0, 1.0),
            blur_radius: 6.0,
        },
        ..Default::default()
    }
}

/// Grouped inset list: rounded card, hairlines between rows.
pub fn group<'a>(rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut col = column![];
    let n = rows.len();
    for (i, r) in rows.into_iter().enumerate() {
        col = col.push(r);
        if i + 1 < n {
            col = col.push(divider());
        }
    }
    container(col).width(Length::Fill).style(card_style).into()
}

pub fn section<'a>(
    title: Option<&str>,
    rows: Vec<Element<'a, Message>>,
    footer: Option<String>,
) -> Element<'a, Message> {
    let mut col = column![];
    if let Some(t) = title {
        col = col.push(section_header(t));
    }
    col = col.push(group(rows));
    if let Some(f) = footer {
        col = col.push(section_footer(f));
    }
    container(col)
        .padding(Padding {
            top: 0.0,
            bottom: 22.0,
            left: 0.0,
            right: 0.0,
        })
        .into()
}

/// Title + optional subtitle on the left, a control on the right.
pub fn list_row<'a>(
    icon: Option<Element<'a, Message>>,
    title: impl text::IntoFragment<'a>,
    subtitle: Option<String>,
    trailing: Element<'a, Message>,
) -> Element<'a, Message> {
    let mut labels = column![text(title).size(14)].spacing(2).width(Length::Fill);
    if let Some(s) = subtitle {
        labels = labels.push(text(s).size(12).style(text_color(secondary)));
    }
    let mut r = row![].spacing(12).align_y(Center);
    if let Some(i) = icon {
        r = r.push(i);
    }
    container(r.push(labels).push(trailing))
        .padding(Padding {
            top: 10.0,
            bottom: 10.0,
            left: 14.0,
            right: 14.0,
        })
        .width(Length::Fill)
        .into()
}

/// A full-width custom block inside a group (sliders, tiles, meters).
pub fn block<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(content)
        .padding(Padding {
            top: 12.0,
            bottom: 12.0,
            left: 14.0,
            right: 14.0,
        })
        .width(Length::Fill)
        .into()
}

/// A value shown in secondary colour (About rows).
pub fn value<'a>(v: String) -> Element<'a, Message> {
    text(v).size(13.5).style(text_color(secondary)).into()
}

/// Rounded-square coloured icon, like System Settings' sidebar.
pub fn icon_tile<'a>(glyph: &'a str, tint: fn(&Theme) -> Color) -> Element<'a, Message> {
    container(
        text(glyph)
            .font(GLYPHS)
            .size(15)
            .style(move |t: &Theme| text::Style {
                color: Some(on_tint(tint(t))),
            }),
    )
    .center(Length::Fixed(26.0))
    .style(move |t: &Theme| {
        let c = tint(t);
        container::Style {
            background: Some(Background::Gradient(
                gradient::Linear::new(0.0)
                    .add_stop(0.0, mix(c, Color::WHITE, 0.12))
                    .add_stop(1.0, mix(c, Color::BLACK, 0.08))
                    .into(),
            )),
            border: Border {
                radius: 7.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
    .into()
}

fn on_tint(c: Color) -> Color {
    if luminance(c) > 0.55 {
        Color::from_rgb8(0x1a, 0x14, 0x1b)
    } else {
        Color::from_rgb8(0xfb, 0xf7, 0xf2)
    }
}

pub fn tint_accent(t: &Theme) -> Color {
    accent(t)
}
pub fn tint_blue(t: &Theme) -> Color {
    connected(t)
}
pub fn tint_gold(t: &Theme) -> Color {
    t.palette().warning
}
pub fn tint_grey(t: &Theme) -> Color {
    mix(t.palette().background, t.palette().text, 0.45)
}

// ── Controls ─────────────────────────────────────────────────────────────────

pub fn toggle_style(t: &Theme, status: toggler::Status) -> toggler::Style {
    let (on, hovered, disabled) = match status {
        toggler::Status::Active { is_toggled } => (is_toggled, false, false),
        toggler::Status::Hovered { is_toggled } => (is_toggled, true, false),
        toggler::Status::Disabled { is_toggled } => (is_toggled, false, true),
    };
    let mut track = if on {
        accent(t)
    } else {
        t.palette().text.scale_alpha(if is_dark(t) { 0.16 } else { 0.14 })
    };
    if hovered {
        track = mix(track, t.palette().text, 0.08);
    }
    let knob = Color::from_rgb8(0xfb, 0xf7, 0xf2);
    toggler::Style {
        background: Background::Color(if disabled { track.scale_alpha(0.45) } else { track }),
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground: Background::Color(if disabled { knob.scale_alpha(0.6) } else { knob }),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: None,
        border_radius: None,
        padding_ratio: 0.1,
    }
}

pub fn switch<'a>(value: bool) -> toggler::Toggler<'a, Message> {
    toggler(value).size(22).style(toggle_style)
}

pub fn picker_style(t: &Theme, status: pick_list::Status) -> pick_list::Style {
    let hovered = matches!(
        status,
        pick_list::Status::Hovered | pick_list::Status::Opened { .. }
    );
    pick_list::Style {
        text_color: t.palette().text,
        placeholder_color: tertiary(t),
        handle_color: secondary(t),
        background: Background::Color(if hovered { surface_hover(t) } else { control(t) }),
        border: Border {
            width: 0.0,
            color: Color::TRANSPARENT,
            radius: 8.0.into(),
        },
    }
}

pub fn menu_style(t: &Theme) -> iced::overlay::menu::Style {
    iced::overlay::menu::Style {
        background: Background::Color(mix(bg(t), t.palette().text, 0.09)),
        border: Border {
            width: 1.0,
            color: hairline(t),
            radius: 10.0.into(),
        },
        text_color: t.palette().text,
        selected_text_color: on_accent(t),
        selected_background: Background::Color(accent(t)),
        shadow: Shadow {
            color: Color::BLACK.scale_alpha(0.35),
            offset: Vector::new(0.0, 6.0),
            blur_radius: 18.0,
        },
    }
}

pub fn picker<'a, T>(
    options: Vec<T>,
    selected: Option<T>,
    on_select: impl Fn(T) -> Message + 'a,
) -> Element<'a, Message>
where
    T: ToString + PartialEq + Clone + 'a,
{
    pick_list(options, selected, on_select)
        .text_size(13)
        .padding(Padding {
            top: 5.0,
            bottom: 5.0,
            left: 10.0,
            right: 8.0,
        })
        .style(picker_style)
        .menu_style(menu_style)
        .into()
}

pub fn field_style(t: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: Background::Color(if focused { control(t) } else { Color::TRANSPARENT }),
        border: Border {
            width: 0.0,
            color: Color::TRANSPARENT,
            radius: 7.0.into(),
        },
        icon: secondary(t),
        placeholder: tertiary(t),
        value: if focused { t.palette().text } else { secondary(t) },
        selection: accent(t).scale_alpha(0.4),
    }
}

pub fn slider_style(t: &Theme, _status: slider::Status) -> slider::Style {
    slider::Style {
        rail: slider::Rail {
            backgrounds: (
                Background::Color(accent(t)),
                Background::Color(t.palette().text.scale_alpha(0.14)),
            ),
            width: 4.0,
            border: Border {
                radius: 2.0.into(),
                ..Default::default()
            },
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: 9.0 },
            background: Background::Color(Color::from_rgb8(0xfb, 0xf7, 0xf2)),
            border_width: 0.5,
            border_color: Color::BLACK.scale_alpha(0.25),
        },
    }
}

pub fn scroll_style(t: &Theme, status: scrollable::Status) -> scrollable::Style {
    let active = !matches!(status, scrollable::Status::Active { .. });
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(
                t.palette().text.scale_alpha(if active { 0.30 } else { 0.14 }),
            ),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: Background::Color(surface(t)),
            border: Border::default(),
            shadow: Shadow::default(),
            icon: t.palette().text,
        },
    }
}

#[derive(Clone, Copy)]
pub enum Pill {
    Primary,
    Secondary,
}

pub fn pill<'a>(label: &'a str, kind: Pill, msg: Option<Message>) -> Element<'a, Message> {
    button(text(label).font(MEDIUM).size(13))
        .padding(Padding {
            top: 6.0,
            bottom: 6.0,
            left: 16.0,
            right: 16.0,
        })
        .on_press_maybe(msg)
        .style(move |t: &Theme, status: button::Status| {
            let (fill, fg) = match kind {
                Pill::Primary => (accent(t), on_accent(t)),
                Pill::Secondary => (control(t), t.palette().text),
            };
            let fill = match status {
                button::Status::Hovered => mix(fill, t.palette().text, 0.08),
                button::Status::Pressed => mix(fill, Color::BLACK, 0.12),
                button::Status::Disabled => fill.scale_alpha(0.5),
                button::Status::Active => fill,
            };
            button::Style {
                background: Some(Background::Color(fill)),
                text_color: fg,
                border: Border {
                    radius: 999.0.into(),
                    ..Default::default()
                },
                shadow: Shadow::default(),
                snap: true,
            }
        })
        .into()
}

/// Borderless text button (copyable values, links).
pub fn plain_button<'a>(content: Element<'a, Message>, msg: Message) -> Element<'a, Message> {
    button(content)
        .padding(0)
        .on_press(msg)
        .style(|t: &Theme, status: button::Status| button::Style {
            background: None,
            text_color: if matches!(status, button::Status::Hovered) {
                t.palette().text
            } else {
                secondary(t)
            },
            border: Border::default(),
            shadow: Shadow::default(),
            snap: true,
        })
        .into()
}

/// Control-Center-style tile (listening modes).
pub fn tile<'a>(
    glyph: &'a str,
    label: &'a str,
    selected: bool,
    tint: fn(&Theme) -> Color,
    msg: Message,
) -> Element<'a, Message> {
    button(
        column![
            text(glyph)
                .font(GLYPHS)
                .size(24)
                .style(move |t: &Theme| text::Style {
                    color: Some(if selected { on_accent(t) } else { tint(t) }),
                }),
            text(label).font(MEDIUM).size(11.5),
        ]
        .spacing(6)
        .width(Length::Fill)
        .align_x(Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(76.0))
    .padding(Padding {
        top: 12.0,
        bottom: 10.0,
        left: 6.0,
        right: 6.0,
    })
    .on_press(msg)
    .style(move |t: &Theme, status: button::Status| {
        let hovered = matches!(status, button::Status::Hovered);
        let (fill, fg) = if selected {
            (accent(t), on_accent(t))
        } else if hovered {
            (surface_hover(t), t.palette().text)
        } else {
            (control(t), t.palette().text)
        };
        button::Style {
            background: Some(Background::Color(fill)),
            text_color: fg,
            border: Border {
                radius: 14.0.into(),
                ..Default::default()
            },
            shadow: if selected {
                Shadow {
                    color: accent(t).scale_alpha(0.35),
                    offset: Vector::new(0.0, 2.0),
                    blur_radius: 10.0,
                }
            } else {
                Shadow::default()
            },
            snap: true,
        }
    })
    .into()
}

/// Small on/off chip (which modes a long press cycles through).
pub fn chip<'a>(label: &'a str, on: bool, msg: Message) -> Element<'a, Message> {
    button(text(label).font(MEDIUM).size(12))
        .padding(Padding {
            top: 5.0,
            bottom: 5.0,
            left: 12.0,
            right: 12.0,
        })
        .on_press(msg)
        .style(move |t: &Theme, status: button::Status| {
            let hovered = matches!(status, button::Status::Hovered);
            let (fill, fg, border) = if on {
                (accent(t).scale_alpha(0.22), accent(t), accent(t).scale_alpha(0.55))
            } else {
                (
                    if hovered { surface_hover(t) } else { control(t) },
                    secondary(t),
                    Color::TRANSPARENT,
                )
            };
            button::Style {
                background: Some(Background::Color(fill)),
                text_color: fg,
                border: Border {
                    width: 1.0,
                    color: border,
                    radius: 999.0.into(),
                },
                shadow: Shadow::default(),
                snap: true,
            }
        })
        .into()
}

// ── Battery rings ────────────────────────────────────────────────────────────

fn pack(c: Color) -> u32 {
    let [r, g, b, a] = c.into_rgba8();
    u32::from_be_bytes([r, g, b, a])
}

thread_local! {
    static RINGS: RefCell<HashMap<(u16, u32, u32, u32), image::Handle>> = RefCell::new(HashMap::new());
}

/// Anti-aliased progress ring with round caps, rendered once per value.
/// `px` is the pixel size (render at 2× the logical size for HiDPI).
pub fn ring(fraction: f32, color: Color, track: Color, px: u32) -> image::Handle {
    let key = ((fraction.clamp(0.0, 1.0) * 1000.0) as u16, pack(color), pack(track), px);
    if let Some(h) = RINGS.with(|m| m.borrow().get(&key).cloned()) {
        return h;
    }
    let s = px as f32;
    let thick = s * 0.095;
    let r = s / 2.0 - thick / 2.0 - 1.0;
    let (cx, cy) = (s / 2.0, s / 2.0);
    let frac = key.0 as f32 / 1000.0;
    let end = frac * std::f32::consts::TAU;
    let start_pt = (cx, cy - r);
    let end_pt = (cx + r * end.sin(), cy - r * end.cos());
    let [tr, tg, tb, ta] = track.into_rgba8();
    let [ar, ag, ab, aa] = color.into_rgba8();
    let mut px_buf = vec![0u8; (px * px * 4) as usize];
    for y in 0..px {
        for x in 0..px {
            let (fx, fy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let d = (fx * fx + fy * fy).sqrt();
            let band = (d - r).abs() - thick / 2.0;
            let a_track = (0.5 - band).clamp(0.0, 1.0) * ta as f32 / 255.0;
            let a_arc = if frac <= 0.0 {
                0.0
            } else {
                let mut theta = fx.atan2(-fy);
                if theta < 0.0 {
                    theta += std::f32::consts::TAU;
                }
                let dist = if frac >= 1.0 || theta <= end {
                    band
                } else {
                    let ds = ((x as f32 + 0.5 - start_pt.0).powi(2)
                        + (y as f32 + 0.5 - start_pt.1).powi(2))
                    .sqrt();
                    let de = ((x as f32 + 0.5 - end_pt.0).powi(2)
                        + (y as f32 + 0.5 - end_pt.1).powi(2))
                    .sqrt();
                    ds.min(de) - thick / 2.0
                };
                (0.5 - dist).clamp(0.0, 1.0) * aa as f32 / 255.0
            };
            let out_a = a_arc + a_track * (1.0 - a_arc);
            let i = ((y * px + x) * 4) as usize;
            if out_a > 0.0 {
                let blend = |ac: u8, tc: u8| {
                    ((ac as f32 * a_arc + tc as f32 * a_track * (1.0 - a_arc)) / out_a) as u8
                };
                px_buf[i] = blend(ar, tr);
                px_buf[i + 1] = blend(ag, tg);
                px_buf[i + 2] = blend(ab, tb);
                px_buf[i + 3] = (out_a * 255.0) as u8;
            }
        }
    }
    let h = image::Handle::from_rgba(px, px, px_buf);
    RINGS.with(|m| m.borrow_mut().insert(key, h.clone()));
    h
}

/// iOS battery-widget gauge: ring with the part's picture inside, level and label below.
pub fn battery_gauge<'a>(
    theme: &Theme,
    label: &'a str,
    part: Option<image::Handle>,
    level: Option<(u8, bool)>,
    size: f32,
) -> Element<'a, Message> {
    let track = theme.palette().text.scale_alpha(0.12);
    let (frac, color) = match level {
        Some((l, c)) => (l as f32 / 100.0, battery_color(theme, l, c)),
        None => (0.0, track),
    };
    let ring_img = image(ring(frac, color, track, (size * 2.0) as u32))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size));
    let inner: Element<'a, Message> = match part {
        Some(h) => image(h)
            .height(Length::Fixed(size * 0.46))
            .filter_method(image::FilterMethod::Linear)
            .into(),
        None => text(G_EARBUDS)
            .font(GLYPHS)
            .size(size * 0.36)
            .style(text_color(secondary))
            .into(),
    };
    let gauge = stack![ring_img, container(inner).center(Length::Fixed(size))];
    let level_text: Element<'a, Message> = match level {
        Some((l, charging)) => {
            let mut r = row![text(format!("{l}%")).font(SEMIBOLD).size(13)]
                .spacing(2)
                .align_y(Center);
            if charging {
                r = r.push(
                    text(G_BOLT)
                        .font(GLYPHS)
                        .size(12)
                        .style(|t: &Theme| text::Style {
                            color: Some(t.palette().success),
                        }),
                );
            }
            r.into()
        }
        None => text("—").font(SEMIBOLD).size(13).style(text_color(tertiary)).into(),
    };
    column![
        gauge,
        Space::new().height(Length::Fixed(4.0)),
        level_text,
        text(label).size(11).style(text_color(secondary)),
    ]
    .align_x(Center)
    .spacing(1)
    .into()
}

// ── Product images ───────────────────────────────────────────────────────────

pub struct Art {
    pub hero: image::Handle,
    pub left: Option<image::Handle>,
    pub right: Option<image::Handle>,
    pub case: Option<image::Handle>,
    pub thumb: image::Handle,
}

macro_rules! asset {
    ($f:literal) => {
        image::Handle::from_bytes(
            include_bytes!(concat!("../../assets/devices/", $f)).as_slice(),
        )
    };
}

thread_local! {
    static ART: RefCell<HashMap<u16, std::rc::Rc<Art>>> = RefCell::new(HashMap::new());
}

/// Product pictures by Apple model id (unknown → AirPods Pro 2).
pub fn art(model: Option<u16>) -> std::rc::Rc<Art> {
    let id = model.unwrap_or(0x2024);
    if let Some(a) = ART.with(|m| m.borrow().get(&id).cloned()) {
        return a;
    }
    let a = std::rc::Rc::new(match id {
        0x200A | 0x201F | 0x202D => Art {
            hero: asset!("podmax.png"),
            left: None,
            right: None,
            case: None,
            thumb: asset!("podmax.png"),
        },
        0x2002 | 0x200F => Art {
            hero: asset!("pod_case.png"),
            left: Some(asset!("pod.png")),
            right: Some(asset!("pod.png")),
            case: Some(asset!("pod_case.png")),
            thumb: asset!("pod_case.png"),
        },
        0x2013 => Art {
            hero: asset!("pod3_case.png"),
            left: Some(asset!("pod3.png")),
            right: Some(asset!("pod3.png")),
            case: Some(asset!("pod3_case.png")),
            thumb: asset!("pod3_case.png"),
        },
        0x2019 | 0x201B => Art {
            hero: asset!("pod4_case.png"),
            left: Some(asset!("pod3.png")),
            right: Some(asset!("pod3.png")),
            case: Some(asset!("pod4_case.png")),
            thumb: asset!("pod4_case.png"),
        },
        _ => Art {
            hero: asset!("pro2_hero.png"),
            left: Some(asset!("pro2_left.png")),
            right: Some(asset!("pro2_right.png")),
            case: Some(asset!("pro2_case.png")),
            thumb: asset!("pro2_hero.png"),
        },
    });
    ART.with(|m| m.borrow_mut().insert(id, a.clone()));
    a
}
