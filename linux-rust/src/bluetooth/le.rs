//! Watches Apple "proximity pairing" BLE advertisements (type 0x07) from our
//! AirPods: battery when the control channel is down, in-ear/in-case state,
//! and the case lid, which drives the iOS-style case-open popup.
//!
//! Frame layout, index into the manufacturer data after the company id:
//!   [0]=07 [1]=19 [2]=01 [3..4]=model (LE) [5]=status [6]=pod battery nibbles
//!   [7]=charging flags (hi nibble) + case battery (lo) [8]=lid [9]=colour
//!   [10]=connection state [11..27]=AES-128-ECB(ENC_KEY) with precise levels
//! Cross-checked against capod, MagicPodsCore, OpenPods and furiousMAC.

use crate::bluetooth::aacp::{AACPEvent, BatteryComponent, BatteryInfo, BatteryStatus};
use crate::devices::enums::{DeviceData, DeviceInformation, DeviceType};
use crate::hub::{Battery, hub, model_name};
use crate::ui::messages::BluetoothUIMessage;
use crate::ui::tray::MyTray;
use crate::utils::{AppSettings, ah, get_devices_path};
use aes::Aes128;
use aes::cipher::Array;
use aes::cipher::{BlockCipherDecrypt, KeyInit};
use bluer::monitor::{Monitor, MonitorEvent, Pattern};
use bluer::{Address, Session};
use futures::StreamExt;
use log::{debug, info, warn};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;

const NEARBY_TIMEOUT: Duration = Duration::from_secs(45);
const LID_STALE: Duration = Duration::from_secs(30);
const AUTO_CONNECT_BACKOFF: Duration = Duration::from_secs(30);

fn decrypt(key: &[u8; 16], data: &[u8; 16]) -> [u8; 16] {
    let cipher = Aes128::new(&Array::from(*key));
    let mut block = Array::from(*data);
    cipher.decrypt_block(&mut block);
    block.into()
}

