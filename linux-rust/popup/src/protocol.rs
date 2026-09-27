//! The LibrePods daemon's JSON event stream and the device model built
//! from it.
//!
//! One JSON object per line. Every line carries the full device state plus
//! an `event` naming what just happened, but the parser only overwrites
//! the keys that are present, so partial objects work too. A key present
//! with `null` means "unknown" and does clear the old value.

use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Batt {
    /// 0–100, `None` when the level is unknown.
    pub level: Option<u8>,
    pub charging: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Device {
    pub connected: bool,
    pub nearby: bool,
    pub name: Option<String>,
    pub model: Option<String>,
    pub model_id: Option<u32>,
    pub left: Option<Batt>,
    pub right: Option<Batt>,
    pub case: Option<Batt>,
    pub headphone: Option<Batt>,
    pub lid_open: Option<bool>,
    pub listening_mode: Option<String>,
    pub conversational_awareness: Option<bool>,
    /// The model lets you pick "Off" as a listening mode.
    pub allow_off: Option<bool>,
    /// (left, right) in an ear.
    pub in_ear: Option<(bool, bool)>,
    /// The daemon says the AirPods are connected to another device.
    pub elsewhere: Option<bool>,
}

/// The events the daemon sends. Unknown names are kept verbatim so they
/// still refresh the card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Status,
    LidOpen,
    LidClose,
    Connected,
    Disconnected,
    Connecting,
    ConnectFailed,
    Mode,
    LowBattery,
    Popup,
    /// Toggle the bar dropdown; carries `anchor_x`.
    Menu,
    Other(String),
}

impl Event {
    fn parse(s: &str) -> Self {
        match s {
            "status" | "" => Event::Status,
            "lid_open" => Event::LidOpen,
            "lid_close" => Event::LidClose,
            "connected" => Event::Connected,
            "disconnected" => Event::Disconnected,
            "connecting" => Event::Connecting,
            "connect_failed" => Event::ConnectFailed,
            "mode" => Event::Mode,
            "low_battery" => Event::LowBattery,
            "popup" => Event::Popup,
            "menu" => Event::Menu,
            other => Event::Other(other.to_string()),
        }
    }
}

/// Parse one line into its event and the object to apply. `None` for
/// blank or malformed lines, which the caller skips.
pub fn parse_line(line: &str) -> Option<(Event, Map<String, Value>)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let Value::Object(obj) = serde_json::from_str::<Value>(line).ok()? else {
        return None;
    };
    let ev = Event::parse(obj.get("event").and_then(Value::as_str).unwrap_or(""));
    Some((ev, obj))
}

impl Device {
    pub fn apply(&mut self, obj: &Map<String, Value>) {
        for (k, v) in obj {
            match k.as_str() {
                "connected" => self.connected = v.as_bool().unwrap_or(false),
                "nearby" => self.nearby = v.as_bool().unwrap_or(false),
                "name" => self.name = nonempty(v),
                "model" => self.model = nonempty(v),
                "model_id" => self.model_id = v.as_u64().and_then(|n| u32::try_from(n).ok()),
                "left" => self.left = batt(v),
                "right" => self.right = batt(v),
                "case" => self.case = batt(v),
                "headphone" => self.headphone = batt(v),
                "lid_open" => self.lid_open = v.as_bool(),
                "listening_mode" => self.listening_mode = nonempty(v),
                "conversational_awareness" => self.conversational_awareness = v.as_bool(),
                "allow_off" => self.allow_off = v.as_bool(),
                "in_ear" => {
                    self.in_ear = v.as_object().map(|o| {
                        let f = |k: &str| o.get(k).and_then(Value::as_bool).unwrap_or(false);
                        (f("left"), f("right"))
                    })
                }
                "connected_elsewhere" | "elsewhere" | "other_device" => self.elsewhere = v.as_bool(),
                _ => {}
            }
        }
    }

