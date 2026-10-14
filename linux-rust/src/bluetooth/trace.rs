//! Optional capture of the AACP control channel, for working on the
//! connection handshake (e.g. the firmware-9A348 drop, librepods#782).
//!
//! `LIBREPODS_AACP_TRACE=/path/to/file` makes the daemon append one line per
//! control frame it sends or receives:
//!
//! ```text
//! 1760000000123 TX 00000400010002000000000000000000  handshake(0x0001)
//! 1760000000456 RX 0400040004000000...                battery(0x0004)
//! ```
//!
//! Secret and personal fields are masked to `00` before anything is written:
//! the BLE identity keys (opcode 0x31), the device-info strings — name,
//! serial numbers, versions (0x1D) — and the frames that carry a Bluetooth
//! address (connected devices, audio source, smart routing). A printable-run
//! mask is a second net for any string in any frame. The handshake, feature
//! and notification opcodes — what the capture is actually for — are kept.
//!
//! Capturing is off unless the variable is set; see tests/sim/aacp_decode.py
//! and the repo README for the workflow.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
pub enum Dir {
    Tx,
    Rx,
}

/// Opcodes whose payload is a secret or personal (keys, serials, strings,
/// Bluetooth addresses): the whole payload after the 6-byte header is masked.
const SENSITIVE: &[u16] = &[
    0x001D, // device information: name, serial numbers, versions
    0x0031, // BLE keys response: IRK + encryption key
    0x002E, // connected devices: MAC per device
    0x000E, // audio source: MAC
    0x0010, // smart routing relay: MAC
    0x0011, // smart routing response: MAC
    0x0014, // connected device MAC
];

fn sink() -> Option<&'static Mutex<File>> {
    static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = PathBuf::from(std::env::var_os("LIBREPODS_AACP_TRACE")?);
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => {
                eprintln!("librepods: AACP trace → {}", path.display());
                Some(Mutex::new(f))
            }
            Err(e) => {
                eprintln!("librepods: could not open AACP trace {}: {e}", path.display());
                None
            }
        }
    })
    .as_ref()
}

/// Cheap check so callers can skip formatting when tracing is off.
pub fn enabled() -> bool {
    sink().is_some()
}

/// Open the trace file now (if LIBREPODS_AACP_TRACE is set), so it is
/// confirmed at startup rather than on the first frame.
pub fn init() {
    let _ = sink();
}

pub fn record(dir: Dir, frame: &[u8]) {
    let Some(sink) = sink() else { return };
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let d = match dir {
        Dir::Tx => "TX",
        Dir::Rx => "RX",
    };
    let line = format!("{ms} {d} {}  {}\n", hex::encode(redact(frame)), label(frame));
    if let Ok(mut f) = sink.lock() {
        let _ = f.write_all(line.as_bytes());
    }
}

/// The little-endian opcode at bytes [4..6]; `None` for a runt frame.
/// Holds for data frames (`04 00 04 00 <op>`) and the handshake
/// (`00 00 04 00 01 00 …` → 0x0001) alike.
pub fn opcode(frame: &[u8]) -> Option<u16> {
    (frame.len() >= 6).then(|| u16::from_le_bytes([frame[4], frame[5]]))
}

fn label(frame: &[u8]) -> String {
    match opcode(frame) {
        Some(op) => format!("{}(0x{op:04x})", opcode_name(op)),
        None => "runt".into(),
    }
}

pub fn opcode_name(op: u16) -> &'static str {
    match op {
        0x0001 => "handshake",
        0x0004 => "battery",
        0x0006 => "ear-detection",
        0x0009 => "control-command",
        0x000D => "audio-source-req",
        0x000E => "audio-source-resp",
        0x000F => "notification-register",
        0x0010 => "smart-routing-relay",
        0x0011 => "smart-routing-resp",
        0x0014 => "connected-device-mac",
        0x0017 => "multi-undocumented",
        0x0019 => "stem-press",
        0x001B => "timestamp",
        0x001D => "device-info",
        0x001E => "rename",
        0x0022 => "case-info-req",
        0x0029 => "host-capabilities-29",
        0x002B => "paired-devices",
        0x002D => "connected-devices-req",
        0x002E => "connected-devices",
        0x0030 => "ble-keys-req",
        0x0031 => "ble-keys-resp",
        0x004B => "conversation-awareness",
        0x004D => "host-capabilities",
        0x004F => "info-req-dead",
        0x0053 => "eq-data",
        0x0058 => "hi-res-audio",
        _ => "unknown",
    }
}