fn verify_rpa(addr: &str, irk: &[u8; 16]) -> bool {
    let Ok(rpa) = addr
        .split(':')
        .map(|s| u8::from_str_radix(s, 16))
        .collect::<Result<Vec<u8>, _>>()
    else {
        return false;
    };
    if rpa.len() != 6 || rpa[0] >> 6 != 0b01 {
        return false; // not a resolvable private address
    }
    let rpa: Vec<u8> = rpa.into_iter().rev().collect();
    let prand: [u8; 3] = [rpa[3], rpa[4], rpa[5]];
    let hash: [u8; 3] = [rpa[0], rpa[1], rpa[2]];
    ah(irk, &prand) == hash
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Advert {
    pub model: u16,
    pub primary_left: bool,
    pub this_in_case: bool,
    pub both_in_case: bool,
    pub in_ear_left: bool,
    pub in_ear_right: bool,
    /// Only meaningful while a bud is in the case.
    pub lid_closed: Option<bool>,
    pub lid_counter: u8,
    pub color: u8,
    /// 0 = not connected to any host, 4 idle, 5 music, 6 call, 7 ringing.
    pub conn_state: u8,
    pub left: Option<Battery>,
    pub right: Option<Battery>,
    pub case: Option<Battery>,
}

pub fn parse_advert(d: &[u8]) -> Option<Advert> {
    if d.len() != 27 || d[0] != 0x07 || d[1] != 0x19 || d[2] != 0x01 {
        return None;
    }
    let status = d[5];
    let primary_left = status & 0x20 != 0;
    let this_in_case = status & 0x40 != 0;
    let both_in_case = status & 0x04 != 0;
    let flipped = !primary_left;
    let ear_flipped = flipped ^ this_in_case;
    let (in_ear_left, in_ear_right) = if ear_flipped {
        (status & 0x08 != 0, status & 0x02 != 0)
    } else {
        (status & 0x02 != 0, status & 0x08 != 0)
    };

    let nib = |n: u8, charging: bool| match n {
        0x0F => None,
        n => Some(Battery {
            level: (n.min(10)) * 10,
            charging,
        }),
    };
    let flags = d[7] >> 4;
    let (lnib, rnib, lchg, rchg) = if flipped {
        (d[6] >> 4, d[6] & 0x0F, flags & 0x02 != 0, flags & 0x01 != 0)
    } else {
        (d[6] & 0x0F, d[6] >> 4, flags & 0x01 != 0, flags & 0x02 != 0)
    };

    Some(Advert {
        model: u16::from_le_bytes([d[3], d[4]]),
        primary_left,
        this_in_case,
        both_in_case,
        in_ear_left,
        in_ear_right,
        lid_closed: (this_in_case || both_in_case).then_some(d[8] & 0x08 != 0),
        lid_counter: d[8] & 0x07,
        color: d[9],
        conn_state: d[10] & 0x0F,
        left: nib(lnib, lchg),
        right: nib(rnib, rchg),
        case: nib(d[7] & 0x0F, flags & 0x04 != 0),
    })
}

/// Precise (1 %) levels from the encrypted tail. None if the key is wrong.
fn decrypt_levels(d: &[u8], key: &[u8; 16], primary_left: bool) -> Option<[Option<Battery>; 3]> {
    let tail: [u8; 16] = d[d.len() - 16..].try_into().ok()?;
    let p = decrypt(key, &tail);
    if p[0] != 0x04 && p[0] != 0x14 {
        return None;
    }
    let lvl = |b: u8| match b {
        0xFF | 0x00 => None,
        b if (b & 0x7F) > 100 => None,
        b => Some(Battery {
            level: b & 0x7F,
            charging: b & 0x80 != 0,
        }),
    };
    let (li, ri) = if primary_left { (1, 2) } else { (2, 1) };
    Some([lvl(p[li]), lvl(p[ri]), lvl(p[3])])
}

#[derive(Default)]
struct LidTracker {
    closed: Option<bool>,
    counter: Option<u8>,
    at: Option<Instant>,
}

impl LidTracker {
    /// Returns Some(open) on an edge. The first advert after the case wakes
    /// still carries the old state, so only a disagreeing later one counts.
    fn feed(&mut self, adv: &Advert) -> Option<bool> {
        let now = Instant::now();
        if self.at.is_some_and(|t| now.duration_since(t) > LID_STALE) {
            *self = Self::default();
        }
        self.at = Some(now);
        let Some(closed) = adv.lid_closed else {
            // No bud in the case: the lid byte is meaningless.
            self.closed = None;
            self.counter = None;
            return None;
        };
        let changed = self.closed.is_some_and(|c| c != closed);
        let bounced = !closed
            && self.closed == Some(false)
            && self.counter.is_some_and(|c| c != adv.lid_counter);
        self.closed = Some(closed);
        self.counter = Some(adv.lid_counter);
        (changed || bounced).then_some(!closed)
    }
}

struct Known {
    devices: HashMap<String, DeviceData>,
    loaded_at: Option<SystemTime>,
}

impl Known {
    fn load() -> Self {
        let path = get_devices_path();
        Self {
            devices: std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            loaded_at: std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
        }
    }

    /// devices.json gains the keys after the first AACP connection; pick that up.
    fn refresh(&mut self) -> bool {
        let m = std::fs::metadata(get_devices_path()).and_then(|m| m.modified()).ok();
        if m != self.loaded_at {
            *self = Self::load();
            return true;
        }
        false
    }

    fn keys(&self) -> impl Iterator<Item = (&String, [u8; 16], Option<[u8; 16]>, &str)> {
        self.devices.iter().filter_map(|(mac, d)| {
            if d.type_ != DeviceType::AirPods {
                return None;
            }
            let Some(DeviceInformation::AirPods(info)) = &d.information else {
                return None;
            };
            let irk: [u8; 16] = hex::decode(&info.le_keys.irk).ok()?.try_into().ok()?;
            let enc = hex::decode(&info.le_keys.enc_key)
                .ok()
                .and_then(|v| v.try_into().ok());
            Some((mac, irk, enc, d.name.as_str()))
        })
    }
}

#[derive(Clone)]
struct Matched {
    mac: String,
    name: String,
    enc_key: Option<[u8; 16]>,
}

pub async fn start_le_monitor(
    tray_handle: Option<ksni::Handle<MyTray>>,
    ui_tx: UnboundedSender<BluetoothUIMessage>,
) -> bluer::Result<()> {
    let session = Session::new().await?;
    let adapter = session.default_adapter().await?;
    if !adapter.is_powered().await.unwrap_or(false) {
        // The supervisor restarts us on the next power-on event.
        std::future::pending::<()>().await;
    }

    let mut known = Known::load();
    let mut verified: HashMap<Address, Matched> = HashMap::new();
    let mut failed: HashSet<Address> = HashSet::new();
    let watching: Arc<Mutex<HashSet<Address>>> = Arc::new(Mutex::new(HashSet::new()));
    let per_device: Arc<Mutex<HashMap<String, DeviceRuntime>>> = Arc::new(Mutex::new(HashMap::new()));

    // Only AirPods proximity-pairing frames: 4C 00 | 07 19.
    let pattern = Pattern {
        data_type: 0xFF,
        start_position: 0,
        content: vec![0x4C, 0x00, 0x07, 0x19],
    };
    let mm = adapter.monitor().await?;
    let mut monitor_handle = mm
        .register(Monitor {
            monitor_type: bluer::monitor::Type::OrPatterns,
            patterns: Some(vec![pattern]),
            ..Default::default()
        })
        .await?;
    info!("LE monitor registered (AirPods proximity frames)");

    // "nearby" decays when the adverts stop.
    tokio::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            hub().update("status", |s| {
                if s.nearby && s.last_seen.is_none_or(|t| t.elapsed() > NEARBY_TIMEOUT) {
                    s.nearby = false;
                    s.lid_open = None;
                    s.other_host = false;
                }
            });
        }
    });

    while let Some(mevt) = monitor_handle.next().await {
        let MonitorEvent::DeviceFound(devid) = mevt else {
            continue;
        };
        let Ok(dev) = adapter.device(devid.device) else {
            continue;
        };
        let addr = dev.address();

        let matched = if let Some(m) = verified.get(&addr) {
            m.clone()
        } else {
            if failed.contains(&addr) {
                if !known.refresh() {
                    continue;
                }
                failed.clear();
            }
            let hit = known.keys().find_map(|(mac, irk, enc, name)| {
                verify_rpa(&addr.to_string(), &irk).then(|| Matched {
                    mac: mac.clone(),
                    name: name.to_string(),
                    enc_key: enc,
                })
            });
            match hit {
                Some(m) => {
                    info!("BLE address {} resolves to our AirPods {}", addr, m.mac);
                    verified.insert(addr, m.clone());
                    m
                }
                None => {
                    debug!("{} is not one of ours", addr);
                    failed.insert(addr);
                    continue;
                }
            }
        };

        if !watching.lock().await.insert(addr) {
            continue; // already streaming this address
        }

        // The data present at discovery time counts too, not just changes.
        if let Ok(Some(md)) = dev.manufacturer_data().await
            && let Some(apple) = md.get(&76)
        {
            handle_frame(apple, &matched, &tray_handle, &ui_tx, &per_device).await;
        }

        let mut events = match dev.events().await {
            Ok(e) => e,
            Err(e) => {
                warn!("Cannot watch {}: {}", addr, e);
                watching.lock().await.remove(&addr);
                continue;
            }
        };
        let tray = tray_handle.clone();
        let ui = ui_tx.clone();
        let rt = per_device.clone();
        let watching = watching.clone();
        tokio::spawn(async move {
            while let Some(ev) = events.next().await {
                if let bluer::DeviceEvent::PropertyChanged(bluer::DeviceProperty::ManufacturerData(md)) = ev
                    && let Some(apple) = md.get(&76)
                {
                    handle_frame(apple, &matched, &tray, &ui, &rt).await;
                }
            }
            watching.lock().await.remove(&addr);
        });
    }
    Ok(())
}

