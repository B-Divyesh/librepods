//! Handing the AirPods back and forth with your iPhone, without iCloud.
//!
//! Apple's automatic switching only works between devices on the same Apple
//! ID; posing as an Apple device (BlueZ DeviceID 0x004C) gets this computer
//! dropped by current AirPods firmware. As a plain Bluetooth host we do what
//! can be done reliably:
//!
//! * **Take them** when you press play here while wearing them. Playback is
//!   held until they are connected, so nothing comes out of the laptop
//!   speakers, and nothing is taken while the other device is on a call (the
//!   AirPods' BLE advert says what they are doing over there).
//! * **Give them back** when nothing has played through them here for a
//!   while, so your iPhone can reconnect them (with "Connect to This iPhone:
//!   Automatically", or an iOS Shortcut that sets the playback destination).
//!
//! In Apple-vendor-ID mode the AirPods arbitrate between hosts themselves:
//! taking over is then media_controller.rs' job and nothing is given back.

use crate::hub::hub;
use crate::media_controller::MediaController;
use crate::utils::AppSettings;
use log::{debug, info, warn};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a connect made for a takeover may take to come up and still
/// count as asked for (Apple mode: media_controller.rs then takes the audio).
const TAKEOVER_WINDOW: Duration = Duration::from_secs(15);
/// After connecting for playback, how long to wait for the AirPods' sink.
const SINK_WAIT: Duration = Duration::from_secs(6);
const TICK: Duration = Duration::from_secs(1);

static REQUESTED: Mutex<Option<Instant>> = Mutex::new(None);

pub fn takeover_requested() -> bool {
    REQUESTED.lock().unwrap().is_some_and(|t| t.elapsed() < TAKEOVER_WINDOW)
}

pub fn clear_takeover() {
    *REQUESTED.lock().unwrap() = None;
}

/// What the AirPods' advert says they are doing on the device they're
/// connected to (low nibble of the status byte; CAPod's mapping).
pub fn busy_on_a_call(state: u8) -> bool {
    matches!(state, 0x06 | 0x07 | 0x09) // call, ringing, hanging up
}

/// Decides when to give the AirPods back. Pure, so it can be tested.
#[derive(Debug, Default)]
pub struct ReleasePolicy {
    idle_since: Option<Instant>,
}

impl ReleasePolicy {
    /// One observation; `true` means: disconnect now.
    pub fn tick(&mut self, now: Instant, connected: bool, in_use: bool, after: Option<Duration>) -> bool {
        let Some(after) = after else {
            self.idle_since = None;
            return false;
        };
        if !connected || in_use {
            // Not ours to give, or busy: start counting again from here.
            self.idle_since = None;
            return false;
        }
        let since = *self.idle_since.get_or_insert(now);
        if now.duration_since(since) >= after {
            self.idle_since = None;
            return true;
        }
        false
    }
}

/// Whether a stream is running through the AirPods (playback, a call, the
/// hi-res mic). Paused players leave their streams IDLE, then SUSPENDED.
fn audio_in_use(mac: &str) -> bool {
    ["sinks", "sources"].iter().any(|kind| {
        match std::process::Command::new("pactl")
            .args(["--format=json", "list", kind])
            .output()
        {
            Ok(out) => running_through(&out.stdout, mac).unwrap_or(true),
            // can't tell: never pull the AirPods out from under someone
            Err(_) => true,
        }
    })
}

/// `pactl --format=json list sinks|sources` → is one of the AirPods' nodes
/// (or the hi-res mic source) RUNNING? `None` if the JSON is unreadable.
fn running_through(json: &[u8], mac: &str) -> Option<bool> {
    let tag = mac.replace(':', "_").to_ascii_uppercase();
    let nodes = serde_json::from_slice::<Vec<serde_json::Value>>(json).ok()?;
    Some(nodes.iter().any(|n| {
        let name = n["name"].as_str().unwrap_or("");
        (name.to_ascii_uppercase().contains(&tag) || name == crate::audio::output::SOURCE_NAME)
            && !name.ends_with(".monitor")
            && n["state"].as_str() == Some("RUNNING")
    }))
}

fn sink_present(mac: &str) -> bool {
    let tag = mac.replace(':', "_").to_ascii_uppercase();
    std::process::Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_ascii_uppercase().contains(&tag))
        .unwrap_or(false)
}

