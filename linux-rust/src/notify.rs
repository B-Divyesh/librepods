//! Desktop notifications driven by hub events: iOS-style low-battery alerts,
//! a small listening-mode toast (same look as the volume OSD), and a plain
//! "connected" notification when the popup binary is not available.

use crate::hub::{Battery, Event, ListeningMode, Status, hub};
use crate::utils::AppSettings;
use dbus::arg::{PropMap, Variant};
use dbus::blocking::Connection;
use log::{debug, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// Set by main when librepods-popup is running; it then owns the
/// connected/case cards and we only do alerts and toasts.
pub static POPUP_RUNNING: AtomicBool = AtomicBool::new(false);

const APP: &str = "LibrePods";

pub fn icon_path() -> String {
    let data = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
        format!("{}/.local/share", std::env::var("HOME").unwrap_or_default())
    });
    let p = format!("{data}/librepods/airpods.png");
    if std::path::Path::new(&p).exists() {
        p
    } else {
        "audio-headphones".into()
    }
}

struct Note<'a> {
    app: &'a str,
    replaces: u32,
    icon: String,
    summary: String,
    body: String,
    timeout_ms: i32,
    urgency: u8,
    sync_tag: Option<&'a str>,
    value: Option<i32>,
    category: Option<&'a str>,
}

fn send(n: Note<'_>) -> Option<u32> {
    let conn = Connection::new_session().ok()?;
    let proxy = conn.with_proxy(
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        Duration::from_secs(3),
    );
    let mut hints: PropMap = HashMap::new();
    hints.insert("urgency".into(), Variant(Box::new(n.urgency)));
    if let Some(tag) = n.sync_tag {
        hints.insert(
            "x-canonical-private-synchronous".into(),
            Variant(Box::new(tag.to_string())),
        );
    }
    if let Some(v) = n.value {
        hints.insert("value".into(), Variant(Box::new(v)));
    }
    if let Some(c) = n.category {
        hints.insert("category".into(), Variant(Box::new(c.to_string())));
    }
    if n.icon.starts_with('/') {
        hints.insert("image-path".into(), Variant(Box::new(n.icon.clone())));
    }
    let actions: Vec<&str> = vec![];
    let r: Result<(u32,), _> = proxy.method_call(
        "org.freedesktop.Notifications",
        "Notify",
        (
            n.app,
            n.replaces,
            n.icon.as_str(),
            n.summary.as_str(),
            n.body.as_str(),
            actions,
            hints,
            n.timeout_ms,
        ),
    );
    match r {
        Ok((id,)) => Some(id),
        Err(e) => {
            warn!("notification failed: {e}");
            None
        }
    }
}

fn fmt_level(label: &str, b: Option<Battery>) -> Option<String> {
    b.map(|b| {
        format!(
            "{label} {}%{}",
            b.level,
            if b.charging { " ⚡" } else { "" }
        )
    })
}

