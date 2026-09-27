use crate::bluetooth::aacp::{AACPEvent, AACPManager, ControlCommandIdentifiers};
use crate::bluetooth::managers::DeviceManagers;
use crate::devices::enums::{
    AirPodsNoiseControlMode, AirPodsState, DeviceData, DeviceState, DeviceType, NothingAncMode,
    NothingState,
};
use crate::ui::airpods::{airpods_view, levels, offline_view};
use crate::ui::messages::BluetoothUIMessage;
use crate::ui::nothing::nothing_view;
use crate::ui::style::{self, Pill};
use crate::utils::{AppSettings, MyTheme, PreferredCodec, get_devices_path};
use bluer::Address;
use iced::widget::{
    Space, button, column, combo_box, container, image, row, text, text_input,
};
use iced::{
    Background, Border, Center, Element, Length, Padding, Settings, Size, Subscription, Task,
    Theme, daemon, window,
};
use log::{debug, error};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{Mutex, RwLock};

pub fn start_ui(
    ui_rx: UnboundedReceiver<BluetoothUIMessage>,
    start_minimized: bool,
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
) -> iced::Result {
    let ui_rx = Arc::new(Mutex::new(ui_rx));

    daemon(
        move || {
            App::new(
                Arc::clone(&ui_rx),
                start_minimized,
                Arc::clone(&device_managers),
            )
        },
        App::update,
        App::view,
    )
    .subscription(App::subscription)
    .theme(App::theme)
    .title(App::title)
    .settings(Settings {
        id: Some("librepods".to_string()),
        // Inter comes from the system (the desktop's UI font); SF Pro stays
        // bundled as a fallback for machines without it.
        fonts: vec![include_bytes!("../../assets/font/sf_pro.otf").as_slice().into()],
        default_font: style::UI,
        antialiasing: true,
        ..Settings::default()
    })
    .run()
}

fn main_window_settings() -> window::Settings {
    let mut settings = window::Settings::default();
    settings.platform_specific.application_id = "me.kavishdevar.librepods".to_string();
    settings.size = Size::new(900.0, 660.0);
    settings.min_size = Some(Size::new(640.0, 480.0));
    settings.icon = window::icon::from_file_data(include_bytes!("../../assets/icon.png"), None).ok();
    settings
}

pub struct App {
    window: Option<window::Id>,
    selected_tab: Tab,
    selected_theme: MyTheme,
    ui_rx: Arc<Mutex<UnboundedReceiver<BluetoothUIMessage>>>,
    bluetooth_state: BluetoothState,
    paired_devices: HashMap<String, Address>,
    device_states: HashMap<String, DeviceState>,
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    pending_add_device: Option<(String, Address)>,
    device_type_state: combo_box::State<DeviceType>,
    selected_device_type: Option<DeviceType>,
    tray_text_mode: bool,
    stem_control: bool,
    preferred_codec: PreferredCodec,
    hires_mic_enabled: bool,
    hires_mic_agc: bool,
    hires_mic_pause_convo: bool,
    a2dp_reset: bool,
    macfeel: AppSettings,
    /// LIBREPODS_UI_DEMO: a fake pair of AirPods so the UI can be reviewed
    /// without hardware (device | settings | empty).
    demo: Option<Demo>,
}

pub struct BluetoothState {
    connected_devices: Vec<String>,
}

impl BluetoothState {
    pub fn new() -> Self {
        Self {
            connected_devices: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    SelectTab(Tab),
    ThemeSelected(MyTheme),
    CopyToClipboard(String),
    BluetoothMessage(BluetoothUIMessage),
    GotPairedDevices(HashMap<String, Address>),
    StartAddDevice(String, Address),
    SelectDeviceType(DeviceType),
    ConfirmAddDevice,
    CancelAddDevice,
    StateChanged(String, DeviceState),
    TrayTextModeChanged(bool),
    StemControlChanged(bool),
    PreferredCodecChanged(PreferredCodec),
    A2dpResetChanged(bool),
    HiResMicAgcChanged(bool),
    HiResMicPauseConvoChanged(bool),
    MicLevelTick,
    RenameDevice(String, String),
    MacFeel(MacFeelSetting),
    /// Send a control command and mirror it in the UI state right away.
    SendControl(String, ControlCommandIdentifiers, Vec<u8>),
    /// Connect (true) / disconnect (false) a device.
    ConnectDevice(String, bool),
    /// "Connect" from the empty state: whichever AirPods are paired.
    ConnectAny,
    /// Something in the hub (battery, lid, Bluetooth power…) changed.
    HubChanged,
}

/// Toggles in the "Mac experience" settings section.
#[derive(Debug, Clone, Copy)]
pub enum MacFeelSetting {
    CasePopup(bool),
    ConnectPopup(bool),
    AutoConnectOnOpen(bool),
    LowBatteryAlerts(bool),
    ModeToasts(bool),
    MediaTakeover(bool),
    EarPause(bool),
    SwitchOutput(bool),
    HideTrayIcon(bool),
    Bluetooth(bool),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Device(String),
    Settings,
    AddDevice,
}

struct Demo {
    mac: String,
    data: DeviceData,
    aacp: Arc<AACPManager>,
}

const DEMO_MAC: &str = "AA:BB:CC:DD:EE:FF";

fn demo_setup() -> Option<(Demo, AirPodsState, Tab)> {
    use crate::bluetooth::aacp::{AirPodsLEKeys, BatteryComponent, BatteryInfo, BatteryStatus};
    use crate::devices::airpods::AirPodsInformation;
    use crate::devices::enums::DeviceInformation;
    let which = std::env::var("LIBREPODS_UI_DEMO").ok()?;
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("demo runtime")
    });
    let aacp = {
        let _g = rt.enter();
        Arc::new(AACPManager::new())
    };
    let info = AirPodsInformation {
        name: "AirPods Pro".into(),
        model_number: "A3047".into(),
        manufacturer: "Apple Inc.".into(),
        serial_number: "H9XK2L7P3Q".into(),
        version1: "7E101".into(),
        version2: "7E101".into(),
        hardware_revision: "1.0.0".into(),
        updater_identifier: String::new(),
        left_serial_number: "H9XK2L7P3L".into(),
        right_serial_number: "H9XK2L7P3R".into(),
        version3: String::new(),
        le_keys: AirPodsLEKeys {
            irk: String::new(),
            enc_key: String::new(),
        },
    };
    let state = AirPodsState {
        device_name: "AirPods Pro".into(),
        noise_control_mode: AirPodsNoiseControlMode::NoiseCancellation,
        noise_control_state: combo_box::State::new(vec![]),
        conversation_awareness_enabled: true,
        personalized_volume_enabled: false,
        allow_off_mode: false,
        hires_mic_enabled: true,
        battery: vec![
            BatteryInfo { component: BatteryComponent::Left, level: 82, status: BatteryStatus::NotCharging },
            BatteryInfo { component: BatteryComponent::Right, level: 18, status: BatteryStatus::NotCharging },
            BatteryInfo { component: BatteryComponent::Case, level: 64, status: BatteryStatus::Charging },
        ],
        controls: HashMap::from([(0x1A, vec![0x0E]), (0x2E, vec![40]), (0x0A, vec![0x01])]),
    };
    let tab = match which.as_str() {
        "settings" => Tab::Settings,
        "empty" => Tab::Device("none".into()),
        _ => Tab::Device(DEMO_MAC.into()),
    };
    let data = DeviceData {
        name: "AirPods Pro".into(),
        type_: DeviceType::AirPods,
        information: Some(DeviceInformation::AirPods(info)),
    };
    Some((
        Demo {
            mac: DEMO_MAC.into(),
            data,
            aacp,
        },
        state,
        tab,
    ))
}