/// MPRIS players that are playing right now.
fn playing_players() -> Vec<String> {
    use dbus::blocking::Connection;
    use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
    let Ok(conn) = Connection::new_session() else { return vec![] };
    MediaController::list_mpris_services(&conn)
        .into_iter()
        .filter(|s| {
            conn.with_proxy(s.as_str(), "/org/mpris/MediaPlayer2", Duration::from_secs(2))
                .get::<String>("org.mpris.MediaPlayer2.Player", "PlaybackStatus")
                .is_ok_and(|st| st == "Playing")
        })
        .collect()
}

fn mpris_call(players: &[String], method: &str) {
    use dbus::blocking::Connection;
    let Ok(conn) = Connection::new_session() else { return };
    for p in players {
        let proxy = conn.with_proxy(p.as_str(), "/org/mpris/MediaPlayer2", Duration::from_secs(2));
        let r: Result<(), _> = proxy.method_call("org.mpris.MediaPlayer2.Player", method, ());
        if let Err(e) = r {
            debug!("{method} {p}: {e}");
        }
    }
}

/// The adapter says Apple's vendor ID (BlueZ DeviceID override).
async fn apple_mode() -> bool {
    async {
        let session = bluer::Session::new().await.ok()?;
        let adapter = session.default_adapter().await.ok()?;
        let m = adapter.modalias().await.ok()??;
        Some(m.source == "bluetooth" && m.vendor == 0x004C)
    }
    .await
    .unwrap_or(false)
}

