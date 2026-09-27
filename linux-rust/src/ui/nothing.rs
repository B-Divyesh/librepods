use crate::bluetooth::att::{ATTHandles, ATTManager};
use crate::devices::enums::{DeviceData, DeviceInformation, DeviceState, NothingState};
use crate::ui::style;
use crate::ui::window::Message;
use iced::widget::{Space, column, combo_box, text_input};
use iced::{Background, Border, Element, Length, Padding, Theme};
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use tokio::runtime::Runtime;

pub fn nothing_view<'a>(
    mac: &'a str,
    devices_list: &HashMap<String, DeviceData>,
    state: &'a NothingState,
    att_manager: Arc<ATTManager>,
) -> Element<'a, Message> {
    let mac = mac.to_string();
    let name = devices_list
        .get(mac.as_str())
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "Nothing".to_string());

    let mode_picker = {
        let state_clone = state.clone();
        let mac = mac.clone();
        let att = att_manager.clone();
        combo_box(
            &state.anc_mode_state,
            "Noise control",
            Some(&state.anc_mode),
            move |selected_mode| {
                let att = att.clone();
                let byte = selected_mode.to_byte();
                let mac_s = mac.clone();
                run_async_in_thread(async move {
                    if let Err(e) = att
                        .write(
                            ATTHandles::NothingEverything,
                            &[
                                0x55, 0x60, 0x01, 0x0F, 0xF0, 0x03, 0x00, 0x00, 0x01, byte, 0x00,
                                0x00, 0x00,
                            ],
                        )
                        .await
                    {
                        log::error!("Failed to set noise control for {}: {}", mac_s, e);
                    }
                });
                let mut state = state_clone.clone();
                state.anc_mode = selected_mode;
                Message::StateChanged(mac.to_string(), DeviceState::Nothing(state))
            },
        )
        .width(Length::Fixed(230.0))
        .size(13.0)
        .padding(Padding {
            top: 5.0,
            bottom: 5.0,
            left: 10.0,
            right: 10.0,
        })
        .input_style(|t: &Theme, _status| text_input::Style {
            background: Background::Color(style::control(t)),
            border: Border {
                width: 0.0,
                color: iced::Color::TRANSPARENT,
                radius: 8.0.into(),
            },
            icon: style::secondary(t),
            placeholder: style::tertiary(t),
            value: t.palette().text,
            selection: style::accent(t).scale_alpha(0.4),
        })
        .menu_style(style::menu_style)
    };

    let mut about: Vec<Element<'a, Message>> = Vec::new();
    if let Some(DeviceInformation::Nothing(info)) =
        devices_list.get(mac.as_str()).and_then(|d| d.information.clone())
    {
        about.push(style::list_row(None, "Serial Number", None, style::value(info.serial_number)));
        about.push(style::list_row(None, "Firmware", None, style::value(info.firmware_version)));
    }
    about.push(style::list_row(None, "Bluetooth Address", None, style::value(mac.clone())));

    let content = column![
        style::section(
            Some("Noise Control"),
            vec![style::list_row(None, "Mode", None, mode_picker.into())],
            None,
        ),
        style::section(Some("About"), about, None),
        Space::new().height(Length::Fixed(0.0)),
    ];
    style::page(name, content.into())
}

fn run_async_in_thread<F>(fut: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(fut);
    });
}
