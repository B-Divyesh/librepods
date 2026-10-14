//! Single source of truth for the AirPods state that lives outside the GUI:
//! the IPC socket (CLI, Waybar, popup), notifications and the LE monitor all
//! read and write it here instead of poking the tray or the window directly.

use crate::bluetooth::aacp::ControlCommandIdentifiers;
use crate::ui::messages::BluetoothUIMessage;
use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Battery {
    pub level: u8,
    pub charging: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Pair {
    pub left: Option<bool>,
    pub right: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ListeningMode {
    Off,
    Anc,
    Transparency,
    Adaptive,
}

impl ListeningMode {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x01 => Some(Self::Off),
            0x02 => Some(Self::Anc),
            0x03 => Some(Self::Transparency),
            0x04 => Some(Self::Adaptive),
            _ => None,
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            Self::Off => 0x01,
            Self::Anc => 0x02,
            Self::Transparency => 0x03,
            Self::Adaptive => 0x04,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "anc" | "noise" | "nc" | "noise-cancellation" => Some(Self::Anc),
            "transparency" | "trans" | "t" => Some(Self::Transparency),
            "adaptive" | "auto" => Some(Self::Adaptive),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Anc => "Noise Cancellation",
            Self::Transparency => "Transparency",
            Self::Adaptive => "Adaptive",
        }
    }
}

/// Where we are in getting AirPods back that forgot this computer (repair.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RepairState {
    /// Waiting for the case to be put in pairing mode.
    Waiting,
    Pairing,
    /// Gave up (timeout or pairing error); `librepods ctl repair` retries.
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Status {
    /// Control channel (AACP) is up with this laptop.
    pub connected: bool,
    /// Seen in a BLE advertisement recently.
    pub nearby: bool,
    /// Their BLE advert says they are connected to some other host.
    #[serde(rename = "connected_elsewhere")]
    pub other_host: bool,
    pub name: Option<String>,
    pub model: Option<String>,
    pub model_id: Option<u16>,
    pub mac: Option<String>,
    pub left: Option<Battery>,
    pub right: Option<Battery>,
    pub case: Option<Battery>,
    pub headphone: Option<Battery>,
    pub in_ear: Pair,
    pub in_case: Pair,
    pub lid_open: Option<bool>,
    pub listening_mode: Option<ListeningMode>,
    pub allow_off: Option<bool>,
    pub conversational_awareness: Option<bool>,
    pub auto_connect: bool,
    pub bluetooth_on: bool,
    /// The AirPods forgot this computer and are being paired again.
    pub repair: Option<RepairState>,
    /// What their advert says they are doing on the host they're connected
    /// to (0x04 idle, 0x05 music, 0x06 call, 0x07 ringing; handoff.rs).
    #[serde(skip)]
    pub other_state: u8,
    #[serde(skip)]
    pub last_seen: Option<Instant>,
    /// From BLE: whether the left bud is the primary (AACP reports primary first).
    #[serde(skip)]
    pub primary_left: Option<bool>,
}

impl Status {
    /// Lowest level across the parts that are actually reporting.
    pub fn min_level(&self) -> Option<u8> {
        if let Some(h) = self.headphone {
            return Some(h.level);
        }
        [self.left, self.right]
            .iter()
            .flatten()
            .map(|b| b.level)
            .min()
    }

    pub fn any_bud_charging(&self) -> bool {
        [self.left, self.right, self.headphone]
            .iter()
            .flatten()
            .any(|b| b.charging)
    }

    pub fn supports_adaptive(&self) -> bool {
        // Pro 2 (Lightning/USB-C), Pro 3, AirPods 4 ANC
        matches!(self.model_id, Some(0x2014 | 0x2024 | 0x2027 | 0x201B) | None)
    }

    /// Next listening mode in the same order as the AirPods' own long-press cycle.
    pub fn next_mode(&self) -> ListeningMode {
        let mut order = vec![ListeningMode::Anc, ListeningMode::Transparency];
        if self.supports_adaptive() {
            order.push(ListeningMode::Adaptive);
        }
        if self.allow_off == Some(true) {
            order.push(ListeningMode::Off);
        }
        let cur = self.listening_mode.unwrap_or(ListeningMode::Off);
        match order.iter().position(|m| *m == cur) {
            Some(i) => order[(i + 1) % order.len()],
            None => order[0],
        }
    }
}