fn read_devices() -> HashMap<String, DeviceData> {
    std::fs::read_to_string(get_devices_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

impl App {
    pub fn new(
        ui_rx: Arc<Mutex<UnboundedReceiver<BluetoothUIMessage>>>,
        start_minimized: bool,
        device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    ) -> (Self, Task<Message>) {
        let wait_task = Task::perform(wait_for_message(Arc::clone(&ui_rx)), |msg| msg);

        let (window, open_task) = if start_minimized {
            (None, Task::none())
        } else {
            let (id, open) = window::open(main_window_settings());
            (Some(id), open.map(Message::WindowOpened))
        };

        let app_settings = AppSettings::load();
        let mut device_states = HashMap::new();
        let mut bluetooth_state = BluetoothState::new();

        // Open on the first AirPods instead of an empty "Select a device".
        let mut selected_tab = read_devices()
            .into_iter()
            .filter(|(_, d)| d.type_ == DeviceType::AirPods)
            .map(|(mac, _)| mac)
            .min()
            .map(Tab::Device)
            .unwrap_or_else(|| Tab::Device("none".to_string()));

        let demo = demo_setup().map(|(demo, state, tab)| {
            // "offline" lists the demo AirPods without connecting them.
            if std::env::var("LIBREPODS_UI_DEMO").as_deref() != Ok("offline") {
                device_states.insert(demo.mac.clone(), DeviceState::AirPods(state));
                bluetooth_state.connected_devices.push(demo.mac.clone());
            }
            selected_tab = tab;
            demo
        });

        (
            Self {
                window,
                selected_tab,
                selected_theme: app_settings.theme,
                ui_rx,
                bluetooth_state,
                paired_devices: HashMap::new(),
                device_states,
                pending_add_device: None,
                device_type_state: combo_box::State::new(vec![DeviceType::Nothing]),
                selected_device_type: None,
                device_managers,
                tray_text_mode: app_settings.tray_text_mode,
                stem_control: app_settings.stem_control,
                preferred_codec: app_settings.preferred_codec,
                hires_mic_enabled: app_settings.hires_mic_enabled,
                hires_mic_agc: app_settings.hires_mic_agc,
                hires_mic_pause_convo: app_settings.hires_mic_pause_convo,
                a2dp_reset: app_settings.a2dp_reset,
                macfeel: app_settings.clone(),
                demo,
            },
            Task::batch(vec![open_task, wait_task]),
        )
    }

    /// devices.json plus the demo device when LIBREPODS_UI_DEMO is set.
    fn devices_list(&self) -> HashMap<String, DeviceData> {
        // Demo mode shows only the demo device (never mixed with real ones);
        // it is only active when LIBREPODS_UI_DEMO is set in the environment.
        if let Some(d) = &self.demo {
            if std::env::var("LIBREPODS_UI_DEMO").as_deref() == Ok("empty") {
                return HashMap::new();
            }
            return HashMap::from([(d.mac.clone(), d.data.clone())]);
        }
        read_devices()
    }

    /// The AirPods page to land on: connected ones first, else the first known.
    fn preferred_airpods(&self) -> Option<String> {
        let list = self.devices_list();
        let airpods = |mac: &String| {
            list.get(mac).is_some_and(|d| d.type_ == DeviceType::AirPods)
        };
        self.bluetooth_state
            .connected_devices
            .iter()
            .find(|m| airpods(m))
            .cloned()
            .or_else(|| {
                list.iter()
                    .filter(|(_, d)| d.type_ == DeviceType::AirPods)
                    .map(|(m, _)| m.clone())
                    .min()
            })
    }

    fn aacp_for(&self, mac: &str) -> Option<Arc<AACPManager>> {
        self.device_managers
            .blocking_read()
            .get(mac)
            .and_then(|m| m.get_aacp())
            .or_else(|| {
                self.demo
                    .as_ref()
                    .filter(|d| d.mac == mac)
                    .map(|d| d.aacp.clone())
            })
    }

    fn save_settings(&self) {
        AppSettings {
            theme: self.selected_theme,
            tray_text_mode: self.tray_text_mode,
            stem_control: self.stem_control,
            hires_mic_enabled: self.hires_mic_enabled,
            hires_mic_agc: self.hires_mic_agc,
            hires_mic_pause_convo: self.hires_mic_pause_convo,
            a2dp_reset: self.a2dp_reset,
            preferred_codec: self.preferred_codec,
            ..self.macfeel.clone()
        }
        .save();
    }

    fn title(&self, _id: window::Id) -> String {
        "LibrePods".to_string()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowOpened(id) => {
                self.window = Some(id);
                // Demo/screenshot aid: LIBREPODS_UI_DEMO_SCROLL=<px> opens scrolled.
                if self.demo.is_some()
                    && let Some(y) = std::env::var("LIBREPODS_UI_DEMO_SCROLL")
                        .ok()
                        .and_then(|v| v.parse::<f32>().ok())
                {
                    return iced::widget::operation::scroll_to(
                        iced::widget::Id::new(style::PAGE_SCROLL),
                        iced::widget::operation::AbsoluteOffset { x: None, y: Some(y) },
                    );
                }
                Task::none()
            }
            Message::WindowClosed(id) => {
                if self.window == Some(id) {
                    self.window = None;
                }
                Task::none()
            }
            Message::SelectTab(tab) => {
                self.selected_tab = tab;
                Task::none()
            }
            Message::ThemeSelected(theme) => {
                self.selected_theme = theme;
                self.save_settings();
                Task::none()
            }
            Message::CopyToClipboard(data) => iced::clipboard::write(data),
            Message::MicLevelTick => Task::none(),
            Message::BluetoothMessage(ui_message) => {
                match ui_message {
                    BluetoothUIMessage::NoOp => {
                        let ui_rx = Arc::clone(&self.ui_rx);

                        Task::perform(wait_for_message(ui_rx), |msg| msg)
                    }
                    BluetoothUIMessage::OpenWindow => {
                        let ui_rx = Arc::clone(&self.ui_rx);
                        let wait_task = Task::perform(wait_for_message(ui_rx), |msg| msg);
                        debug!("Opening main window...");
                        // "AirPods Settings…" opens straight onto the AirPods page.
                        if let Some(mac) = self.preferred_airpods() {
                            self.selected_tab = Tab::Device(mac);
                        }
                        if let Some(window_id) = self.window {
                            Task::batch(vec![window::gain_focus(window_id), wait_task])
                        } else {
                            let (new_window_task, open_task) = window::open(main_window_settings());
                            self.window = Some(new_window_task);
                            Task::batch(vec![open_task.map(Message::WindowOpened), wait_task])
                        }
                    }
                    BluetoothUIMessage::DeviceConnected(mac) => {
                        let ui_rx = Arc::clone(&self.ui_rx);
                        let wait_task = Task::perform(wait_for_message(ui_rx), |msg| msg);
                        debug!(
                            "Device connected: {}. Adding to connected devices list",
                            mac
                        );
                        let mut already_connected = false;
                        for device in &self.bluetooth_state.connected_devices {
                            if device == &mac {
                                already_connected = true;
                                break;
                            }
                        }
                        if !already_connected {
                            self.bluetooth_state.connected_devices.push(mac.clone());
                        }
                        if self.selected_tab == Tab::Device("none".to_string())
                            && self.devices_list().contains_key(&mac)
                        {
                            self.selected_tab = Tab::Device(mac.clone());
                        }

                        // self.device_states.insert(mac.clone(), DeviceState::AirPods(AirPodsState {
                        //     conversation_awareness_enabled: false,
                        // }));

                        let type_ = {
                            let devices_json = std::fs::read_to_string(get_devices_path())
                                .unwrap_or_else(|e| {
                                    error!("Failed to read devices file: {}", e);
                                    "{}".to_string()
                                });
                            let devices_list: HashMap<String, DeviceData> =
                                serde_json::from_str(&devices_json).unwrap_or_else(|e| {
                                    error!("Deserialization failed: {}", e);
                                    HashMap::new()
                                });
                            devices_list.get(&mac).map(|d| d.type_.clone())
                        };
                        match type_ {
                            Some(DeviceType::AirPods) => {
                                let device_managers = self.device_managers.blocking_read();
                                let device_manager = device_managers.get(&mac).unwrap();
                                let aacp_manager = device_manager.get_aacp().unwrap();
                                let aacp_manager_state = aacp_manager.state.clone();
                                let state = aacp_manager_state.blocking_lock();
                                debug!("AACP manager found for AirPods device {}", mac);
                                let device_name = {
                                    let devices_json = std::fs::read_to_string(get_devices_path())
                                        .unwrap_or_else(|e| {
                                            error!("Failed to read devices file: {}", e);
                                            "{}".to_string()
                                        });
                                    let devices_list: HashMap<String, DeviceData> =
                                        serde_json::from_str(&devices_json).unwrap_or_else(|e| {
                                            error!("Deserialization failed: {}", e);
                                            HashMap::new()
                                        });
                                    devices_list
                                        .get(&mac)
                                        .map(|d| d.name.clone())
                                        .unwrap_or_else(|| "Unknown Device".to_string())
                                };
                                self.device_states.insert(mac.clone(), DeviceState::AirPods(AirPodsState {
                                    device_name,
                                    battery: state.battery_info.clone(),
                                    noise_control_mode: state.control_command_status_list.iter().find_map(|status| {
                                        if status.identifier == ControlCommandIdentifiers::ListeningMode {
                                            status.value.first().map(AirPodsNoiseControlMode::from_byte)
                                        } else {
                                            None
                                        }
                                    }).unwrap_or(AirPodsNoiseControlMode::Transparency),
                                    noise_control_state: combo_box::State::new(
                                        {
                                            let mut modes = vec![
                                                AirPodsNoiseControlMode::Transparency,
                                                AirPodsNoiseControlMode::NoiseCancellation,
                                                AirPodsNoiseControlMode::Adaptive
                                            ];
                                            if state.control_command_status_list.iter().any(|status| {
                                                status.identifier == ControlCommandIdentifiers::AllowOffOption &&
                                                matches!(status.value.as_slice(), [0x01])
                                            }) {
                                                modes.insert(0, AirPodsNoiseControlMode::Off);
                                            }
                                            modes
                                        }
                                    ),
                                    conversation_awareness_enabled: state.control_command_status_list.iter().any(|status| {
                                        status.identifier == ControlCommandIdentifiers::ConversationDetectConfig &&
                                        matches!(status.value.as_slice(), [0x01])
                                    }),
                                    personalized_volume_enabled: state.control_command_status_list.iter().any(|status| {
                                        status.identifier == ControlCommandIdentifiers::AdaptiveVolumeConfig &&
                                        matches!(status.value.as_slice(), [0x01])
                                    }),
                                    allow_off_mode: state.control_command_status_list.iter().any(|status| {
                                        status.identifier == ControlCommandIdentifiers::AllowOffOption &&
                                        matches!(status.value.as_slice(), [0x01])
                                    }),
                                    hires_mic_enabled: self.hires_mic_enabled,
                                    controls: state
                                        .control_command_status_list
                                        .iter()
                                        .map(|s| (s.identifier as u8, s.value.clone()))
                                        .collect(),
                                }));
                            }
                            Some(DeviceType::Nothing) => {
                                self.device_states.insert(
                                    mac.clone(),
                                    DeviceState::Nothing(NothingState {
                                        anc_mode: NothingAncMode::Off,
                                        anc_mode_state: combo_box::State::new(vec![
                                            NothingAncMode::Off,
                                            NothingAncMode::Transparency,
                                            NothingAncMode::AdaptiveNoiseCancellation,
                                            NothingAncMode::LowNoiseCancellation,
                                            NothingAncMode::MidNoiseCancellation,
                                            NothingAncMode::HighNoiseCancellation,
                                        ]),
                                    }),
                                );
                            }
                            _ => {}
                        }

                        Task::batch(vec![wait_task])
                    }
                    BluetoothUIMessage::DeviceDisconnected(mac) => {
                        let ui_rx = Arc::clone(&self.ui_rx);
                        let wait_task = Task::perform(wait_for_message(ui_rx), |msg| msg);
                        debug!("Device disconnected: {}", mac);

                        self.bluetooth_state
                            .connected_devices
                            .retain(|device| device != &mac);

                        self.device_states.remove(&mac);


                        Task::batch(vec![wait_task])
                    }
                    BluetoothUIMessage::AACPUIEvent(mac, event) => {
                        let ui_rx = Arc::clone(&self.ui_rx);
                        let wait_task = Task::perform(wait_for_message(ui_rx), |msg| msg);
                        debug!("AACP UI Event for {}: {:?}", mac, event);
                        match event {
                            AACPEvent::ControlCommand(status) => {
                                if let Some(DeviceState::AirPods(state)) = self.device_states.get_mut(&mac) {
                                    state.controls.insert(status.identifier as u8, status.value.clone());
                                }
                                match status.identifier {
                                ControlCommandIdentifiers::ListeningMode => {
                                    let mode = status
                                        .value
                                        .first()
                                        .map(AirPodsNoiseControlMode::from_byte)
                                        .unwrap_or(AirPodsNoiseControlMode::Transparency);
                                    if let Some(DeviceState::AirPods(state)) =
                                        self.device_states.get_mut(&mac)
                                    {
                                        state.noise_control_mode = mode;
                                    }
                                }
                                ControlCommandIdentifiers::ConversationDetectConfig => {
                                    let is_enabled = match status.value.as_slice() {
                                        [0x01] => true,
                                        [0x02] => false,
                                        _ => {
                                            error!(
                                                "Unknown Conversation Detect Config value: {:?}",
                                                status.value
                                            );
                                            false
                                        }
                                    };
                                    if let Some(DeviceState::AirPods(state)) =
                                        self.device_states.get_mut(&mac)
                                    {
                                        state.conversation_awareness_enabled = is_enabled;
                                    }
                                }
                                ControlCommandIdentifiers::AdaptiveVolumeConfig => {
                                    let is_enabled = match status.value.as_slice() {
                                        [0x01] => true,
                                        [0x02] => false,
                                        _ => {
                                            error!(
                                                "Unknown Adaptive Volume Config value: {:?}",
                                                status.value
                                            );
                                            false
                                        }
                                    };
                                    if let Some(DeviceState::AirPods(state)) =
                                        self.device_states.get_mut(&mac)
                                    {
                                        state.personalized_volume_enabled = is_enabled;
                                    }
                                }
                                ControlCommandIdentifiers::AllowOffOption => {
                                    let is_enabled = match status.value.as_slice() {
                                        [0x01] => true,
                                        [0x02] => false,
                                        _ => {
                                            error!(
                                                "Unknown Allow Off Option value: {:?}",
                                                status.value
                                            );
                                            false
                                        }
                                    };
                                    if let Some(DeviceState::AirPods(state)) =
                                        self.device_states.get_mut(&mac)
                                    {
                                        state.allow_off_mode = is_enabled;
                                        state.noise_control_state = combo_box::State::new({
                                            let mut modes = vec![
                                                AirPodsNoiseControlMode::Transparency,
                                                AirPodsNoiseControlMode::NoiseCancellation,
                                                AirPodsNoiseControlMode::Adaptive,
                                            ];
                                            if is_enabled {
                                                modes.insert(0, AirPodsNoiseControlMode::Off);
                                            }
                                            modes
                                        });
                                    }
                                }
                                _ => {
                                    debug!("Unhandled Control Command Status: {:?}", status);
                                }
                            }},
                            AACPEvent::BatteryInfo(battery_info) => {
                                if let Some(DeviceState::AirPods(state)) =
                                    self.device_states.get_mut(&mac)
                                {
                                    state.battery = battery_info;
                                    debug!("Updated battery info for {}: {:?}", mac, state.battery);
                                }
                            }
                            _ => {}
                        }
                        Task::batch(vec![wait_task])
                    }
                    BluetoothUIMessage::ATTNotification(mac, handle, value) => {
                        debug!(
                            "ATT Notification for {}: handle=0x{:04X}, value={:?}",
                            mac, handle, value
                        );

                        // TODO: Handle Nothing's ANC Mode changes here

                        let ui_rx = Arc::clone(&self.ui_rx);
                        let wait_task = Task::perform(wait_for_message(ui_rx), |msg| msg);
                        Task::batch(vec![wait_task])
                    }
                }
            }
            // Message::ShowNewDialogTab => {
            //     debug!("switching to Add Device tab");
            //     self.selected_tab = Tab::AddDevice;
            //     Task::perform(load_paired_devices(), Message::GotPairedDevices)
            // }
            Message::GotPairedDevices(map) => {
                self.paired_devices = map;
                Task::none()
            }
            Message::StartAddDevice(name, addr) => {
                self.pending_add_device = Some((name, addr));
                self.selected_device_type = None;
                Task::none()
            }
            Message::SelectDeviceType(device_type) => {
                self.selected_device_type = Some(device_type);
                Task::none()
            }
            Message::ConfirmAddDevice => {
                if let Some((name, addr)) = self.pending_add_device.take()
                    && let Some(type_) = self.selected_device_type.take()
                {
                    let devices_path = get_devices_path();
                    let devices_json = std::fs::read_to_string(&devices_path).unwrap_or_else(|e| {
                        error!("Failed to read devices file: {}", e);
                        "{}".to_string()
                    });
                    let mut devices_list: HashMap<String, DeviceData> =
                        serde_json::from_str(&devices_json).unwrap_or_else(|e| {
                            error!("Deserialization failed: {}", e);
                            HashMap::new()
                        });
                    devices_list.insert(
                        addr.to_string(),
                        DeviceData {
                            name,
                            type_: type_.clone(),
                            information: None,
                        },
                    );
                    let updated_json = serde_json::to_string(&devices_list).unwrap_or_else(|e| {
                        error!("Serialization failed: {}", e);
                        "{}".to_string()
                    });
                    if let Err(e) = std::fs::write(&devices_path, updated_json) {
                        error!("Failed to write devices file: {}", e);
                    }
                    self.selected_tab = Tab::Device(addr.to_string());
                }
                Task::none()
            }
            Message::CancelAddDevice => {
                self.pending_add_device = None;
                self.selected_device_type = None;
                Task::none()
            }
            Message::StateChanged(mac, state) => {
                if let DeviceState::AirPods(a) = &state
                    && a.hires_mic_enabled != self.hires_mic_enabled
                {
                    self.hires_mic_enabled = a.hires_mic_enabled;
                    self.save_settings();
                }
                self.device_states.insert(mac.clone(), state);
                // if airpods, update the noise control state combo box based on allow off mode
                let type_ = {
                    let devices_json =
                        std::fs::read_to_string(get_devices_path()).unwrap_or_else(|e| {
                            error!("Failed to read devices file: {}", e);
                            "{}".to_string()
                        });
                    let devices_list: HashMap<String, DeviceData> =
                        serde_json::from_str(&devices_json).unwrap_or_else(|e| {
                            error!("Deserialization failed: {}", e);
                            HashMap::new()
                        });
                    devices_list.get(&mac).map(|d| d.type_.clone())
                };
                if let Some(DeviceType::AirPods) = type_
                    && let Some(DeviceState::AirPods(state)) = self.device_states.get_mut(&mac)
                {
                    state.noise_control_state = combo_box::State::new({
                        let mut modes = vec![
                            AirPodsNoiseControlMode::Transparency,
                            AirPodsNoiseControlMode::NoiseCancellation,
                            AirPodsNoiseControlMode::Adaptive,
                        ];
                        if state.allow_off_mode {
                            modes.insert(0, AirPodsNoiseControlMode::Off);
                        }
                        modes
                    });
                }
                Task::none()
            }
            Message::TrayTextModeChanged(is_enabled) => {
                self.tray_text_mode = is_enabled;
                self.save_settings();
                Task::none()
            }
            Message::PreferredCodecChanged(codec) => {
                self.preferred_codec = codec;
                self.save_settings();
                // Apply now instead of on the next reconnect.
                for mac in self.bluetooth_state.connected_devices.clone() {
                    std::thread::spawn(move || {
                        if let Some(p) = crate::media_controller::apply_preferred_codec_sync(&mac) {
                            log::info!("Switched {} to {}", mac, p);
                        }
                    });
                }
                Task::none()
            }
            Message::RenameDevice(mac, name) => {
                let name = name.trim().to_string();
                if !name.is_empty()
                    && let Ok(managers) = self.device_managers.try_read()
                    && let Some(aacp) = managers.get(&mac).and_then(|m| m.get_aacp())
                {
                    let a = aacp.clone();
                    aacp.runtime().spawn(async move {
                        if let Err(e) = a.send_rename_packet(&name).await {
                            error!("Rename failed: {}", e);
                        }
                    });
                }
                Task::none()
            }
            Message::MacFeel(setting) => {
                let m = &mut self.macfeel;
                match setting {
                    MacFeelSetting::CasePopup(v) => m.case_popup = v,
                    MacFeelSetting::ConnectPopup(v) => m.connect_popup = v,
                    MacFeelSetting::AutoConnectOnOpen(v) => m.auto_connect_on_open = v,
                    MacFeelSetting::LowBatteryAlerts(v) => m.low_battery_alerts = v,
                    MacFeelSetting::ModeToasts(v) => m.mode_toasts = v,
                    MacFeelSetting::MediaTakeover(v) => m.media_takeover = v,
                    MacFeelSetting::EarPause(v) => m.ear_pause = v,
                    MacFeelSetting::SwitchOutput(v) => m.switch_output_when_removed = v,
                    MacFeelSetting::HideTrayIcon(v) => m.hide_tray_icon = v,
                    MacFeelSetting::Bluetooth(on) => {
                        crate::hub::hub().update("status", |s| s.bluetooth_on = on);
                        std::thread::spawn(move || {
                            let rt = tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build();
                            if let Ok(rt) = rt
                                && let Err(e) = rt.block_on(crate::ipc::set_bluetooth(on))
                            {
                                error!("Bluetooth power change failed: {}", e);
                            }
                        });
                        return Task::none();
                    }
                }
                self.save_settings();
                Task::none()
            }
            Message::SendControl(mac, id, value) => {
                if let Some(DeviceState::AirPods(st)) = self.device_states.get_mut(&mac) {
                    st.controls.insert(id as u8, value.clone());
                    if id == ControlCommandIdentifiers::ListeningMode
                        && let Some(b) = value.first()
                    {
                        st.noise_control_mode = AirPodsNoiseControlMode::from_byte(b);
                    }
                }
                if let Some(aacp) = self.aacp_for(&mac) {
                    // Cycling through Off needs Off allowed first, as on iOS.
                    if id == ControlCommandIdentifiers::ListeningModeConfigs
                        && value.first().is_some_and(|m| m & 0x01 != 0)
                    {
                        crate::ui::airpods_extra::send(
                            &aacp,
                            ControlCommandIdentifiers::AllowOffOption,
                            vec![0x01],
                        );
                        if let Some(DeviceState::AirPods(st)) = self.device_states.get_mut(&mac) {
                            st.allow_off_mode = true;
                        }
                    }
                    crate::ui::airpods_extra::send(&aacp, id, value);
                }
                Task::none()
            }
            Message::ConnectDevice(mac, connect) => {
                if connect {
                    crate::hub::hub().emit("connecting");
                }
                std::thread::spawn(move || {
                    let action = if connect { "connect" } else { "disconnect" };
                    if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build()
                        && let Err(e) = rt.block_on(crate::ipc::bluetoothctl(action, &mac))
                    {
                        error!("{action} {mac} failed: {e}");
                        if connect {
                            crate::hub::hub().emit("connect_failed");
                        }
                    }
                });
                Task::none()
            }
            Message::ConnectAny => {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).args(["ctl", "connect"]).spawn();
                }
                Task::none()
            }
            Message::HubChanged => Task::none(),
            Message::StemControlChanged(is_enabled) => {
                self.stem_control = is_enabled;
                self.save_settings();
                Task::none()
            }
            Message::A2dpResetChanged(is_enabled) => {
                self.a2dp_reset = is_enabled;
                self.save_settings();
                Task::none()
            }
            Message::HiResMicAgcChanged(is_enabled) => {
                self.hires_mic_agc = is_enabled;
                self.save_settings();
                Task::none()
            }
            Message::HiResMicPauseConvoChanged(is_enabled) => {
                self.hires_mic_pause_convo = is_enabled;
                self.save_settings();
                Task::none()
            }
        }
    }

    fn view(&self, _id: window::Id) -> Element<'_, Message> {
        let theme: Theme = self.selected_theme.into();
        let devices_list = self.devices_list();
        let content: Element<'_, Message> = match &self.selected_tab {
            Tab::Settings => self.settings_page(),
            Tab::AddDevice => self.add_device_page(&devices_list),
            Tab::Device(id) if !devices_list.contains_key(id) => self.empty_page(),
            Tab::Device(id) => self.device_page(&theme, id, &devices_list),
        };
        row![
            self.sidebar(&devices_list),
            container(Space::new())
                .width(Length::Fixed(1.0))
                .height(Length::Fill)
                .style(|t: &Theme| container::Style {
                    background: Some(Background::Color(style::hairline(t))),
                    ..Default::default()
                }),
            container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|t: &Theme| container::Style {
                    background: Some(Background::Color(style::bg(t))),
                    ..Default::default()
                }),
        ]
        .into()
    }

    fn device_page<'a>(
        &'a self,
        theme: &Theme,
        id: &'a str,
        devices_list: &HashMap<String, DeviceData>,
    ) -> Element<'a, Message> {
        let name = devices_list
            .get(id)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "AirPods".into());
        match devices_list.get(id).map(|d| d.type_.clone()) {
            Some(DeviceType::AirPods) => match (self.device_states.get(id), self.aacp_for(id)) {
                (Some(DeviceState::AirPods(state)), Some(aacp)) => airpods_view(
                    theme,
                    id,
                    devices_list,
                    state,
                    aacp,
                    self.hires_mic_pause_convo,
                ),
                _ => offline_view(theme, id, name),
            },
            Some(DeviceType::Nothing) => {
                let att = self
                    .device_managers
                    .blocking_read()
                    .get(id)
                    .and_then(|m| m.get_att());
                match (self.device_states.get(id), att) {
                    (Some(DeviceState::Nothing(state)), Some(att)) => {
                        nothing_view(id, devices_list, state, att)
                    }
                    _ => style::page(
                        name,
                        style::section(
                            None,
                            vec![style::list_row(
                                None,
                                "Not connected",
                                Some("Connect this device to change its settings.".into()),
                                style::pill("Connect", Pill::Primary, Some(Message::ConnectDevice(id.to_string(), true))),
                            )],
                            None,
                        ),
                    ),
                }
            }
            None => self.empty_page(),
        }
    }

    /// No AirPods known yet (or none selected): a friendly hint, not a void.
    fn empty_page(&self) -> Element<'_, Message> {
        let card = container(
            column![
                image(style::art(None).hero.clone())
                    .height(Length::Fixed(150.0))
                    .filter_method(image::FilterMethod::Linear)
                    .opacity(0.85),
                Space::new().height(Length::Fixed(14.0)),
                text("No AirPods connected").font(style::SEMIBOLD).size(19),
                text("Open the AirPods case near this computer, or put them in your ears.")
                    .size(13)
                    .style(style::text_color(style::secondary)),
                Space::new().height(Length::Fixed(16.0)),
                style::pill("Connect", Pill::Primary, Some(Message::ConnectAny)),
            ]
            .spacing(4)
            .align_x(Center),
        )
        .padding(Padding {
            top: 30.0,
            bottom: 30.0,
            left: 40.0,
            right: 40.0,
        })
        .style(|t: &Theme| {
            let mut s = style::card_style(t);
            s.border.radius = 18.0.into();
            s
        });
        container(card).center(Length::Fill).into()
    }

    fn sidebar(&self, devices_list: &HashMap<String, DeviceData>) -> Element<'_, Message> {
        let hub = crate::hub::hub().snapshot();
        let mut devices: Vec<(String, DeviceData)> = devices_list.clone().into_iter().collect();
        devices.sort_by(|a, b| {
            (a.1.type_ != DeviceType::AirPods, a.1.name.clone())
                .cmp(&(b.1.type_ != DeviceType::AirPods, b.1.name.clone()))
        });

        let mut list = column![].spacing(2);
        for (mac, device) in devices {
            let connected = self.bluetooth_state.connected_devices.contains(&mac);
            let subtitle = if connected {
                let min = match self.device_states.get(&mac) {
                    Some(DeviceState::AirPods(st)) => {
                        let lv = levels(&mac, &st.battery);
                        lv.headphone
                            .map(|h| h.0)
                            .or_else(|| [lv.left, lv.right].iter().flatten().map(|b| b.0).min())
                    }
                    _ => None,
                };
                match min {
                    Some(l) => format!("Connected · {l}%"),
                    None => "Connected".into(),
                }
            } else if hub.mac.as_deref() == Some(mac.as_str()) && hub.nearby {
                "Nearby".into()
            } else {
                "Not connected".into()
            };
            let icon: Element<'_, Message> = if device.type_ == DeviceType::AirPods {
                let model = if hub.mac.as_deref() == Some(mac.as_str()) || hub.mac.is_none() {
                    hub.model_id
                } else {
                    None
                };
                container(
                    image(style::art(model).thumb.clone())
                        .height(Length::Fixed(24.0))
                        .filter_method(image::FilterMethod::Linear),
                )
                .center(Length::Fixed(28.0))
                .into()
            } else {
                style::icon_tile(style::G_HEADPHONES, style::tint_grey)
            };
            let selected = self.selected_tab == Tab::Device(mac.clone());
            list = list.push(side_item(
                icon,
                device.name.clone(),
                Some((subtitle, connected)),
                selected,
                Message::SelectTab(Tab::Device(mac.clone())),
            ));
        }
        if devices_list.is_empty() {
            list = list.push(
                container(
                    text("Your AirPods appear here after they connect once.")
                        .size(11.5)
                        .style(style::text_color(style::tertiary)),
                )
                .padding(Padding {
                    top: 4.0,
                    bottom: 0.0,
                    left: 10.0,
                    right: 10.0,
                }),
            );
        }

        let settings = side_item(
            style::icon_tile(style::G_GEAR, style::tint_grey),
            "Settings".into(),
            None,
            self.selected_tab == Tab::Settings,
            Message::SelectTab(Tab::Settings),
        );

        container(
            column![
                container(
                    row![
                        style::icon_tile(style::G_EARBUDS, style::tint_accent),
                        text("LibrePods").font(style::SEMIBOLD).size(15),
                    ]
                    .spacing(10)
                    .align_y(Center)
                )
                .padding(Padding {
                    top: 2.0,
                    bottom: 0.0,
                    left: 8.0,
                    right: 8.0,
                }),
                Space::new().height(Length::Fixed(20.0)),
                container(
                    text("DEVICES")
                        .font(style::SEMIBOLD)
                        .size(10.5)
                        .style(style::text_color(style::tertiary))
                )
                .padding(Padding {
                    top: 0.0,
                    bottom: 4.0,
                    left: 10.0,
                    right: 10.0,
                }),
                list,
                Space::new().height(Length::Fill),
                settings,
            ]
            .padding(Padding {
                top: 18.0,
                bottom: 14.0,
                left: 10.0,
                right: 10.0,
            }),
        )
        .width(Length::Fixed(236.0))
        .height(Length::Fill)
        .style(|t: &Theme| container::Style {
            background: Some(Background::Color(style::sidebar(t))),
            ..Default::default()
        })
        .into()
    }

    fn settings_page(&self) -> Element<'_, Message> {
        let bt_on = crate::hub::hub().snapshot().bluetooth_on;
        let m = &self.macfeel;
        let mac_toggle = |title: &'static str,
                          subtitle: &'static str,
                          value: bool,
                          f: fn(bool) -> MacFeelSetting|
         -> Element<'_, Message> {
            style::list_row(
                None,
                title,
                Some(subtitle.to_string()),
                style::switch(value)
                    .on_toggle(move |v| Message::MacFeel(f(v)))
                    .into(),
            )
        };
        let plain_toggle = |title: &'static str,
                            subtitle: &'static str,
                            value: bool,
                            f: fn(bool) -> Message|
         -> Element<'_, Message> {
            style::list_row(
                None,
                title,
                Some(subtitle.to_string()),
                style::switch(value).on_toggle(f).into(),
            )
        };

        let content = column![
            style::section(
                Some("Bluetooth"),
                vec![style::list_row(
                    Some(style::icon_tile(style::G_BLUETOOTH, style::tint_blue)),
                    "Bluetooth",
                    Some(if bt_on { "On".into() } else { "Off".into() }),
                    style::switch(bt_on)
                        .on_toggle(|v| Message::MacFeel(MacFeelSetting::Bluetooth(v)))
                        .into(),
                )],
                Some("Also on the bar: right-click the Bluetooth icon, or press Super + Alt + B.".into()),
            ),
            style::section(
                Some("Mac Experience"),
                vec![
                    mac_toggle("Case-Open Card", "Show the battery card with a Connect button when you open the case nearby.", m.case_popup, MacFeelSetting::CasePopup),
                    mac_toggle("Connect When the Case Opens", "Take the AirPods as soon as the lid opens, unless they're busy on another device.", m.auto_connect_on_open, MacFeelSetting::AutoConnectOnOpen),
                    mac_toggle("Connected Card", "Briefly show the battery card whenever the AirPods connect.", m.connect_popup, MacFeelSetting::ConnectPopup),
                    mac_toggle("Automatic Switching", "When media starts playing here, move the AirPods over from your phone.", m.media_takeover, MacFeelSetting::MediaTakeover),
                    mac_toggle("Pause Media When Removed", "Pause when you take an AirPod out, resume when you put it back.", m.ear_pause, MacFeelSetting::EarPause),
                    mac_toggle("Use Speakers When Not Wearing", "Switch output back to this computer while the AirPods are out of your ears.", m.switch_output_when_removed, MacFeelSetting::SwitchOutput),
                    mac_toggle("Low Battery Alerts", "Notify at 20 % and 10 %, like iPhone and Mac.", m.low_battery_alerts, MacFeelSetting::LowBatteryAlerts),
                    mac_toggle("Listening Mode Toasts", "A small on-screen hint when the listening mode or Conversation Awareness changes.", m.mode_toasts, MacFeelSetting::ModeToasts),
                ],
                None,
            ),
            style::section(
                Some("Audio"),
                vec![style::list_row(
                    None,
                    "Preferred Codec",
                    Some("Used for playback; the others are fallbacks.".into()),
                    style::picker(
                        PreferredCodec::ALL.to_vec(),
                        Some(self.preferred_codec),
                        Message::PreferredCodecChanged,
                    ),
                )],
                None,
            ),
            style::section(
                Some("Controls"),
                vec![plain_toggle(
                    "Stem Press Track Control",
                    "Double press for next track, triple press for previous. Turn off if your desktop already handles the AirPods' media keys.",
                    self.stem_control,
                    Message::StemControlChanged,
                )],
                None,
            ),
            style::section(
                Some("Hi-Res Microphone"),
                vec![
                    plain_toggle("Automatic Gain", "Normalises the hi-res microphone level. Turn off for a raw capture.", self.hires_mic_agc, Message::HiResMicAgcChanged),
                    plain_toggle("Pause Conversation Awareness While Capturing", "Turns Conversation Awareness off during capture and restores it afterwards.", self.hires_mic_pause_convo, Message::HiResMicPauseConvoChanged),
                    plain_toggle("Reset Audio After Capture", "Briefly restarts playback when the mic starts or stops. Prevents one AirPod going silent on some setups.", self.a2dp_reset, Message::A2dpResetChanged),
                ],
                Some("Turn the hi-res microphone itself on per device, under Microphone.".into()),
            ),
            style::section(
                Some("Appearance"),
                vec![
                    style::list_row(
                        None,
                        "Theme",
                        None,
                        style::picker(MyTheme::ALL.to_vec(), Some(self.selected_theme), Message::ThemeSelected),
                    ),
                    plain_toggle("Battery as Text in Tray", "Show the percentage instead of a ring.", self.tray_text_mode, Message::TrayTextModeChanged),
                    mac_toggle("Hide Tray Icon", "Use the Waybar AirPods module instead. Applies after a restart.", m.hide_tray_icon, MacFeelSetting::HideTrayIcon),
                ],
                None,
            ),
        ];
        style::page("Settings".into(), content.into())
    }

    fn add_device_page(&self, devices_list: &HashMap<String, DeviceData>) -> Element<'_, Message> {
        let mut rows: Vec<Element<'_, Message>> = Vec::new();
        for (name, addr) in self.paired_devices.clone() {
            if devices_list.contains_key(&addr.to_string()) {
                continue;
            }
            let pending = matches!(&self.pending_add_device, Some((_, a)) if *a == addr);
            let trailing: Element<'_, Message> = if pending {
                row![
                    combo_box(
                        &self.device_type_state,
                        "Device type",
                        self.selected_device_type.as_ref(),
                        Message::SelectDeviceType,
                    )
                    .width(Length::Fixed(150.0))
                    .input_style(|t: &Theme, _| text_input::Style {
                        background: Background::Color(style::control(t)),
                        border: Border {
                            radius: 8.0.into(),
                            ..Default::default()
                        },
                        icon: style::secondary(t),
                        placeholder: style::tertiary(t),
                        value: t.palette().text,
                        selection: style::accent(t).scale_alpha(0.4),
                    })
                    .menu_style(style::menu_style),
                    style::pill("Cancel", Pill::Secondary, Some(Message::CancelAddDevice)),
                    style::pill("Add", Pill::Primary, Some(Message::ConfirmAddDevice)),
                ]
                .spacing(8)
                .align_y(Center)
                .into()
            } else {
                style::pill("Add", Pill::Secondary, Some(Message::StartAddDevice(name.clone(), addr)))
            };
            rows.push(style::list_row(None, name, Some(addr.to_string()), trailing));
        }
        if rows.is_empty() {
            rows.push(style::list_row(
                None,
                "No new paired devices",
                Some("Every paired device is already listed.".into()),
                Space::new().into(),
            ));
        }
        style::page("Add Device".into(), style::section(Some("Paired Devices"), rows, None))
    }

    fn theme(&self, _id: window::Id) -> Theme {
        self.selected_theme.into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let close = window::close_events().map(Message::WindowClosed);

        // Redraw the mic meter at 20 Hz only while a hi-res mic is actually
        // capturing and a window is open; otherwise poll lazily (1 Hz) so a
        // capture that starts is noticed, or not at all.
        let enabled = self.window.is_some()
            && self
                .device_states
                .values()
                .any(|s| matches!(s, DeviceState::AirPods(a) if a.hires_mic_enabled));
        let capturing = enabled
            && self.device_managers.try_read().is_ok_and(|m| {
                m.values()
                    .any(|d| d.get_aacp().is_some_and(|a| a.mic_active()))
            });

        let mut subs = vec![close];
        if self.window.is_some() {
            // Redraw on hub changes (battery, lid, connection, Bluetooth power).
            subs.push(Subscription::run(hub_events));
        }
        if enabled {
            let period = if capturing { 50 } else { 1000 };
            subs.push(
                iced::time::every(std::time::Duration::from_millis(period))
                    .map(|_| Message::MicLevelTick),
            );
        }
        Subscription::batch(subs)
    }
}

