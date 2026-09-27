//! AirPods settings beyond the upstream page, laid out like iOS/macOS
//! Settings › AirPods. Encodings (opcode 0x09 control commands) were
//! cross-checked against the LibrePods Android app, capod and MagicPodsCore;
//! on/off switches are 0x01 = on, 0x02 = off.

use crate::bluetooth::aacp::{AACPManager, ControlCommandIdentifiers as Id};
use crate::devices::enums::{AirPodsState, DeviceState};
use crate::ui::style;
use crate::ui::window::Message;
use iced::widget::{Space, column, row, slider, text};
use iced::{Center, Element, Length};
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
pub fn pro2_or_later(model: Option<u16>) -> bool {
    matches!(model, None | Some(0x2014 | 0x2024 | 0x2027))
}

pub fn has_adaptive(model: Option<u16>) -> bool {
    pro2_or_later(model) || model == Some(0x201B)
}

fn has_sleep_detection(model: Option<u16>) -> bool {
    pro2_or_later(model) || matches!(model, Some(0x2019 | 0x201B))
}

/// Send a control command without blocking the UI; errors are logged, never fatal.
pub fn send(aacp: &Arc<AACPManager>, id: Id, value: Vec<u8>) {
    let a = aacp.clone();
    aacp.runtime().spawn(async move {
        if let Err(e) = a.send_control_command(id, &value).await {
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

fn switch_row<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp: &Arc<AACPManager>,
    id: Id,
    title: &'static str,
    subtitle: Option<&'static str>,
) -> Element<'a, Message> {
    let mac = mac.to_string();
    let aacp = aacp.clone();
    let st = state.clone();
    style::list_row(
        None,
        title,
        subtitle.map(str::to_string),
        style::switch(is_on(state, id))
            .on_toggle(move |on| {
                let v = vec![if on { 0x01 } else { 0x02 }];
                send(&aacp, id, v.clone());
                changed(&mac, &st, id, v)
            })
            .into(),
    )
}

#[allow(clippy::too_many_arguments)]
fn picker_row<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp: &Arc<AACPManager>,
    id: Id,
    title: &'static str,
    subtitle: Option<&'static str>,
    options: Vec<Choice>,
    default: usize,
) -> Element<'a, Message> {
    let selected = value_of(state, id)
        .and_then(|v| options.iter().find(|c| c.value == v).cloned())
        .or_else(|| options.get(default).cloned());
    let mac = mac.to_string();
    let aacp = aacp.clone();
    let st = state.clone();
    style::list_row(
        None,
        title,
        subtitle.map(str::to_string),
        style::picker(options, selected, move |c: Choice| {
            send(&aacp, id, c.value.clone());
            changed(&mac, &st, id, c.value)
        }),
    )
}

/// Rows for the Noise Control section, below the mode tiles.
pub fn noise_rows<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp: &Arc<AACPManager>,
    model: Option<u16>,
) -> Vec<Element<'a, Message>> {
    let mut rows = Vec::new();
    if has_adaptive(model) {
        // Wire value is inverted: 0 = strongest noise reduction.
        let ui = value_of(state, Id::AutoAncStrength)
            .and_then(|v| v.first().copied())
            .map(|w| 100u8.saturating_sub(w.min(100)))
            .unwrap_or(50);
        let mac_s = mac.to_string();
        let aacp_s = aacp.clone();
        let st = state.clone();
        rows.push(style::block(
            column![
                row![
                    text("Adaptive Audio").size(14).width(Length::Fill),
                    text(format!("{ui}")).size(12).style(style::text_color(style::tertiary)),
                ]
                .align_y(Center),
                text("How much of your surroundings Adaptive lets through.")
                    .size(12)
                    .style(style::text_color(style::secondary)),
                Space::new().height(Length::Fixed(6.0)),
                row![
                    text("Less noise").size(11).style(style::text_color(style::tertiary)),
                    slider(0u8..=100u8, ui, move |v: u8| {
                        let wire = vec![100 - v];
                        send(&aacp_s, Id::AutoAncStrength, wire.clone());
                        changed(&mac_s, &st, Id::AutoAncStrength, wire)
                    })
                    .step(5u8)
                    .style(style::slider_style)
                    .width(Length::Fill),
                    text("More noise").size(11).style(style::text_color(style::tertiary)),
                ]
                .spacing(12)
                .align_y(Center),
            ]
            .spacing(2)
            .into(),
        ));
    }
    rows.push(switch_row(
        mac,
        state,
        aacp,
        Id::OneBudAncMode,
        "Noise Cancellation with One AirPod",
        Some("Keep noise cancellation on when only one AirPod is in your ear."),
    ));
    rows
}