pub fn model_name(id: u16) -> &'static str {
    match id {
        0x2002 => "AirPods (1st generation)",
        0x200F => "AirPods (2nd generation)",
        0x2013 => "AirPods (3rd generation)",
        0x2019 => "AirPods 4",
        0x201B => "AirPods 4 (ANC)",
        0x200E => "AirPods Pro",
        0x2014 => "AirPods Pro 2",
        0x2024 => "AirPods Pro 2 (USB-C)",
        0x2027 => "AirPods Pro 3",
        0x200A => "AirPods Max",
        0x201F => "AirPods Max (USB-C)",
        _ => "AirPods",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub event: &'static str,
    /// For "menu": x (logical px) of the bar icon the menu hangs under.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_x: Option<i32>,
    #[serde(flatten)]
    pub status: Status,
}

type CommandTx = UnboundedSender<(ControlCommandIdentifiers, Vec<u8>)>;

pub struct Hub {
    /// Set for connections that already existed when we started, so no
    /// "connected" card pops up at login / service restart.
    quiet_connect: std::sync::atomic::AtomicBool,
    status: Mutex<Status>,
    events: broadcast::Sender<Event>,
    command_tx: Mutex<Option<CommandTx>>,
    ui_tx: Mutex<Option<UnboundedSender<BluetoothUIMessage>>>,
}

static HUB: OnceLock<Hub> = OnceLock::new();

pub fn hub() -> &'static Hub {
    HUB.get_or_init(|| Hub {
        quiet_connect: std::sync::atomic::AtomicBool::new(false),
        status: Mutex::new(Status {
            auto_connect: true,
            ..Default::default()
        }),
        events: broadcast::channel(128).0,
        command_tx: Mutex::new(None),
        ui_tx: Mutex::new(None),
    })
}

impl Hub {
    /// Apply `f` and broadcast. Plain "status" events are only sent when
    /// something visible changed; named events always go out.
    pub fn update(&self, event: &'static str, f: impl FnOnce(&mut Status)) {
        let snapshot = {
            let mut s = self.status.lock().unwrap();
            let before = s.clone();
            f(&mut s);
            if event == "status" && strip_time(&before) == strip_time(&s) {
                return;
            }
            s.clone()
        };
        let _ = self.events.send(Event {
            event,
            anchor_x: None,
            status: snapshot,
        });
    }

    pub fn set_quiet_connect(&self) {
        self.quiet_connect.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn clear_quiet_connect(&self) {
        self.quiet_connect.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Emit "connected", or just a status update for a pre-existing connection.
    pub fn announce_connected(&self) {
        if self.quiet_connect.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.update("status", |_| {});
        } else {
            self.emit("connected");
        }
    }

    /// The control channel is gone (also when BlueZ dropped the device, which
    /// the Connected=false watcher in main.rs cannot look up any more).
    pub fn mark_disconnected(&self) {
        self.set_command_tx(None);
        self.update("disconnected", |s| {
            s.connected = false;
            s.listening_mode = None;
            s.conversational_awareness = None;
            s.allow_off = None;
            s.headphone = None;
            s.in_ear = Default::default();
        });
    }

    pub fn emit(&self, event: &'static str) {
        self.update(event, |_| {});
    }

    /// Ask the popup process to drop down the bar menu under `x`.
    pub fn emit_menu(&self, x: Option<i32>) {
        let _ = self.events.send(Event {
            event: "menu",
            anchor_x: x,
            status: self.snapshot(),
        });
    }

    pub fn snapshot(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn set_command_tx(&self, tx: Option<CommandTx>) {
        *self.command_tx.lock().unwrap() = tx;
    }

    pub fn send_command(&self, id: ControlCommandIdentifiers, value: Vec<u8>) -> Result<(), String> {
        match &*self.command_tx.lock().unwrap() {
            Some(tx) => tx
                .send((id, value))
                .map_err(|_| "control channel closed".to_string()),
            None => Err("AirPods are not connected".to_string()),
        }
    }

    pub fn set_ui_tx(&self, tx: UnboundedSender<BluetoothUIMessage>) {
        *self.ui_tx.lock().unwrap() = Some(tx);
    }

    pub fn send_ui(&self, msg: BluetoothUIMessage) -> Result<(), String> {
        match &*self.ui_tx.lock().unwrap() {
            Some(tx) => tx.send(msg).map_err(|_| "window is not running".to_string()),
            None => Err("running without a window (--no-tray)".to_string()),
        }
    }
}

fn strip_time(s: &Status) -> Status {
    Status {
        last_seen: None,
        ..s.clone()
    }
}
