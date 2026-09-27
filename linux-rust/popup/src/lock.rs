//! Is the session locked? Asked only when a lid/connect event wants to
//! show the card, so a process spawn is fine. (podctl-popup suppresses its
//! bubble the same way, there via logind's LockedHint over D-Bus.)

use std::process::{Command, Stdio};

const LOCKERS: &[&str] = &["hyprlock", "swaylock", "gtklock", "waylock", "i3lock", "physlock"];

pub fn session_locked() -> bool {
    locker_running() || locked_hint()
}

/// A screen locker process owned by us is running. hyprlock does not set
/// logind's LockedHint, so this is the check that matters on Hyprland.
fn locker_running() -> bool {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    let uid = unsafe { libc::getuid() };
    for e in dir.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().filter(|s| s.bytes().all(|b| b.is_ascii_digit())) else {
            continue;
        };
        let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else {
            continue;
        };
        if LOCKERS.contains(&comm.trim()) {
            use std::os::unix::fs::MetadataExt;
            if e.metadata().map(|m| m.uid() == uid).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

fn locked_hint() -> bool {
    let sid = std::env::var("XDG_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "auto".into());
    Command::new("loginctl")
        .args(["show-session", &sid, "-p", "LockedHint", "--value"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "yes")
        .unwrap_or(false)
}
