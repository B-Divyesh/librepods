//! The AirPods pane, modelled on macOS System Settings › AirPods / iOS
//! Settings › AirPods: a hero card with the product, battery rings and a
//! Connect button, Control-Center style listening-mode tiles, then grouped
//! inset lists.

use crate::bluetooth::aacp::{
    AACPManager, BatteryComponent, BatteryInfo, BatteryStatus, ControlCommandIdentifiers as Id,
};
use crate::devices::enums::{AirPodsState, DeviceData, DeviceInformation, DeviceState};
use crate::ui::airpods_extra::{self, has_adaptive};
use crate::ui::style::{self, Pill};
use crate::ui::window::Message;
use iced::widget::{Space, column, container, image, row, text, text_input};
use iced::{Background, Border, Center, Color, Element, Length, Padding, Theme, gradient};
use std::collections::HashMap;
use std::sync::Arc;

type Level = Option<(u8, bool)>;

pub struct Levels {
    pub left: Level,
    pub right: Level,
    pub case: Level,
    pub headphone: Level,
}

/// Battery from the control channel, topped up with the hub (BLE adverts keep
/// the case level when the buds are out of it).
pub fn levels(mac: &str, battery: &[BatteryInfo]) -> Levels {
    let get = |c: BatteryComponent| -> Level {
        battery
            .iter()
            .find(|b| b.component == c && b.status != BatteryStatus::Disconnected)
            .map(|b| (b.level, b.status == BatteryStatus::Charging))
    };
    let mut l = Levels {
        left: get(BatteryComponent::Left),
        right: get(BatteryComponent::Right),
        case: get(BatteryComponent::Case),
        headphone: get(BatteryComponent::Headphone),
    };
    let hub = crate::hub::hub().snapshot();
    if hub.mac.as_deref() == Some(mac) {
        let conv = |b: Option<crate::hub::Battery>| b.map(|b| (b.level, b.charging));
        l.left = l.left.or(conv(hub.left));
        l.right = l.right.or(conv(hub.right));
        l.case = l.case.or(conv(hub.case));
        l.headphone = l.headphone.or(conv(hub.headphone));
    }
    l
}

fn model_of(mac: &str) -> Option<u16> {
    let hub = crate::hub::hub().snapshot();
    if hub.mac.as_deref() == Some(mac) || hub.mac.is_none() {
        hub.model_id
    } else {
        None
    }
}

fn is_max(model: Option<u16>) -> bool {
    matches!(model, Some(0x200A | 0x201F | 0x202D))
}