    /// Card title: the device name, typeset (Div's → Div’s).
    pub fn title(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.model.clone())
            .unwrap_or_else(|| "AirPods".into())
            .replace('\'', "\u{2019}")
    }

    pub fn product(&self) -> Product {
        Product::identify(self.model_id, self.model.as_deref(), self.name.as_deref())
    }

    /// Where the AirPods are, for the menu header.
    pub fn link_state(&self) -> LinkState {
        if self.connected {
            LinkState::Connected
        } else if self.elsewhere == Some(true)
            || (self.elsewhere.is_none() && self.nearby && self.in_ear.is_some_and(|(l, r)| l || r))
        {
            // Worn but not connected here: they're playing from the phone.
            LinkState::Elsewhere
        } else {
            LinkState::NotConnected
        }
    }

    pub fn mode(&self) -> Option<Mode> {
        self.listening_mode.as_deref().and_then(Mode::parse)
    }

    /// Listening modes this model offers, in menu order.
    pub fn modes(&self) -> Vec<Mode> {
        let mut v = Vec::with_capacity(4);
        if self.allow_off == Some(true) {
            v.push(Mode::Off);
        }
        v.push(Mode::Anc);
        v.push(Mode::Transparency);
        // Adaptive: AirPods Pro 2 (Lightning/USB-C), Pro 3, AirPods 4 ANC.
        if matches!(self.model_id, None | Some(0x2014 | 0x2024 | 0x2027 | 0x201B)) {
            v.push(Mode::Adaptive);
        }
        v
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkState {
    Connected,
    Elsewhere,
    NotConnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Off,
    Anc,
    Transparency,
    Adaptive,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.to_ascii_lowercase().as_str() {
            "off" | "none" => Some(Mode::Off),
            "anc" | "nc" | "noise_cancellation" | "noise-cancellation" | "noisecancellation" => Some(Mode::Anc),
            "transparency" | "transparent" => Some(Mode::Transparency),
            "adaptive" => Some(Mode::Adaptive),
            _ => None,
        }
    }

    /// Argument of the daemon's `mode` command.
    pub fn wire(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Anc => "anc",
            Mode::Transparency => "transparency",
            Mode::Adaptive => "adaptive",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Off => "Off",
            Mode::Anc => "Noise Cancellation",
            Mode::Transparency => "Transparency",
            Mode::Adaptive => "Adaptive",
        }
    }
}

fn nonempty(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `{"level": 80, "charging": false}`; a bare number is accepted as a
/// level. Out-of-range levels (the AAP "unknown" sentinel is often 0xFF
/// or -1) read as unknown.
fn batt(v: &Value) -> Option<Batt> {
    let level_of = |x: &Value| {
        x.as_f64()
            .filter(|l| (0.0..=100.0).contains(l))
            .map(|l| l.round() as u8)
    };
    match v {
        Value::Object(o) => {
            let level = o.get("level").and_then(level_of);
            let charging = o.get("charging").and_then(Value::as_bool).unwrap_or(false);
            if level.is_none() && !charging {
                None
            } else {
                Some(Batt { level, charging })
            }
        }
        Value::Number(_) => level_of(v).map(|l| Batt {
            level: Some(l),
            charging: false,
        }),
        _ => None,
    }
}

/// Which illustration to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Product {
    /// AirPods Pro 1/2/3: silicone tips, short stems, landscape case.
    Pro,
    /// AirPods 3 / 4: open fit, short stems, compact case.
    Open,
    /// AirPods 1 / 2: long stems, portrait case.
    Classic,
    /// AirPods Max: over-ear headphones, one battery.
    Max,
}

impl Product {
    pub fn identify(model_id: Option<u32>, model: Option<&str>, name: Option<&str>) -> Self {
        match model_id {
            Some(0x200E | 0x2014 | 0x2024 | 0x2027) => return Product::Pro,
            Some(0x2013 | 0x2019 | 0x201B) => return Product::Open,
            Some(0x200F | 0x2002) => return Product::Classic,
            Some(0x200A | 0x201F) => return Product::Max,
            _ => {}
        }
        // Unknown id (or a newer one): guess from the strings.
        let text = format!("{} {}", model.unwrap_or(""), name.unwrap_or("")).to_lowercase();
        if text.contains("max") {
            Product::Max
        } else if text.contains("pro") {
            Product::Pro
        } else if text.contains("airpods 4")
            || text.contains("airpods 3")
            || text.contains("(3rd")
            || text.contains("(4th")
        {
            Product::Open
        } else if text.contains("airpods 2") || text.contains("airpods 1") || text.contains("(2nd") {
            Product::Classic
        } else {
            // Generic buds for everything else.
            Product::Open
        }
    }
}

