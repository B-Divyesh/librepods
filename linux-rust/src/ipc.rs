//! Control socket at `$XDG_RUNTIME_DIR/librepods.sock` and the `librepods ctl`
//! client that talks to it. Line-based: one command per line in, JSON lines out.
//!
//!   status                      current state
//!   watch                       current state, then one line per change
//!   mode off|anc|transparency|adaptive|next
//!   ca on|off|toggle            conversational awareness
//!   connect | disconnect | toggle
//!   open                        show the LibrePods window
//!   popup                       show the case popup now

use crate::bluetooth::aacp::ControlCommandIdentifiers;
use crate::hub::{Battery, Event, ListeningMode, Status, hub};
use crate::ui::messages::BluetoothUIMessage;
use log::{debug, info, warn};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast::error::RecvError;

pub fn socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc_getuid() }));
    PathBuf::from(dir).join("librepods.sock")
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

pub async fn serve() {
    let path = socket_path();
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        warn!("Another LibrePods instance owns {}, IPC disabled", path.display());
        return;
    }
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            warn!("Cannot bind {}: {}", path.display(), e);
            return;
        }
    };
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    info!("IPC listening on {}", path.display());
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle(stream).await {
                        debug!("IPC client ended: {}", e);
                    }
                });
            }
            Err(e) => {
                warn!("IPC accept failed: {}", e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

fn ok() -> String {
    json!({"ok": true}).to_string()
}

fn err(e: impl ToString) -> String {
    json!({"ok": false, "error": e.to_string()}).to_string()
}

async fn handle(stream: UnixStream) -> std::io::Result<()> {
    let (r, mut w) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        let mut args = line.split_whitespace();
        let cmd = args.next().unwrap_or("");
        let arg = args.next().unwrap_or("");
        if cmd == "watch" {
            // `watch popup`: the popup gets lid/connect events only when the
            // user has those cards enabled.
            let for_popup = arg == "popup";
            let mut rx = hub().subscribe();
            let first = Event {
                event: "status",
                anchor_x: None,
                status: hub().snapshot(),
            };
            w.write_all(format!("{}\n", serde_json::to_string(&first)?).as_bytes())
                .await?;
            loop {
                let mut ev = match rx.recv().await {
                    Ok(ev) => ev,
                    Err(RecvError::Lagged(_)) => Event {
                        event: "status",
                        anchor_x: None,
                        status: hub().snapshot(),
                    },
                    Err(RecvError::Closed) => return Ok(()),
                };
                if for_popup {
                    let st = crate::utils::AppSettings::load();
                    if (ev.event == "lid_open" && !st.case_popup)
                        || (ev.event == "connected" && !st.connect_popup)
                    {
                        ev.event = "status";
                    }
                }
                w.write_all(format!("{}\n", serde_json::to_string(&ev)?).as_bytes())
                    .await?;
            }
        }
        let reply = run(cmd, arg).await;
        w.write_all(format!("{}\n", reply).as_bytes()).await?;
    }
    Ok(())
}

async fn run(cmd: &str, arg: &str) -> String {
    let st = hub().snapshot();
    match cmd {
        "status" => serde_json::to_string(&st).unwrap_or_else(err),
        "mode" => {
            let mode = if arg == "next" || arg.is_empty() {
                Some(st.next_mode())
            } else {
                ListeningMode::parse(arg)
            };
            let Some(mode) = mode else {
                return err(format!("unknown mode '{arg}'"));
            };
            if mode == ListeningMode::Off && st.allow_off != Some(true) {
                // Off has to be allowed first, as on iOS.
                let _ = hub().send_command(ControlCommandIdentifiers::AllowOffOption, vec![0x01]);
            }
            match hub().send_command(ControlCommandIdentifiers::ListeningMode, vec![mode.to_byte()]) {
                Ok(()) => {
                    // Optimistic update so the toast / bar react instantly; the
                    // AirPods echo the real value right after.
                    hub().update("mode", |s| s.listening_mode = Some(mode));
                    json!({"ok": true, "mode": mode}).to_string()
                }
                Err(e) => err(e),
            }
        }
        "ca" | "conversation" => {
            let on = match arg {
                "on" | "1" | "true" => true,
                "off" | "0" | "false" => false,
                _ => !st.conversational_awareness.unwrap_or(false),
            };
            match hub().send_command(
                ControlCommandIdentifiers::ConversationDetectConfig,
                vec![if on { 0x01 } else { 0x02 }],
            ) {
                Ok(()) => {
                    hub().update("ca", |s| s.conversational_awareness = Some(on));
                    json!({"ok": true, "conversational_awareness": on}).to_string()
                }
                Err(e) => err(e),
            }
        }
        "connect" | "disconnect" | "toggle" => {
            let mac = match st.mac.clone().or_else(default_airpods_mac) {
                Some(m) => Some(m),
                None => paired_airpods().await,
            };
            let Some(mac) = mac else {
                return err("no paired AirPods found");
            };
            let action = match cmd {
                "toggle" if st.connected => "disconnect",
                "toggle" => "connect",
                c => c,
            };
            if action == "connect" {
                hub().emit("connecting");
            }
            match bluetoothctl(action, &mac).await {
                Ok(()) => {
                    if action == "disconnect" {
                        let name = st.name.clone().unwrap_or_else(|| "AirPods".into());
                        crate::notify::toast(format!("{G_EARBUDS_OFF}   {name} disconnected"));
                    }
                    ok()
                }
                Err(e) => {
                    if action == "connect" {
                        hub().emit("connect_failed");
                    }
                    err(e)
                }
            }
        }
        "bluetooth" | "bt" => {
            let on = match arg {
                "on" => true,
                "off" => false,
                _ => !st.bluetooth_on,
            };
            match set_bluetooth(on).await {
                Ok(()) => json!({"ok": true, "bluetooth": on}).to_string(),
                Err(e) => err(e),
            }
        }
        "open" => match hub().send_ui(BluetoothUIMessage::OpenWindow) {
            Ok(()) => ok(),
            Err(e) => err(e),
        },
        "popup" => {
            hub().emit("popup");
            ok()
        }
        "menu" => {
            // The dropdown lives in librepods-popup; without it, open the window.
            if crate::notify::POPUP_RUNNING.load(std::sync::atomic::Ordering::Relaxed) {
                hub().emit_menu(arg.parse().ok());
                ok()
            } else {
                match hub().send_ui(BluetoothUIMessage::OpenWindow) {
                    Ok(()) => ok(),
                    Err(e) => err(e),
                }
            }
        }
        "help" | "" => json!({"commands": [
            "status", "watch", "mode off|anc|transparency|adaptive|next", "ca on|off|toggle",
            "bluetooth on|off|toggle",
            "connect", "disconnect", "toggle", "bluetooth on|off|toggle", "open", "popup", "menu [x]"]})
        .to_string(),
        other => err(format!("unknown command '{other}'")),
    }
}

/// First AirPods in devices.json — used when we have not seen them this session.
fn default_airpods_mac() -> Option<String> {
    let json = std::fs::read_to_string(crate::utils::get_devices_path()).ok()?;
    let map: std::collections::HashMap<String, crate::devices::enums::DeviceData> =
        serde_json::from_str(&json).ok()?;
    map.into_iter()
        .find(|(_, d)| d.type_ == crate::devices::enums::DeviceType::AirPods)
        .map(|(mac, _)| mac)
}

/// Power the adapter on/off; turning on also lifts an rfkill soft block.
pub async fn set_bluetooth(on: bool) -> Result<(), String> {
    if on {
        let _ = tokio::process::Command::new("rfkill")
            .args(["unblock", "bluetooth"])
            .status()
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let session = bluer::Session::new().await.map_err(|e| e.to_string())?;
    let adapter = session.default_adapter().await.map_err(|e| e.to_string())?;
    adapter.set_powered(on).await.map_err(|e| e.to_string())
}

/// Any paired device exposing Apple's AACP service.
async fn paired_airpods() -> Option<String> {
    let aacp = uuid::Uuid::parse_str("74ec2172-0bad-4d01-8f77-997b2be0722a").ok()?;
    let session = bluer::Session::new().await.ok()?;
    let adapter = session.default_adapter().await.ok()?;
    for addr in adapter.device_addresses().await.ok()? {
        let Ok(dev) = adapter.device(addr) else { continue };
        if dev.is_paired().await.unwrap_or(false)
            && dev
                .uuids()
                .await
                .ok()
                .flatten()
                .is_some_and(|u| u.contains(&aacp))
        {
            return Some(addr.to_string());
        }
    }
    None
}

pub async fn bluetoothctl(action: &str, mac: &str) -> Result<(), String> {
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new("bluetoothctl")
            .arg(action)
            .arg(mac)
            .output(),
    )
    .await
    .map_err(|_| format!("{action} timed out"))?
    .map_err(|e| format!("bluetoothctl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string()
        + &String::from_utf8_lossy(&out.stderr);
    if out.status.success() && !text.contains("Failed") {
        Ok(())
    } else {
        Err(text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("failed")
            .trim()
            .to_string())
    }
}

// ───────────────────────────── client side ─────────────────────────────

pub fn run_client(args: &[String]) -> i32 {
    let cmd = args.first().map(String::as_str).unwrap_or("status");
    match cmd {
        "waybar" => waybar_loop(),
        "menu" => {
            // Hang the menu under the pointer (the bar icon that was clicked).
            let x = std::process::Command::new("hyprctl")
                .arg("cursorpos")
                .output()
                .ok()
                .and_then(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .split(',')
                        .next()
                        .and_then(|v| v.trim().parse::<f64>().ok())
                })
                .map(|v| v.round() as i32);
            let Some(stream) = connect_or_report() else { return 1 };
            let mut w = &stream;
            let cmd = match x {
                Some(x) => format!("menu {x}\n"),
                None => "menu\n".to_string(),
            };
            let _ = w.write_all(cmd.as_bytes());
            let mut line = String::new();
            let _ = BufReader::new(&stream).read_line(&mut line);
            0
        }
        "watch" => {
            let Some(stream) = connect_or_report() else { return 1 };
            let mut w = &stream;
            let _ = w.write_all(b"watch\n");
            for line in BufReader::new(&stream).lines().map_while(Result::ok) {
                println!("{line}");
            }
            0
        }
        _ => {
            let Some(stream) = connect_or_report() else { return 1 };
            let mut w = &stream;
            let _ = w.write_all(format!("{}\n", args.join(" ")).as_bytes());
            let mut line = String::new();
            let _ = BufReader::new(&stream).read_line(&mut line);
            let v: serde_json::Value = serde_json::from_str(&line).unwrap_or(json!({}));
            if cmd == "status" {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or(line));
                return 0;
            }
            if v.get("ok") == Some(&json!(false)) {
                eprintln!(
                    "librepods: {}",
                    v.get("error").and_then(|e| e.as_str()).unwrap_or("failed")
                );
                return 1;
            }
            println!("{}", line.trim());
            0
        }
    }
}

fn connect_or_report() -> Option<std::os::unix::net::UnixStream> {
    match std::os::unix::net::UnixStream::connect(socket_path()) {
        Ok(s) => Some(s),
        Err(_) if std::env::args().any(|a| a == "open") => {
            // Launched from the app menu while not running: start the service
            // and wait for its socket.
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "start", "librepods.service"])
                .status();
            for _ in 0..50 {
                std::thread::sleep(Duration::from_millis(100));
                if let Ok(s) = std::os::unix::net::UnixStream::connect(socket_path()) {
                    std::thread::sleep(Duration::from_millis(400));
                    return Some(s);
                }
            }
            eprintln!("librepods: could not start the service");
            None
        }
        Err(_) => {
            eprintln!("librepods: not running (no {})", socket_path().display());
            None
        }
    }
}

