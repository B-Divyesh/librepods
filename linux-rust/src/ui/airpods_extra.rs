//! AirPods settings the upstream Linux page lacks, matching the iOS/Android
//! "AirPods" settings screen. Encodings (opcode 0x09 control commands) were
//! cross-checked against the LibrePods Android app, capod and MagicPodsCore;
//! on/off switches are 0x01 = on, 0x02 = off.

use crate::bluetooth::aacp::{AACPManager, ControlCommandIdentifiers as Id};
use crate::devices::enums::{AirPodsState, DeviceState};
use crate::ui::window::Message;
use iced::widget::{Space, column, container, pick_list, row, slider, text, toggler};
use iced::{Background, Border, Center, Element, Length, Padding, Theme};
use log::error;
use std::fmt;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
struct Choice {
    label: &'static str,
    value: Vec<u8>,
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label)
    }
}

fn choices(list: &[(&'static str, &[u8])]) -> Vec<Choice> {
    list.iter()
        .map(|(label, value)| Choice {
            label,
            value: value.to_vec(),
        })
        .collect()
}

/// Pro 2 (Lightning / USB-C) and Pro 3.
fn pro2_or_later(model: Option<u16>) -> bool {
    matches!(model, None | Some(0x2014 | 0x2024 | 0x2027))
}

fn has_adaptive(model: Option<u16>) -> bool {
    pro2_or_later(model) || model == Some(0x201B)
}

fn has_sleep_detection(model: Option<u16>) -> bool {
    pro2_or_later(model) || matches!(model, Some(0x2019 | 0x201B))
}

fn card<'a>(
    title: &'a str,
    subtitle: Option<&'a str>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let mut labels = column![text(title).size(16)].width(Length::Fill);
    if let Some(sub) = subtitle {
        labels = labels.push(text(sub).size(12).style(|theme: &Theme| {
            let mut style = text::Style::default();
            style.color = Some(theme.palette().text.scale_alpha(0.7));
            style
        }));
    }
    container(row![labels, control].align_y(Center).spacing(12))
        .padding(Padding {
            top: 8.0,
            bottom: 8.0,
            left: 18.0,
            right: 18.0,
        })
        .style(|theme: &Theme| {
            let mut style = container::Style::default();
            style.background = Some(Background::Color(theme.palette().primary.scale_alpha(0.1)));
            let mut border = Border::default();
            border.color = theme.palette().primary.scale_alpha(0.5);
            style.border = border.rounded(16);
            style
        })
        .into()
}

fn heading<'a>(title: &'a str) -> Element<'a, Message> {
    container(text(title).size(18).style(|theme: &Theme| {
        let mut style = text::Style::default();
        style.color = Some(theme.palette().primary);
        style
    }))
    .padding(Padding {
        top: 0.0,
        bottom: 0.0,
        left: 18.0,
        right: 18.0,
    })
    .into()
}

/// Send a control command without blocking the UI; errors are logged, never fatal.
fn send(aacp: &Arc<AACPManager>, id: Id, value: Vec<u8>) {
    let aacp = aacp.clone();
    aacp.clone().runtime().spawn(async move {
        if let Err(e) = aacp.send_control_command(id, &value).await {
            error!("Failed to send {id}: {e}");
        }
    });
}

fn changed(mac: &str, state: &AirPodsState, id: Id, value: Vec<u8>) -> Message {
    let mut st = state.clone();
    st.controls.insert(id as u8, value);
    Message::StateChanged(mac.to_string(), DeviceState::AirPods(st))
}

fn value_of(state: &AirPodsState, id: Id) -> Option<Vec<u8>> {
    state.controls.get(&(id as u8)).cloned()
}

fn is_on(state: &AirPodsState, id: Id) -> bool {
    matches!(value_of(state, id).as_deref(), Some([0x01, ..]))
}

fn switch<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp: &Arc<AACPManager>,
    id: Id,
    title: &'a str,
    subtitle: Option<&'a str>,
) -> Element<'a, Message> {
    let mac = mac.to_string();
    let aacp = aacp.clone();
    let st = state.clone();
    card(
        title,
        subtitle,
        toggler(is_on(state, id))
            .on_toggle(move |on| {
                let v = vec![if on { 0x01 } else { 0x02 }];
                send(&aacp, id, v.clone());
                changed(&mac, &st, id, v)
            })
            .size(20)
            .into(),
    )
}

fn picker<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp: &Arc<AACPManager>,
    id: Id,
    title: &'a str,
    subtitle: Option<&'a str>,
    options: Vec<Choice>,
    default: usize,
) -> Element<'a, Message> {
    let current = value_of(state, id);
    let selected = current
        .and_then(|v| options.iter().find(|c| c.value == v).cloned())
        .or_else(|| options.get(default).cloned());
    let mac = mac.to_string();
    let aacp = aacp.clone();
    let st = state.clone();
    card(
        title,
        subtitle,
        pick_list(options, selected, move |c: Choice| {
            send(&aacp, id, c.value.clone());
            changed(&mac, &st, id, c.value)
        })
        .width(Length::Fixed(190.0))
        .into(),
    )
}