/// One battery ring on the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RingId {
    Left,
    Right,
    Buds,
    Case,
    Headphones,
}

impl RingId {
    pub fn label(self) -> &'static str {
        match self {
            RingId::Left => "Left",
            RingId::Right => "Right",
            RingId::Buds => "AirPods",
            RingId::Case => "Case",
            RingId::Headphones => "Battery",
        }
    }
}

/// Buds within this many percent of each other share one ring, as on iOS.
pub const MERGE_PCT: u8 = 5;

/// The rings to show for the current state, left to right.
pub fn rings(dev: &Device) -> Vec<(RingId, Batt)> {
    if dev.product() == Product::Max || dev.headphone.is_some() {
        let b = dev.headphone.or(dev.left).or(dev.right).unwrap_or_default();
        return vec![(RingId::Headphones, b)];
    }
    let mut out = Vec::with_capacity(3);
    match (dev.left, dev.right) {
        (None, None) => out.push((RingId::Buds, Batt::default())),
        (Some(l), Some(r))
            if l.charging == r.charging
                && matches!((l.level, r.level), (Some(a), Some(b)) if a.abs_diff(b) <= MERGE_PCT) =>
        {
            // Show the lower of the two, never flattering.
            let level = l.level.min(r.level);
            out.push((
                RingId::Buds,
                Batt {
                    level,
                    charging: l.charging,
                },
            ));
        }
        (l, r) => {
            out.push((RingId::Left, l.unwrap_or_default()));
            out.push((RingId::Right, r.unwrap_or_default()));
        }
    }
    out.push((RingId::Case, dev.case.unwrap_or_default()));
    out
}