async fn wait_for_message(ui_rx: Arc<Mutex<UnboundedReceiver<BluetoothUIMessage>>>) -> Message {
    let mut rx = ui_rx.lock().await;
    match rx.recv().await {
        Some(msg) => Message::BluetoothMessage(msg),
        None => {
            error!("UI message channel closed");
            Message::BluetoothMessage(BluetoothUIMessage::NoOp)
        }
    }
}

fn hub_events() -> impl futures::Stream<Item = Message> {
    futures::stream::unfold(crate::hub::hub().subscribe(), |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(_) => return Some((Message::HubChanged, rx)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    })
}

/// A System Settings sidebar row.
fn side_item<'a>(
    icon: Element<'a, Message>,
    title: String,
    subtitle: Option<(String, bool)>,
    selected: bool,
    msg: Message,
) -> Element<'a, Message> {
    let mut labels = column![text(title).font(style::MEDIUM).size(13.5)].spacing(1);
    if let Some((sub, connected)) = subtitle {
        labels = labels.push(text(sub).size(11.5).style(move |t: &Theme| text::Style {
            color: Some(if selected {
                t.palette().text.scale_alpha(0.75)
            } else if connected {
                style::connected(t)
            } else {
                style::tertiary(t)
            }),
        }));
    }
    button(row![icon, labels].spacing(10).align_y(Center))
        .width(Length::Fill)
        .padding(Padding {
            top: 6.0,
            bottom: 6.0,
            left: 8.0,
            right: 8.0,
        })
        .on_press(msg)
        .style(move |t: &Theme, status: button::Status| {
            let hovered = matches!(status, button::Status::Hovered);
            let background = if selected {
                Some(Background::Color(style::accent(t).scale_alpha(0.24)))
            } else if hovered {
                Some(Background::Color(t.palette().text.scale_alpha(0.06)))
            } else {
                None
            };
            button::Style {
                background,
                text_color: t.palette().text,
                border: Border {
                    radius: 8.0.into(),
                    ..Default::default()
                },
                shadow: Default::default(),
                snap: true,
            }
        })
        .into()
}