pub fn extra_settings<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp: Arc<AACPManager>,
) -> Element<'a, Message> {
    let model = crate::hub::hub().snapshot().model_id;
    let mut col = column![].spacing(10);

    // ── Press and hold / gestures ──────────────────────────────────────────
    col = col.push(heading("Press and Hold"));
    if has_adaptive(model) {
        // Which listening modes a long press cycles through (0x1A bitmask).
        let mask = value_of(state, Id::ListeningModeConfigs)
            .and_then(|v| v.first().copied())
            .unwrap_or(0x0E);
        let mut modes = row![].spacing(14).align_y(Center);
        for (bit, label) in [
            (0x02u8, "Noise Cancellation"),
            (0x04, "Transparency"),
            (0x08, "Adaptive"),
            (0x01, "Off"),
        ] {
            let mac = mac.to_string();
            let aacp = aacp.clone();
            let st = state.clone();
            modes = modes.push(
                column![
                    toggler(mask & bit != 0)
                        .on_toggle(move |on| {
                            let new = if on { mask | bit } else { mask & !bit };
                            // iOS requires at least two modes in the cycle.
                            if new.count_ones() < 2 {
                                return Message::StateChanged(
                                    mac.clone(),
                                    DeviceState::AirPods(st.clone()),
                                );
                            }
                            if bit == 0x01 && on {
                                send(&aacp, Id::AllowOffOption, vec![0x01]);
                            }
                            send(&aacp, Id::ListeningModeConfigs, vec![new]);
                            changed(&mac, &st, Id::ListeningModeConfigs, vec![new])
                        })
                        .size(18),
                    text(label).size(11),
                ]
                .align_x(Center)
                .spacing(4),
            );
        }
        col = col.push(card(
            "Cycles Between",
            Some("Listening modes a long press on the stem switches through (at least two)."),
            modes.into(),
        ));
    }
    col = col.push(picker(
        mac,
        state,
        &aacp,
        Id::DoubleClickInterval,
        "Press Speed",
        Some("How quickly you must press to register a double or triple press."),
        choices(&[("Default", &[0x00]), ("Slower", &[0x01]), ("Slowest", &[0x02])]),
        0,
    ));
    col = col.push(picker(
        mac,
        state,
        &aacp,
        Id::ClickHoldInterval,
        "Press and Hold Duration",
        Some("How long you hold the stem for a long press."),
        choices(&[("Default", &[0x00]), ("Shorter", &[0x01]), ("Shortest", &[0x02])]),
        0,
    ));
    if pro2_or_later(model) {
        col = col.push(switch(
            mac,
            state,
            &aacp,
            Id::VolumeSwipeMode,
            "Volume Control",
            Some("Swipe up or down on the stem to change the volume."),
        ));
        col = col.push(picker(
            mac,
            state,
            &aacp,
            Id::VolumeSwipeInterval,
            "Volume Swipe Length",
            None,
            choices(&[("Default", &[0x00]), ("Longer", &[0x01]), ("Longest", &[0x02])]),
            0,
        ));
    }

    // ── Calls ──────────────────────────────────────────────────────────────
    col = col.push(Space::new().height(Length::from(10)));
    col = col.push(heading("Call Controls"));
    col = col.push(picker(
        mac,
        state,
        &aacp,
        Id::CallManagementConfig,
        "Mute & End Call",
        None,
        choices(&[
            ("Press once to mute, twice to end", &[0x00, 0x03]),
            ("Press twice to mute, once to end", &[0x00, 0x02]),
        ]),
        0,
    ));

    // ── Audio ──────────────────────────────────────────────────────────────
    col = col.push(Space::new().height(Length::from(10)));
    col = col.push(heading("Noise Control"));
    if has_adaptive(model) {
        // Wire value is inverted: 0 = strongest noise reduction.
        let ui = value_of(state, Id::AutoAncStrength)
            .and_then(|v| v.first().copied())
            .map(|w| 100u8.saturating_sub(w.min(100)))
            .unwrap_or(50);
        let mac_s = mac.to_string();
        let aacp_s = aacp.clone();
        let st = state.clone();
        col = col.push(card(
            "Adaptive Audio",
            Some("Left: less noise · Right: more of your surroundings"),
            row![
                text("Less").size(11),
                slider(0u8..=100u8, ui, move |v: u8| {
                    let wire = vec![100 - v];
                    send(&aacp_s, Id::AutoAncStrength, wire.clone());
                    changed(&mac_s, &st, Id::AutoAncStrength, wire)
                })
                .step(5u8)
                .width(Length::Fixed(160.0)),
                text("More").size(11),
            ]
            .spacing(8)
            .align_y(Center)
            .into(),
        ));
    }
    col = col.push(switch(
        mac,
        state,
        &aacp,
        Id::OneBudAncMode,
        "Noise Cancellation with One AirPod",
        Some("Keep noise cancellation on when only one AirPod is in your ear."),
    ));

    // ── General ────────────────────────────────────────────────────────────
    col = col.push(Space::new().height(Length::from(10)));
    col = col.push(heading("General"));
    col = col.push(switch(
        mac,
        state,
        &aacp,
        Id::EarDetectionConfig,
        "Automatic Ear Detection",
        Some("Route audio to the AirPods only while they are in your ears."),
    ));
    if has_sleep_detection(model) {
        col = col.push(switch(
            mac,
            state,
            &aacp,
            Id::SleepDetectionConfig,
            "Pause Media When Falling Asleep",
            None,
        ));
    }
    col = col.push(picker(
        mac,
        state,
        &aacp,
        Id::MicMode,
        "Microphone",
        None,
        choices(&[
            ("Automatic", &[0x00]),
            ("Always Right AirPod", &[0x01]),
            ("Always Left AirPod", &[0x02]),
        ]),
        0,
    ));

    col.into()
}