#[derive(Default)]
struct DeviceRuntime {
    lid: LidTracker,
    last_connect_attempt: Option<Instant>,
    connecting: bool,
}

async fn handle_frame(
    apple: &[u8],
    m: &Matched,
    tray: &Option<ksni::Handle<MyTray>>,
    ui_tx: &UnboundedSender<BluetoothUIMessage>,
    rt: &Arc<Mutex<HashMap<String, DeviceRuntime>>>,
) {
    let Some(mut adv) = parse_advert(apple) else {
        return;
    };
    if let Some(key) = &m.enc_key {
        match decrypt_levels(apple, key, adv.primary_left) {
            Some([l, r, c]) => {
                adv.left = l.or(adv.left);
                adv.right = r.or(adv.right);
                adv.case = c.or(adv.case);
            }
            None => debug!("ENC key did not decrypt this frame; using 10 % levels"),
        }
    }
    debug!("advert from {}: {:?}", m.mac, adv);

    let lid_edge = rt
        .lock()
        .await
        .entry(m.mac.clone())
        .or_default()
        .lid
        .feed(&adv);

    let connected = hub().snapshot().connected;
    let event = match lid_edge {
        Some(true) => "lid_open",
        Some(false) => "lid_close",
        None => "status",
    };
    if let Some(open) = lid_edge {
        info!("Case lid {} ({})", if open { "opened" } else { "closed" }, m.mac);
    }
    hub().update(event, |s| {
        s.nearby = true;
        s.last_seen = Some(Instant::now());
        s.mac.get_or_insert_with(|| m.mac.clone());
        if s.name.is_none() && !m.name.is_empty() {
            s.name = Some(m.name.clone());
        }
        s.model_id = Some(adv.model);
        s.primary_left = Some(adv.primary_left);
        s.model = Some(model_name(adv.model).to_string());
        s.other_host = !s.connected && adv.conn_state != 0;
        s.lid_open = adv.lid_closed.map(|c| !c);
        s.in_case.left = Some((adv.both_in_case || (adv.this_in_case && adv.primary_left)) && !adv.in_ear_left);
        s.in_case.right = Some((adv.both_in_case || (adv.this_in_case && !adv.primary_left)) && !adv.in_ear_right);
        if !s.connected {
            // The control channel is authoritative while it is up.
            s.left = adv.left;
            s.right = adv.right;
            s.in_ear.left = Some(adv.in_ear_left);
            s.in_ear.right = Some(adv.in_ear_right);
        }
        if adv.case.is_some() {
            s.case = adv.case;
        }
    });

    if !connected {
        update_tray(tray, &adv).await;
    }
    send_ui_battery(ui_tx, &m.mac, &adv);

    if lid_edge == Some(true) || adv.in_ear_left || adv.in_ear_right {
        maybe_auto_connect(m, &adv, connected, rt).await;
    }
}