/// Top card: product picture, status, battery rings, Connect/Disconnect.
pub fn hero<'a>(
    theme: &Theme,
    mac: &str,
    connected: bool,
    lv: Levels,
    model: Option<u16>,
) -> Element<'a, Message> {
    let art = style::art(model);
    let hub = crate::hub::hub().snapshot();
    let ours = hub.mac.as_deref() == Some(mac);

    let (dot, status): (fn(&Theme) -> Color, &str) = if connected {
        (style::connected, "Connected")
    } else if ours && hub.other_host {
        (style::tertiary, "Connected to another device")
    } else if ours && hub.nearby {
        (style::secondary, "Nearby")
    } else {
        (style::tertiary, "Not connected")
    };
    let status_line = row![
        container(Space::new())
            .width(Length::Fixed(7.0))
            .height(Length::Fixed(7.0))
            .style(move |t: &Theme| container::Style {
                background: Some(Background::Color(dot(t))),
                border: Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        text(status).font(style::MEDIUM).size(13).style(move |t: &Theme| text::Style {
            color: Some(if connected { style::connected(t) } else { style::secondary(t) }),
        }),
    ]
    .spacing(7)
    .align_y(Center);

    // Like iOS: only parts that report a level get a ring.
    let mut gauges = row![].spacing(22);
    if is_max(model) {
        if lv.headphone.is_some() {
            gauges = gauges.push(style::battery_gauge(theme, "Battery", None, lv.headphone, 64.0));
        }
    } else {
        for (label, part, level) in [
            ("Left", art.left.clone(), lv.left),
            ("Right", art.right.clone(), lv.right),
            ("Case", art.case.clone(), lv.case),
        ] {
            if level.is_some() {
                gauges = gauges.push(style::battery_gauge(theme, label, part, level, 62.0));
            }
        }
    }
    let has_gauges = lv.headphone.is_some() || lv.left.is_some() || lv.right.is_some() || lv.case.is_some();

    let button = if connected {
        style::pill("Disconnect", Pill::Secondary, Some(Message::ConnectDevice(mac.to_string(), false)))
    } else {
        style::pill("Connect", Pill::Primary, Some(Message::ConnectDevice(mac.to_string(), true)))
    };

    let mut info = column![status_line].spacing(3).width(Length::Fill);
    if let Some(id) = model {
        info = info.push(
            text(crate::hub::model_name(id))
                .size(12.5)
                .style(style::text_color(style::tertiary)),
        );
    }
    info = info.push(Space::new().height(Length::Fixed(14.0)));
    if has_gauges {
        info = info
            .push(gauges)
            .push(Space::new().height(Length::Fixed(14.0)));
    }
    let info = info.push(button);

    container(
        row![
            container(
                image(art.hero.clone())
                    .height(Length::Fixed(168.0))
                    .filter_method(image::FilterMethod::Linear)
            )
            .center_x(Length::Fixed(190.0)),
            info,
        ]
        .spacing(18)
        .align_y(Center),
    )
    .padding(Padding {
        top: 20.0,
        bottom: 20.0,
        left: 14.0,
        right: 20.0,
    })
    .width(Length::Fill)
    .style(|t: &Theme| {
        let mut s = style::card_style(t);
        let top = style::mix(style::surface(t), style::accent(t), 0.07);
        s.background = Some(Background::Gradient(
            gradient::Linear::new(std::f32::consts::PI)
                .add_stop(0.0, top)
                .add_stop(1.0, style::surface(t))
                .into(),
        ));
        s.border.radius = 16.0.into();
        s
    })
    .into()
}

/// Page shown for a known pair of AirPods that isn't connected right now.
pub fn offline_view<'a>(theme: &Theme, mac: &str, name: String) -> Element<'a, Message> {
    let model = model_of(mac);
    let lv = levels(mac, &[]);
    let content = column![
        hero(theme, mac, false, lv, model),
        Space::new().height(Length::Fixed(22.0)),
        style::section(
            None,
            vec![style::list_row(
                None,
                "Connect your AirPods to change their settings",
                Some(
                    "Open the case nearby or put them in your ears. They reconnect on their own \
                     if this computer was the last one they used; otherwise press Connect."
                        .to_string()
                ),
                Space::new().into(),
            )],
            None,
        ),
    ];
    style::page(name, content.into())
}

fn mode_caption(mode: u8) -> &'static str {
    match mode {
        0x02 => "Noise Cancellation blocks out external sounds.",
        0x03 => "Transparency lets outside sound in, so you can hear what's going on around you.",
        0x04 => "Adaptive dynamically blends Transparency and Noise Cancellation to your environment.",
        _ => "Noise control is off. Loud sounds are not reduced.",
    }
}