/// A copy of `frame` with secret and personal bytes zeroed. Pure, so the
/// rules are unit-tested (see `tests`).
pub fn redact(frame: &[u8]) -> Vec<u8> {
    let mut out = frame.to_vec();
    if let Some(op) = opcode(frame)
        && SENSITIVE.contains(&op)
        && out.len() > 6
    {
        for b in &mut out[6..] {
            *b = 0;
        }
    }
    // Second net: any run of >= 6 printable ASCII bytes (a name, a serial, a
    // version string, an updater id) in any frame, past the header.
    let start = out.len().min(6);
    let mut i = start;
    while i < out.len() {
        if is_printable(out[i]) {
            let mut j = i + 1;
            while j < out.len() && is_printable(out[j]) {
                j += 1;
            }
            if j - i >= 6 {
                for b in &mut out[i..j] {
                    *b = 0;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn is_printable(b: u8) -> bool {
    (0x20..=0x7e).contains(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(op: u16, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0x04, 0x00, 0x04, 0x00, (op & 0xff) as u8, (op >> 8) as u8];
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn opcode_is_read_from_offset_4() {
        let handshake = [0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00];
        assert_eq!(opcode(&handshake), Some(0x0001));
        assert_eq!(opcode(&frame(0x004D, &[0xff])), Some(0x004D));
        assert_eq!(opcode(&[0x04, 0x00]), None);
    }

    #[test]
    fn ble_keys_payload_is_fully_masked() {
        // 0x31 then two 16-byte keys
        let key = [0xAB; 16];
        let mut payload = vec![0x00, 0x02, 0x01, 0x00, 0x10, 0x00];
        payload.extend_from_slice(&key);
        payload.extend_from_slice(&key);
        let r = redact(&frame(0x0031, &payload));
        assert!(r[6..].iter().all(|&b| b == 0), "no key byte survives");
        assert_eq!(&r[..6], &[0x04, 0x00, 0x04, 0x00, 0x31, 0x00], "header kept");
    }

    #[test]
    fn device_info_strings_are_masked() {
        let mut payload = vec![0x01, 0x00];
        payload.extend_from_slice(b"AirPods Pro\x00H7XK2L7P3Q\x00Apple Inc.\x00");
        let r = redact(&frame(0x001D, &payload));
        let hay = String::from_utf8_lossy(&r).into_owned();
        for s in ["AirPods", "H7XK2L7P3Q", "Apple"] {
            assert!(!hay.contains(s), "{s} leaked");
        }
    }

    #[test]
    fn connected_devices_mac_is_masked() {
        // 0x2E: 2 bytes, count, then a MAC (binary, not printable)
        let r = redact(&frame(0x002E, &[0x01, 0x00, 0x01, 0xac, 0xde, 0x48, 0x00, 0x11, 0x22, 0x01, 0x02]));
        assert!(r[6..].iter().all(|&b| b == 0), "MAC and the rest masked");
    }

    #[test]
    fn handshake_and_flags_are_kept_verbatim() {
        // the frames the capture is actually for are not sensitive
        let handshake = [0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00];
        assert_eq!(redact(&handshake), handshake);
        let caps = frame(0x004D, &[0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(redact(&caps), caps);
        let notif = frame(0x000F, &[0xff, 0xff, 0xff, 0xff]);
        assert_eq!(redact(&notif), notif);
    }

    #[test]
    fn short_binary_fields_survive() {
        // a battery frame's levels must not be mistaken for a string
        let batt = frame(0x0004, &[0x01, 0x04, 0x01, 0x02, 0x50, 0x01]);
        assert_eq!(redact(&batt), batt);
    }
}