/// Which bud's microphone is used.
pub fn mic_side_row<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp: &Arc<AACPManager>,
) -> Element<'a, Message> {
    picker_row(
        mac,
        state,
        aacp,
        Id::MicMode,
        "Microphone",
        None,
        choices(&[
            ("Automatic", &[0x00]),
            ("Always Right AirPod", &[0x01]),
            ("Always Left AirPod", &[0x02]),
        ]),
        0,
    )
}

/// Press-and-hold / stem gestures.
pub fn press_rows<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp: &Arc<AACPManager>,
    model: Option<u16>,
) -> Vec<Element<'a, Message>> {
    let mut rows = Vec::new();
    if has_adaptive(model) {
        // Which listening modes a long press cycles through (0x1A bitmask).
        let mask = value_of(state, Id::ListeningModeConfigs)
            .and_then(|v| v.first().copied())
            .unwrap_or(0x0E);
        let mut chips = row![].spacing(8).align_y(Center);
        for (bit, label) in [
            (0x02u8, "Noise Cancellation"),
            (0x04, "Transparency"),
            (0x08, "Adaptive"),
            (0x01, "Off"),
        ] {
            let on = mask & bit != 0;
            let new = if on { mask & !bit } else { mask | bit };
            // iOS requires at least two modes in the cycle.
            let msg = if new.count_ones() < 2 {
                Message::StateChanged(mac.to_string(), DeviceState::AirPods(state.clone()))
            } else {
                Message::SendControl(mac.to_string(), Id::ListeningModeConfigs, vec![new])
            };
            chips = chips.push(style::chip(label, on, msg));
        }
        rows.push(style::block(
            column![
                text("Press and Hold Cycles Between").size(14),
                text("Listening modes a long press on the stem switches through.")
                    .size(12)
                    .style(style::text_color(style::secondary)),
                Space::new().height(Length::Fixed(8.0)),
                chips,
            ]
            .spacing(2)
            .into(),
        ));
    }
    rows.push(picker_row(
        mac,
        state,
        aacp,
        Id::DoubleClickInterval,
        "Press Speed",
        Some("How quickly you must press for a double or triple press."),
        choices(&[("Default", &[0x00]), ("Slower", &[0x01]), ("Slowest", &[0x02])]),
        0,
    ));
    rows.push(picker_row(
        mac,
        state,
        aacp,
        Id::ClickHoldInterval,
        "Press and Hold Duration",
        None,
        choices(&[("Default", &[0x00]), ("Shorter", &[0x01]), ("Shortest", &[0x02])]),
        0,
    ));
    if pro2_or_later(model) {
        rows.push(switch_row(
            mac,
            state,
            aacp,
            Id::VolumeSwipeMode,
            "Volume Control",
            Some("Swipe up or down on the stem to change the volume."),
        ));
        rows.push(picker_row(
            mac,
            state,
            aacp,
            Id::VolumeSwipeInterval,
            "Volume Swipe Length",
            None,
            choices(&[("Default", &[0x00]), ("Longer", &[0x01]), ("Longest", &[0x02])]),
            0,
        ));
    }
    rows
}

pub fn call_rows<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp: &Arc<AACPManager>,
) -> Vec<Element<'a, Message>> {
    vec![picker_row(
        mac,
        state,
        aacp,
        Id::CallManagementConfig,
        "Mute & End Call",
        None,
        choices(&[
            ("Press once to mute", &[0x00, 0x03]),
            ("Press twice to mute", &[0x00, 0x02]),
        ]),
        0,
    )]
}

pub fn general_rows<'a>(
    mac: &str,
    state: &'a AirPodsState,
    aacp: &Arc<AACPManager>,
    model: Option<u16>,
) -> Vec<Element<'a, Message>> {
    let mut rows = vec![switch_row(
        mac,
        state,
        aacp,
        Id::EarDetectionConfig,
        "Automatic Ear Detection",
        Some("Play audio through the AirPods only while they are in your ears."),
    )];
    if has_sleep_detection(model) {
        rows.push(switch_row(
            mac,
            state,
            aacp,
            Id::SleepDetectionConfig,
            "Pause Media When Falling Asleep",
            None,
        ));
    }
    rows
}