async fn maybe_auto_connect(
    m: &Matched,
    adv: &Advert,
    connected: bool,
    rt: &Arc<Mutex<HashMap<String, DeviceRuntime>>>,
) {
    if connected || adv.conn_state != 0 || !AppSettings::load().auto_connect_on_open {
        return;
    }
    {
        let mut map = rt.lock().await;
        let d = map.entry(m.mac.clone()).or_default();
        if d.connecting
            || d
                .last_connect_attempt
                .is_some_and(|t| t.elapsed() < AUTO_CONNECT_BACKOFF)
        {
            return;
        }
        d.connecting = true;
        d.last_connect_attempt = Some(Instant::now());
    }
    info!("AirPods free and case open, connecting to {}", m.mac);
    hub().emit("connecting");
    // bluer's connect() races BlueZ's own reconnect logic; bluetoothctl copes.
    let res = crate::ipc::bluetoothctl("connect", &m.mac).await;
    if let Err(e) = &res {
        info!("Auto-connect to {} failed: {}", m.mac, e);
        hub().emit("connect_failed");
    }
    if let Some(d) = rt.lock().await.get_mut(&m.mac) {
        d.connecting = false;
    }
}

async fn update_tray(tray: &Option<ksni::Handle<MyTray>>, adv: &Advert) {
    let Some(handle) = tray else { return };
    let conv = |b: Option<Battery>| match b {
        None => (None, Some(BatteryStatus::Disconnected)),
        Some(b) => (
            Some(b.level),
            Some(if b.charging {
                BatteryStatus::Charging
            } else {
                BatteryStatus::NotCharging
            }),
        ),
    };
    let (l, ls) = conv(adv.left);
    let (r, rs) = conv(adv.right);
    let (c, cs) = conv(adv.case);
    handle
        .update(move |t: &mut MyTray| {
            t.battery_l = l;
            t.battery_l_status = ls;
            t.battery_r = r;
            t.battery_r_status = rs;
            t.battery_c = c;
            t.battery_c_status = cs;
        })
        .await;
}