/// A believable device for `--demo` and `--dump`.
pub fn demo_device(product: Option<&str>) -> Device {
    let (name, model, id) = match product.unwrap_or("pro").to_ascii_lowercase().as_str() {
        "max" => ("AirPods Max", "AirPods Max (USB-C)", 0x201F),
        "4" | "airpods4" | "open" => ("AirPods 4", "AirPods 4 (ANC)", 0x201B),
        "3" | "airpods3" => ("AirPods", "AirPods (3rd generation)", 0x2013),
        "2" | "1" | "classic" => ("AirPods", "AirPods (2nd generation)", 0x200F),
        "pro1" => ("AirPods Pro", "AirPods Pro", 0x200E),
        "pro3" => ("AirPods Pro", "AirPods Pro 3", 0x2027),
        other => match u32::from_str_radix(other.trim_start_matches("0x"), 16) {
            Ok(id) if other.starts_with("0x") => ("AirPods", "AirPods", id),
            _ => ("AirPods Pro", "AirPods Pro 2 (USB-C)", 0x2024),
        },
    };
    let mut dev = Device {
        connected: false,
        nearby: true,
        name: Some(name.into()),
        model: Some(model.into()),
        model_id: Some(id),
        left: Some(Batt {
            level: Some(92),
            charging: false,
        }),
        right: Some(Batt {
            level: Some(64),
            charging: false,
        }),
        case: Some(Batt {
            level: Some(38),
            charging: true,
        }),
        headphone: None,
        lid_open: Some(true),
        listening_mode: Some("anc".into()),
        conversational_awareness: Some(true),
        allow_off: Some(false),
        in_ear: Some((false, false)),
        elsewhere: None,
    };
    if dev.product() == Product::Max {
        dev.left = None;
        dev.right = None;
        dev.case = None;
        dev.headphone = Some(Batt {
            level: Some(73),
            charging: true,
        });
    }
    dev
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"event":"status","connected":false,"nearby":true,"name":"AirPods Pro","model":"AirPods Pro 2 (USB-C)","model_id":8228,"mac":"AC:DE:48:00:11:22","left":{"level":80,"charging":false},"right":{"level":78,"charging":false},"case":{"level":55,"charging":true},"headphone":null,"in_ear":{"left":false,"right":false},"in_case":{"left":true,"right":true},"lid_open":true,"listening_mode":"anc","conversational_awareness":true,"auto_connect":true}"#;

    #[test]
    fn parses_sample() {
        let (ev, obj) = parse_line(SAMPLE).unwrap();
        assert_eq!(ev, Event::Status);
        let mut d = Device::default();
        d.apply(&obj);
        assert!(!d.connected);
        assert_eq!(d.model_id, Some(8228));
        assert_eq!(d.product(), Product::Pro);
        assert_eq!(d.left.unwrap().level, Some(80));
        assert!(d.case.unwrap().charging);
        assert_eq!(d.headphone, None);
        assert_eq!(d.title(), "AirPods Pro");
    }

    #[test]
    fn close_buds_merge() {
        let (_, obj) = parse_line(SAMPLE).unwrap();
        let mut d = Device::default();
        d.apply(&obj);
        let r = rings(&d);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, RingId::Buds);
        assert_eq!(r[0].1.level, Some(78));
        assert_eq!(r[1].0, RingId::Case);
    }

    #[test]
    fn distant_buds_split() {
        let mut d = demo_device(None);
        d.left = Some(Batt {
            level: Some(90),
            charging: false,
        });
        d.right = Some(Batt {
            level: Some(40),
            charging: false,
        });
        let ids: Vec<_> = rings(&d).into_iter().map(|r| r.0).collect();
        assert_eq!(ids, [RingId::Left, RingId::Right, RingId::Case]);
    }

    #[test]
    fn nulls_clear_and_partial_lines_keep() {
        let mut d = demo_device(None);
        let (_, obj) = parse_line(r#"{"event":"status","left":null}"#).unwrap();
        d.apply(&obj);
        assert_eq!(d.left, None);
        assert!(d.right.is_some(), "absent keys must be left alone");
        let (_, obj) = parse_line(r#"{"right":{"level":255}}"#).unwrap();
        d.apply(&obj);
        assert_eq!(d.right, None, "sentinel levels read as unknown");
    }

    #[test]
    fn unknown_everything_shows_two_blank_rings() {
        let d = Device::default();
        let r = rings(&d);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|(_, b)| b.level.is_none()));
    }

    #[test]
    fn max_gets_one_ring() {
        let d = demo_device(Some("max"));
        let r = rings(&d);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, RingId::Headphones);
    }

    #[test]
    fn menu_fields_and_modes() {
        let (ev, obj) = parse_line(
            r#"{"event":"menu","anchor_x":1700,"connected":true,"model_id":8228,"listening_mode":"transparency","conversational_awareness":false,"allow_off":true}"#,
        )
        .unwrap();
        assert_eq!(ev, Event::Menu);
        let mut d = Device::default();
        d.apply(&obj);
        assert_eq!(d.mode(), Some(Mode::Transparency));
        assert_eq!(d.conversational_awareness, Some(false));
        assert_eq!(d.modes(), [Mode::Off, Mode::Anc, Mode::Transparency, Mode::Adaptive]);
        d.allow_off = None;
        d.model_id = Some(0x200E); // Pro 1: no Adaptive
        assert_eq!(d.modes(), [Mode::Anc, Mode::Transparency]);
        d.model_id = None; // unknown: offer it
        assert!(d.modes().contains(&Mode::Adaptive));
        assert_eq!(Mode::parse("noise_cancellation"), Some(Mode::Anc));
    }

    #[test]
    fn link_states() {
        let mut d = Device::default();
        assert_eq!(d.link_state(), LinkState::NotConnected);
        d.nearby = true;
        d.in_ear = Some((true, false));
        assert_eq!(d.link_state(), LinkState::Elsewhere);
        d.elsewhere = Some(false);
        assert_eq!(d.link_state(), LinkState::NotConnected, "an explicit flag wins");
        d.connected = true;
        assert_eq!(d.link_state(), LinkState::Connected);
    }

    #[test]
    fn event_names() {
        for (s, e) in [
            ("lid_open", Event::LidOpen),
            ("lid_close", Event::LidClose),
            ("connecting", Event::Connecting),
            ("connect_failed", Event::ConnectFailed),
            ("popup", Event::Popup),
        ] {
            let line = format!(r#"{{"event":"{s}"}}"#);
            assert_eq!(parse_line(&line).unwrap().0, e);
        }
        assert!(parse_line("not json").is_none());
        assert!(parse_line("[1,2]").is_none());
    }
}