/// Streams Waybar JSON forever; reconnects when LibrePods restarts.
fn waybar_loop() -> i32 {
    let mut printed_offline = false;
    loop {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(socket_path()) {
            printed_offline = false;
            let mut w = &stream;
            if w.write_all(b"watch\n").is_ok() {
                for line in BufReader::new(&stream).lines().map_while(Result::ok) {
                    if let Ok(ev) = serde_json::from_str::<serde_json::Value>(&line) {
                        println!("{}", waybar_json(&ev));
                        let _ = std::io::stdout().flush();
                    }
                }
            }
        }
        if !printed_offline {
            println!(r#"{{"text":"","tooltip":"","class":"offline"}}"#);
            let _ = std::io::stdout().flush();
            printed_offline = true;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

const G_EARBUDS: &str = "\u{f184f}";
const G_EARBUDS_OUTLINE: &str = "\u{f1852}";
const G_EARBUDS_OFF: &str = "\u{f1851}";
const G_HEADPHONES: &str = "\u{f02cb}";
const G_ANC: &str = "\u{f0a45}";
const G_TRANSPARENCY: &str = "\u{f07c5}";
const G_ADAPTIVE: &str = "\u{f0674}";
const G_BOLT: &str = "\u{f140b}";

fn bar(level: u8) -> String {
    let filled = (level as usize + 5) / 10;
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled.min(10)))
}

fn waybar_json(v: &serde_json::Value) -> String {
    let st: WireStatus = serde_json::from_value(v.clone()).unwrap_or_default();
    let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("status");
    let is_max = matches!(st.model_id, Some(0x200A | 0x201F));
    let mut classes: Vec<&str> = Vec::new();

    let min = st
        .headphone
        .map(|b| b.level)
        .or_else(|| [st.left, st.right].iter().flatten().map(|b| b.level).min());
    let charging = [st.left, st.right, st.headphone]
        .iter()
        .flatten()
        .any(|b| b.charging);

    if !st.bluetooth_on {
        // The Bluetooth module already shows "off"; stay out of the way.
        return json!({"text": "", "tooltip": "", "class": ["bt-off"]}).to_string();
    }
    let (glyph, text) = if st.connected {
        classes.push("connected");
        let g = if is_max { G_HEADPHONES } else { G_EARBUDS };
        let mode = match st.listening_mode.as_deref() {
            Some("anc") => {
                classes.push("anc");
                format!(" {G_ANC}")
            }
            Some("transparency") => {
                classes.push("transparency");
                format!(" {G_TRANSPARENCY}")
            }
            Some("adaptive") => {
                classes.push("adaptive");
                format!(" {G_ADAPTIVE}")
            }
            Some("off") => {
                classes.push("mode-off");
                String::new()
            }
            _ => String::new(),
        };
        let pct = match min {
            Some(l) => format!(" {l}%"),
            None => String::new(),
        };
        let bolt = if charging { format!(" {G_BOLT}") } else { String::new() };
        (g, format!("{g}{pct}{bolt}{mode}"))
    } else if matches!(event, "connecting" | "lid_open") {
        classes.push("connecting");
        (G_EARBUDS_OUTLINE, G_EARBUDS_OUTLINE.to_string())
    } else if st.nearby {
        classes.push("nearby");
        (G_EARBUDS_OUTLINE, G_EARBUDS_OUTLINE.to_string())
    } else {
        classes.push("disconnected");
        (G_EARBUDS_OFF, G_EARBUDS_OFF.to_string())
    };
    let _ = glyph;

    if charging {
        classes.push("charging");
    }
    if let Some(l) = min
        && !charging
    {
        if l <= 10 {
            classes.push("critical");
        } else if l <= 20 {
            classes.push("low");
        }
    }
    if st.in_ear.left == Some(true) || st.in_ear.right == Some(true) {
        classes.push("in-ear");
    }
    if st.lid_open == Some(true) {
        classes.push("lid-open");
    }

    // Tooltip: "Name  ·  state" header like the other bar modules.
    let name = st.name.clone().unwrap_or_else(|| "AirPods".into());
    let state = if st.connected {
        "Connected"
    } else if st.other_host {
        "On another device"
    } else if st.nearby {
        "Nearby"
    } else {
        "Not connected"
    };
    let mut tip = format!("<b>{}</b>  ·  {}", glib_escape(&name), state);
    if let Some(model) = &st.model
        && *model != name
    {
        tip += &format!("\n<span alpha='60%'>{}</span>", glib_escape(model));
    }
    tip += "\n";
    let row = |label: &str, b: Option<Battery>| -> String {
        match b {
            Some(b) => format!(
                "\n<tt>{label:<5} {} {:>3}%</tt>{}",
                bar(b.level),
                b.level,
                if b.charging { format!(" {G_BOLT}") } else { String::new() }
            ),
            None => String::new(),
        }
    };
    if is_max {
        tip += &row("Max", st.headphone);
    } else {
        tip += &row("Left", st.left);
        tip += &row("Right", st.right);
        tip += &row("Case", st.case);
    }
    if st.left.is_none() && st.right.is_none() && st.headphone.is_none() {
        tip += "\n<span alpha='60%'>battery unknown — open the case nearby</span>";
    }
    tip += "\n";
    if st.connected {
        if let Some(m) = st.listening_mode.as_deref().and_then(ListeningMode::parse) {
            tip += &format!("\n{}  {}", mode_glyph(m), m.label());
        }
        if let Some(ca) = st.conversational_awareness {
            tip += &format!(
                "\nConversation Awareness  {}",
                if ca { "On" } else { "Off" }
            );
        }
        let ear = |b: Option<bool>| match b {
            Some(true) => "in ear",
            Some(false) => "out",
            None => "?",
        };
        if !is_max {
            tip += &format!(
                "\n<span alpha='60%'>L {} · R {}</span>",
                ear(st.in_ear.left),
                ear(st.in_ear.right)
            );
        }
        tip += "\n\n<span alpha='60%'>click: menu · right: next mode · middle: disconnect</span>";
    } else if st.other_host {
        tip += "\n<span alpha='60%'>click: menu · middle: take over</span>";
    } else {
        tip += "\n<span alpha='60%'>click: menu · middle: connect</span>";
    }

    json!({
        "text": text,
        "alt": st.listening_mode.clone().unwrap_or_default(),
        "tooltip": tip,
        "class": classes,
        "percentage": min.unwrap_or(0),
    })
    .to_string()
}

fn mode_glyph(m: ListeningMode) -> &'static str {
    match m {
        ListeningMode::Anc => G_ANC,
        ListeningMode::Transparency => G_TRANSPARENCY,
        ListeningMode::Adaptive => G_ADAPTIVE,
        ListeningMode::Off => G_EARBUDS,
    }
}

fn glib_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Client-side view of `hub::Status` (the client never links against the hub).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct WireStatus {
    connected: bool,
    bluetooth_on: bool,
    nearby: bool,
    #[serde(alias = "connected_elsewhere")]
    other_host: bool,
    name: Option<String>,
    model: Option<String>,
    model_id: Option<u16>,
    left: Option<Battery>,
    right: Option<Battery>,
    case: Option<Battery>,
    headphone: Option<Battery>,
    in_ear: WirePair,
    lid_open: Option<bool>,
    listening_mode: Option<String>,
    conversational_awareness: Option<bool>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct WirePair {
    left: Option<bool>,
    right: Option<bool>,
}

impl<'de> serde::Deserialize<'de> for Battery {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct B {
            level: u8,
            charging: bool,
        }
        let b = B::deserialize(d)?;
        Ok(Battery {
            level: b.level,
            charging: b.charging,
        })
    }
}

#[allow(dead_code)]
fn _assert_status_serializes(s: &Status) -> String {
    serde_json::to_string(s).unwrap_or_default()
}