/// The window shows the case level too; over AACP the case reports itself as
/// disconnected whenever the buds are outside it.
fn send_ui_battery(ui_tx: &UnboundedSender<BluetoothUIMessage>, mac: &str, adv: &Advert) {
    let info: Vec<BatteryInfo> = [
        (BatteryComponent::Left, adv.left),
        (BatteryComponent::Right, adv.right),
        (BatteryComponent::Case, adv.case),
    ]
    .into_iter()
    .filter_map(|(component, b)| {
        b.map(|b| BatteryInfo {
            component,
            level: b.level,
            status: if b.charging {
                BatteryStatus::Charging
            } else {
                BatteryStatus::NotCharging
            },
        })
    })
    .collect();
    if !info.is_empty() {
        let _ = ui_tx.send(BluetoothUIMessage::AACPUIEvent(
            mac.to_string(),
            AACPEvent::BatteryInfo(info),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adv(hex: &str) -> Advert {
        parse_advert(&hex::decode(hex).unwrap()).unwrap()
    }

    // Captures from AirPods Pro 2 USB-C (podctl test vectors).
    #[test]
    fn both_buds_in_ears() {
        let a = adv("07190124200b778f1100055f5740de21491ea5bcb9b3a07f8e950d");
        assert_eq!(a.model, 0x2024);
        assert!(a.in_ear_left && a.in_ear_right);
        assert_eq!(a.left.map(|b| b.level), Some(70));
        assert_eq!(a.case, None);
        assert_eq!(a.lid_closed, None); // no bud in the case
        assert_eq!(a.conn_state, 5);
    }

    #[test]
    fn case_closing() {
        let a = adv("07190124207577b83a0005be5ec35a37dcf327b8478682685f4bd6");
        assert!(a.both_in_case);
        assert_eq!(a.case.map(|b| b.level), Some(80));
        assert!(a.left.unwrap().charging && a.right.unwrap().charging);
        assert_eq!(a.lid_closed, Some(true));
    }

    #[test]
    fn lid_edge_needs_a_disagreeing_frame() {
        let closed = adv("07190124201487b8580000d0d43874863e0a2b0897ff63d15ff687");
        let mut open = closed;
        open.lid_closed = Some(false);
        open.lid_counter = 1;
        let mut t = LidTracker::default();
        assert_eq!(t.feed(&closed), None); // first frame after wake: stale
        assert_eq!(t.feed(&open), Some(true));
        assert_eq!(t.feed(&open), None);
        assert_eq!(t.feed(&closed), Some(false));
    }

    #[test]
    fn rejects_other_frames() {
        assert!(parse_advert(&[0x10, 0x05, 0x01]).is_none());
        assert!(parse_advert(&[0x07, 0x11, 0x01, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
    }

    #[test]
    fn rpa_spec_sample() {
        // Core spec Vol 3 Part H App. D; the IRK is stored byte-reversed.
        let mut irk: [u8; 16] = hex::decode("ec0234a357c8ad05341010a60a397d9b")
            .unwrap()
            .try_into()
            .unwrap();
        irk.reverse();
        assert!(verify_rpa("70:81:94:0D:FB:AA", &irk));
        assert!(!verify_rpa("F0:81:94:0D:FB:AA", &irk));
    }
}