#[allow(clippy::too_many_arguments)]
pub fn airpods_view<'a>(
    theme: &Theme,
    mac: &'a str,
    devices_list: &HashMap<String, DeviceData>,
    state: &'a AirPodsState,
    aacp: Arc<AACPManager>,
    pause_convo: bool,
) -> Element<'a, Message> {
    let mac_s = mac.to_string();
    let model = model_of(mac);
    let lv = levels(mac, &state.battery);

    // ── Noise control ────────────────────────────────────────────────────────
    let current = state.noise_control_mode.to_byte();
    // Same per-mode tints as the Waybar pill: ANC amber, Transparency blue, Adaptive gold.
    type Tint = fn(&Theme) -> Color;
    let mut modes: Vec<(u8, &'static str, &'static str, Tint)> = vec![
        (0x02, style::G_ANC, "Noise Cancellation", style::tint_accent),
        (0x03, style::G_TRANSPARENCY, "Transparency", style::tint_blue),
    ];
    if has_adaptive(model) {
        modes.push((0x04, style::G_ADAPTIVE, "Adaptive", style::tint_gold));
    }
    if state.allow_off_mode {
        modes.push((0x01, style::G_EARBUDS, "Off", style::tint_grey));
    }
    let mut tiles = row![].spacing(10);
    for (byte, glyph, label, tint) in modes {
        tiles = tiles.push(style::tile(
            glyph,
            label,
            current == byte,
            tint,
            Message::SendControl(mac_s.clone(), Id::ListeningMode, vec![byte]),
        ));
    }
    let listening = vec![style::block(tiles.into())];
    let mut noise = airpods_extra::noise_rows(mac, state, &aacp, model);

    // ── Audio ────────────────────────────────────────────────────────────────
    let pv = {
        let st = state.clone();
        let mac = mac_s.clone();
        let a = aacp.clone();
        style::list_row(
            None,
            "Personalized Volume",
            Some("Adjusts the volume of media in response to your environment.".into()),
            style::switch(state.personalized_volume_enabled)
                .on_toggle(move |on| {
                    airpods_extra::send(&a, Id::AdaptiveVolumeConfig, vec![if on { 1 } else { 2 }]);
                    let mut st = st.clone();
                    st.personalized_volume_enabled = on;
                    Message::StateChanged(mac.clone(), DeviceState::AirPods(st))
                })
                .into(),
        )
    };
    let ca = {
        let st = state.clone();
        let mac = mac_s.clone();
        let a = aacp.clone();
        let locked = pause_convo && aacp.mic_active();
        let t = style::switch(state.conversation_awareness_enabled);
        let t = if locked {
            t
        } else {
            t.on_toggle(move |on| {
                let a2 = a.clone();
                a.runtime().spawn(async move { a2.set_conversation_detection(on).await });
                let mut st = st.clone();
                st.conversation_awareness_enabled = on;
                Message::StateChanged(mac.clone(), DeviceState::AirPods(st))
            })
        };
        style::list_row(
            None,
            "Conversation Awareness",
            Some(if locked {
                "Paused while the hi-res microphone is capturing.".into()
            } else {
                "Lowers media volume and reduces background noise when you start speaking.".into()
            }),
            t.into(),
        )
    };
    let off = {
        let st = state.clone();
        let mac = mac_s.clone();
        let a = aacp.clone();
        style::list_row(
            None,
            "Off Listening Mode",
            Some("Adds Off to the listening modes. Loud sounds aren't reduced when Off.".into()),
            style::switch(state.allow_off_mode)
                .on_toggle(move |on| {
                    airpods_extra::send(&a, Id::AllowOffOption, vec![if on { 1 } else { 2 }]);
                    let mut st = st.clone();
                    st.allow_off_mode = on;
                    Message::StateChanged(mac.clone(), DeviceState::AirPods(st))
                })
                .into(),
        )
    };

    // ── General: name first ──────────────────────────────────────────────────
    let name_row = {
        let st = state.clone();
        let mac = mac_s.clone();
        style::list_row(
            None,
            "Name",
            None,
            text_input("AirPods", &state.device_name)
                .on_input(move |new_name| {
                    let mut st = st.clone();
                    st.device_name = new_name;
                    Message::StateChanged(mac.clone(), DeviceState::AirPods(st))
                })
                // Rename on Enter, not per keystroke (each packet renames the buds).
                .on_submit(Message::RenameDevice(mac_s.clone(), state.device_name.clone()))
                .align_x(iced::alignment::Horizontal::Right)
                .size(13.5)
                .padding(Padding {
                    top: 4.0,
                    bottom: 4.0,
                    left: 8.0,
                    right: 0.0,
                })
                .width(Length::Fixed(220.0))
                .style(style::field_style)
                .into(),
        )
    };
    let mut general = vec![name_row];
    general.extend(airpods_extra::general_rows(mac, state, &aacp, model));
    noise.push(off);

    // ── Hi-res microphone ────────────────────────────────────────────────────
    let mic = {
        let st = state.clone();
        let mac = mac_s.clone();
        let a = aacp.clone();
        let mut rows = vec![airpods_extra::mic_side_row(&mac, state, &aacp), style::list_row(
            None,
            "Hi-Res Microphone",
            Some(
                "Streams the AirPods' AAC-ELD microphone as an \u{201c}AirPodsHiRes\u{201d} input, \
                 so music keeps playing in high quality while you talk."
                    .into(),
            ),
            style::switch(state.hires_mic_enabled)
                .on_toggle(move |on| {
                    let a2 = a.clone();
                    a.runtime().spawn(async move { a2.set_hires_mic_enabled(on).await });
                    let mut st = st.clone();
                    st.hires_mic_enabled = on;
                    Message::StateChanged(mac.clone(), DeviceState::AirPods(st))
                })
                .into(),
        )];
        if aacp.mic_active() {
            rows.push(style::block(level_meter(
                aacp.mic_level().clamp(0.0, 1.0),
                aacp.mic_app(),
            )));
        }
        rows
    };

    // ── About ────────────────────────────────────────────────────────────────
    let mut about: Vec<Element<'a, Message>> = Vec::new();
    if let Some(DeviceInformation::AirPods(info)) =
        devices_list.get(mac).and_then(|d| d.information.clone())
    {
        let plain = |label: &'static str, v: String| -> Option<Element<'a, Message>> {
            (!v.is_empty()).then(|| style::list_row(None, label, None, style::value(v)))
        };
        let copyable = |label: &'static str, v: String| -> Option<Element<'a, Message>> {
            (!v.is_empty()).then(|| {
                style::list_row(
                    None,
                    label,
                    None,
                    style::plain_button(
                        row![
                            text(v.clone()).size(13.5),
                            text(style::G_COPY).font(style::GLYPHS).size(12),
                        ]
                        .spacing(6)
                        .align_y(Center)
                        .into(),
                        Message::CopyToClipboard(v),
                    ),
                )
            })
        };
        about.extend(
            [
                plain("Model Number", info.model_number.clone()),
                plain("Manufacturer", info.manufacturer.clone()),
                copyable("Serial Number", info.serial_number.clone()),
                copyable("Left Serial Number", info.left_serial_number.clone()),
                copyable("Right Serial Number", info.right_serial_number.clone()),
                plain("Firmware", info.version1.clone()),
                (info.version2 != info.version1)
                    .then(|| plain("Firmware (build)", info.version2.clone()))
                    .flatten(),
                plain("Hardware Revision", info.hardware_revision.clone()),
            ]
            .into_iter()
            .flatten(),
        );
    }
    about.push(style::list_row(None, "Bluetooth Address", None, style::value(mac_s.clone())));

    let selected_caption = mode_caption(current).to_string();
    let mut content = column![
        hero(theme, mac, true, lv, model),
        Space::new().height(Length::Fixed(24.0)),
        style::section(Some("Listening Mode"), listening, Some(selected_caption)),
        style::section(Some("Noise Control"), noise, None),
        style::section(Some("Audio"), vec![pv, ca], None),
    ];
    if !is_max(model) {
        content = content.push(style::section(
            Some("Press and Hold"),
            airpods_extra::press_rows(mac, state, &aacp, model),
            None,
        ));
    }
    content = content
        .push(style::section(
            Some("Call Controls"),
            airpods_extra::call_rows(mac, state, &aacp),
            None,
        ))
        .push(style::section(Some("General"), general, None))
        .push(style::section(
            Some("Microphone"),
            mic,
            Some("Capture starts only while an app records from the AirPodsHiRes input.".into()),
        ))
        .push(style::section(Some("About"), about, None));

    style::page(state.device_name.clone(), content.into())
}

fn level_meter<'a>(level: f32, app: Option<String>) -> Element<'a, Message> {
    let filled = ((level * 1000.0).round() as u16).max(1);
    let rest = 1000u16.saturating_sub(filled).max(1);
    let hot = level >= 0.9;
    let label = match app {
        Some(app) => format!("In use by {app}"),
        None => "Input level".to_string(),
    };
    let bar = container(
        row![
            container(Space::new())
                .width(Length::FillPortion(filled))
                .height(Length::Fill)
                .style(move |t: &Theme| container::Style {
                    background: Some(Background::Color(if hot {
                        t.palette().warning
                    } else {
                        t.palette().success
                    })),
                    border: Border {
                        radius: 3.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            container(Space::new()).width(Length::FillPortion(rest)),
        ]
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fixed(6.0))
    .style(|t: &Theme| container::Style {
        background: Some(Background::Color(t.palette().text.scale_alpha(0.1))),
        border: Border {
            radius: 3.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });
    column![
        text(label).size(12).style(style::text_color(style::secondary)),
        bar
    ]
    .spacing(6)
    .into()
}