/// Pause what's playing, connect, resume once the AirPods can take the audio.
async fn bring_over(mac: &str, apple: bool) {
    let held = tokio::task::spawn_blocking(|| {
        let p = playing_players();
        mpris_call(&p, "Pause");
        p
    })
    .await
    .unwrap_or_default();
    if apple {
        *REQUESTED.lock().unwrap() = Some(Instant::now());
    }
    hub().emit("connecting");
    let result = crate::ipc::bluetoothctl("connect", mac).await;
    if let Err(e) = &result {
        warn!("could not bring the AirPods over: {e}");
        clear_takeover();
        if !crate::repair::running() {
            hub().emit("connect_failed");
        }
    } else {
        let deadline = Instant::now() + SINK_WAIT;
        while Instant::now() < deadline {
            let m = mac.to_string();
            if tokio::task::spawn_blocking(move || sink_present(&m)).await.unwrap_or(false) {
                // let WirePlumber make it the default before audio resumes
                tokio::time::sleep(Duration::from_millis(400)).await;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    tokio::task::spawn_blocking(move || mpris_call(&held, "Play"));
}

pub async fn run() {
    let mut was_playing: Option<bool> = None;
    let mut release = ReleasePolicy::default();
    let mut apple = apple_mode().await;
    let mut apple_checked = Instant::now();
    loop {
        tokio::time::sleep(TICK).await;
        if apple_checked.elapsed() > Duration::from_secs(60) {
            apple = apple_mode().await;
            apple_checked = Instant::now();
        }
        let st = hub().snapshot();
        let settings = AppSettings::load();

        // ── give back ──
        let after = (settings.hand_back_when_idle && !apple)
            .then(|| Duration::from_secs(settings.hand_back_after_secs.max(10) as u64));
        if let Some(mac) = st.mac.clone().filter(|_| st.connected) {
            let m = mac.clone();
            let in_use = tokio::task::spawn_blocking(move || audio_in_use(&m)).await.unwrap_or(true);
            if release.tick(Instant::now(), true, in_use, after) {
                info!("nothing has played through the AirPods for a while: handing them back");
                let name = st.name.clone().unwrap_or_else(|| "AirPods".into());
                if crate::ipc::bluetoothctl("disconnect", &mac).await.is_ok() {
                    crate::notify::toast(format!("\u{f1851}   {name} handed back"));
                }
            }
        } else {
            release.tick(Instant::now(), false, false, after);
        }

        // ── take ──
        let worn = st.in_ear.left == Some(true) || st.in_ear.right == Some(true);
        let eligible = !st.connected
            && st.nearby
            && worn
            && st.bluetooth_on
            && st.repair.is_none()
            && !crate::repair::running();
        if !eligible {
            was_playing = None; // fresh baseline next time they qualify
            continue;
        }
        let playing = tokio::task::spawn_blocking(MediaController::check_if_playing)
            .await
            .unwrap_or(false);
        let started = was_playing == Some(false) && playing;
        was_playing = Some(playing);
        if !started || !settings.media_takeover {
            continue;
        }
        if busy_on_a_call(st.other_state) {
            info!("media started here, but the AirPods are on a call on another device");
            crate::notify::toast("\u{f1851}   AirPods are on a call on another device".into());
            continue;
        }
        let Some(mac) = st.mac.clone() else { continue };
        info!("media started here while the AirPods are worn elsewhere: bringing them over");
        bring_over(&mac, apple).await;
        was_playing = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn gives_back_only_after_a_quiet_stretch() {
        let mut p = ReleasePolicy::default();
        let t = Instant::now();
        let after = Some(120 * S);
        assert!(!p.tick(t, true, false, after), "just connected");
        assert!(!p.tick(t + 119 * S, true, false, after));
        assert!(p.tick(t + 120 * S, true, false, after), "two quiet minutes");
        assert!(!p.tick(t + 121 * S, true, false, after), "fires once");
    }

    #[test]
    fn any_audio_restarts_the_clock() {
        let mut p = ReleasePolicy::default();
        let t = Instant::now();
        let after = Some(60 * S);
        assert!(!p.tick(t, true, false, after));
        assert!(!p.tick(t + 50 * S, true, true, after), "a call or a song");
        assert!(!p.tick(t + 100 * S, true, false, after), "the quiet stretch starts when the audio stops");
        assert!(!p.tick(t + 159 * S, true, false, after));
        assert!(p.tick(t + 160 * S, true, false, after));
    }

    #[test]
    fn off_or_disconnected_never_fires() {
        let mut p = ReleasePolicy::default();
        let t = Instant::now();
        assert!(!p.tick(t, true, false, None));
        assert!(!p.tick(t + 9999 * S, true, false, None), "setting off");
        let mut p = ReleasePolicy::default();
        assert!(!p.tick(t, false, false, Some(10 * S)));
        assert!(!p.tick(t + 60 * S, false, false, Some(10 * S)), "not connected");
        // connecting later starts a fresh count
        assert!(!p.tick(t + 61 * S, true, false, Some(10 * S)));
        assert!(p.tick(t + 71 * S, true, false, Some(10 * S)));
    }

    #[test]
    fn reads_pactl_node_states() {
        let mac = "AC:DE:48:00:11:22";
        let sinks = br#"[
            {"index":55,"state":"RUNNING","name":"alsa_output.pci-0000_00_1f.3.analog-stereo"},
            {"index":71,"state":"IDLE","name":"bluez_output.AC_DE_48_00_11_22.1"}]"#;
        assert_eq!(running_through(sinks, mac), Some(false), "speakers playing, AirPods paused");
        let sinks = br#"[{"index":71,"state":"RUNNING","name":"bluez_output.AC_DE_48_00_11_22.1"}]"#;
        assert_eq!(running_through(sinks, mac), Some(true));
        let sources = br#"[
            {"index":72,"state":"RUNNING","name":"bluez_output.AC_DE_48_00_11_22.1.monitor"},
            {"index":80,"state":"SUSPENDED","name":"bluez_input.AC_DE_48_00_11_22.0"}]"#;
        assert_eq!(running_through(sources, mac), Some(false), "a monitor is not a call");
        let sources = br#"[{"index":90,"state":"RUNNING","name":"AirPodsHiRes"}]"#;
        assert_eq!(running_through(sources, mac), Some(true), "hi-res mic in use");
        assert_eq!(running_through(b"garbage", mac), None);
        assert_eq!(running_through(b"[]", mac), Some(false));
    }

    #[test]
    fn calls_on_the_other_device_are_left_alone() {
        assert!(busy_on_a_call(0x06));
        assert!(busy_on_a_call(0x07));
        assert!(busy_on_a_call(0x09));
        assert!(!busy_on_a_call(0x04), "idle");
        assert!(!busy_on_a_call(0x05), "music: playing here takes them, like a Mac");
        assert!(!busy_on_a_call(0x00));
    }
}