pub fn battery_line(s: &Status) -> String {
    if s.headphone.is_some() {
        return fmt_level("", s.headphone).unwrap_or_default();
    }
    [
        fmt_level("L", s.left),
        fmt_level("R", s.right),
        fmt_level("Case", s.case),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("   ")
}

#[derive(Default)]
struct LowState {
    warned20: bool,
    warned10: bool,
}

pub async fn run() {
    let mut rx = hub().subscribe();
    let mut low: HashMap<&'static str, LowState> = HashMap::new();
    let mut connected_note: u32 = 0;
    // Never repeat the same card within a minute (reconnect blips, restarts).
    let mut last_shown: HashMap<&'static str, std::time::Instant> = HashMap::new();
    let mut recently = |key: &'static str, secs: u64| -> bool {
        let now = std::time::Instant::now();
        let hit = last_shown
            .get(key)
            .is_some_and(|t| now.duration_since(*t) < Duration::from_secs(secs));
        if !hit {
            last_shown.insert(key, now);
        }
        hit
    };
    loop {
        let ev: Event = match rx.recv().await {
            Ok(ev) => ev,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return,
        };
        let settings = AppSettings::load();
        let s = ev.status.clone();
        match ev.event {
            "connected"
                if settings.connect_popup
                    && !POPUP_RUNNING.load(Ordering::Relaxed)
                    && !recently("connected", 60) =>
            {
                let name = s.name.clone().unwrap_or_else(|| "AirPods".into());
                let body = battery_line(&s);
                let id = tokio::task::spawn_blocking(move || {
                    send(Note {
                        app: APP,
                        replaces: 0,
                        icon: icon_path(),
                        summary: format!("{name} Connected"),
                        body,
                        timeout_ms: 3500,
                        urgency: 1,
                        sync_tag: Some("librepods-connected"),
                        value: None,
                        category: Some("device.added"),
                    })
                })
                .await
                .ok()
                .flatten();
                connected_note = id.unwrap_or(0);
            }
            "mode" | "ca" if settings.mode_toasts && s.connected => {
                let (glyph, text) = if ev.event == "ca" {
                    (
                        "\u{f05cb}",
                        format!(
                            "Conversation Awareness {}",
                            if s.conversational_awareness == Some(true) { "On" } else { "Off" }
                        ),
                    )
                } else {
                    match s.listening_mode {
                        Some(m) => (mode_glyph(m), m.label().to_string()),
                        None => continue,
                    }
                };
                tokio::task::spawn_blocking(move || {
                    send(Note {
                        app: "osd",
                        replaces: 0,
                        icon: String::new(),
                        summary: format!("{glyph}   {text}"),
                        body: String::new(),
                        timeout_ms: 1400,
                        urgency: 0,
                        sync_tag: Some("librepods-mode"),
                        value: None,
                        category: None,
                    })
                });
            }
            "lid_open" | "popup"
                if (settings.case_popup || ev.event == "popup")
                    && !POPUP_RUNNING.load(Ordering::Relaxed)
                    && (ev.event == "popup" || !recently("case", 10)) =>
            {
                let name = s.name.clone().unwrap_or_else(|| "AirPods".into());
                let status = if s.connected { "Connected" } else { "Not connected — middle-click the bar icon to connect" };
                let body = format!("{}\n{}", battery_line(&s), status);
                tokio::task::spawn_blocking(move || {
                    send(Note {
                        app: APP,
                        replaces: 0,
                        icon: icon_path(),
                        summary: name,
                        body,
                        timeout_ms: 6000,
                        urgency: 1,
                        sync_tag: Some("librepods-case"),
                        value: None,
                        category: Some("device"),
                    })
                });
            }
            "connect_failed" => {
                let name = s.name.clone().unwrap_or_else(|| "AirPods".into());
                toast(format!("\u{f1851}   Couldn't connect to {name}"));
            }
            "repair" | "repaired" => {
                // One sticky notification that follows the recovery along
                // (same sync tag), so it is still there if the card was missed.
                use crate::hub::RepairState;
                let name = s.name.clone().unwrap_or_else(|| "AirPods".into());
                let (summary, body, timeout_ms, urgency) = match (ev.event, s.repair) {
                    ("repaired", _) => (
                        format!("{name} paired again"),
                        "All set — they'll connect to this computer as before.".to_string(),
                        5000,
                        1,
                    ),
                    (_, Some(RepairState::Waiting)) => (
                        format!("{name} need to pair again"),
                        "They forgot this computer. Hold the button on the back of the case \
                         until the light flashes white — pairing happens by itself."
                            .to_string(),
                        0,
                        2,
                    ),
                    (_, Some(RepairState::Pairing)) => (
                        format!("Pairing {name}…"),
                        "Keep the case open next to the computer.".to_string(),
                        0,
                        1,
                    ),
                    (_, Some(RepairState::Failed)) => (
                        format!("Couldn't pair {name}"),
                        "Open the case next to the computer, hold the button until the light \
                         flashes white, then click Try Again on the card (or run \
                         `librepods ctl repair`)."
                            .to_string(),
                        0,
                        2,
                    ),
                    // cancelled
                    (_, None) => (format!("{name}: pairing cancelled"), String::new(), 2500, 0),
                };
                tokio::task::spawn_blocking(move || {
                    send(Note {
                        app: APP,
                        replaces: 0,
                        icon: icon_path(),
                        summary,
                        body,
                        timeout_ms,
                        urgency,
                        sync_tag: Some("librepods-repair"),
                        value: None,
                        category: Some("device"),
                    })
                });
            }
            "disconnected" => {
                low.clear();
                let _ = connected_note;
            }
            _ => {}
        }

        if settings.low_battery_alerts && s.connected {
            for (key, label, b) in [
                ("left", "Left AirPod", s.left),
                ("right", "Right AirPod", s.right),
                ("headphone", "AirPods", s.headphone),
            ] {
                let Some(b) = b else { continue };
                let st = low.entry(key).or_default();
                if b.charging || b.level >= 30 {
                    *st = LowState::default();
                    continue;
                }
                let crit = b.level <= 10 && !st.warned10;
                let warn = b.level <= 20 && !st.warned20;
                if !(crit || warn) {
                    continue;
                }
                st.warned20 = true;
                if b.level <= 10 {
                    st.warned10 = true;
                }
                let name = s.name.clone().unwrap_or_else(|| "AirPods".into());
                let body = battery_line(&s);
                debug!("low battery alert for {label}: {}%", b.level);
                tokio::task::spawn_blocking(move || {
                    send(Note {
                        app: APP,
                        replaces: 0,
                        icon: icon_path(),
                        summary: format!("{name}: {label} at {}%", b.level),
                        body: format!("Low battery\n{body}"),
                        timeout_ms: if crit { 0 } else { 8000 },
                        urgency: if crit { 2 } else { 1 },
                        sync_tag: Some("librepods-battery"),
                        value: Some(b.level as i32),
                        category: Some("device"),
                    })
                });
            }
        }
    }
}

/// Small bottom-centre OSD toast, same look as the volume/caffeine ones.
pub fn toast(text: String) {
    tokio::task::spawn_blocking(move || {
        send(Note {
            app: "osd",
            replaces: 0,
            icon: String::new(),
            summary: text,
            body: String::new(),
            timeout_ms: 1500,
            urgency: 0,
            sync_tag: Some("librepods-mode"),
            value: None,
            category: None,
        })
    });
}

fn mode_glyph(m: ListeningMode) -> &'static str {
    match m {
        ListeningMode::Anc => "\u{f0a45}",
        ListeningMode::Transparency => "\u{f07c5}",
        ListeningMode::Adaptive => "\u{f0674}",
        ListeningMode::Off => "\u{f184f}",
    }
}
