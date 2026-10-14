//! Getting the AirPods back when they have forgotten this computer.
//!
//! AirPods sometimes drop their half of the pairing: after being used with an
//! iPhone, after a reset, or when the other OS of a dual boot pairs them (both
//! share one Bluetooth address). BlueZ still holds its half, so every connect
//! fails with `br-connection-key-missing` and only pairing again helps.
//!
//! When a connect fails that way we say so on the card, in a notification and
//! in the bar, and re-pair by ourselves the moment the case is put in pairing
//! mode: forget the stale bond, scan until the AirPods turn up (a case in
//! pairing mode answers the inquiry), pair through our own agent, trust,
//! connect. Nothing to type, no terminal.

use crate::hub::{RepairState, hub};
use bluer::agent::Agent;
use bluer::{Address, AdapterEvent};
use futures::StreamExt;
use log::{info, warn};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::{Instant, sleep_until};

/// How long we wait for the case to go into pairing mode
/// (LIBREPODS_REPAIR_TIMEOUT seconds overrides it for tests/sim).
fn wait_for_pairing_mode() -> Duration {
    std::env::var("LIBREPODS_REPAIR_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(180))
}

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();

/// The daemon's runtime. Connects also run from short-lived runtimes (the
/// window's buttons), which would take a spawned recovery down with them.
pub fn set_runtime(handle: tokio::runtime::Handle) {
    let _ = RUNTIME.set(handle);
}

/// BlueZ's words for "the other side has no key for us any more".
pub fn is_key_missing(err: &str) -> bool {
    err.contains("key-missing") || err.contains("Key Missing")
}

pub fn running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

pub fn cancel() -> bool {
    let was = running();
    CANCEL.store(true, Ordering::SeqCst);
    was
}

/// Start pairing `mac` again unless that is already under way.
pub fn start(mac: String, why: &str) -> bool {
    let Some(rt) = RUNTIME.get() else {
        warn!("pairing recovery unavailable (no runtime yet)");
        return false;
    };
    if RUNNING.swap(true, Ordering::SeqCst) {
        return false;
    }
    CANCEL.store(false, Ordering::SeqCst);
    warn!("AirPods {mac} need to be paired again ({why})");
    rt.spawn(async move {
        hub().update("repair", |s| s.repair = Some(RepairState::Waiting));
        let result = run(&mac).await;
        RUNNING.store(false, Ordering::SeqCst);
        match result {
            Ok(()) => {
                info!("AirPods {mac} paired again");
                hub().update("repaired", |s| s.repair = None);
            }
            Err(e) if CANCEL.load(Ordering::SeqCst) => {
                info!("pairing recovery cancelled ({e})");
                hub().update("repair", |s| s.repair = None);
            }
            Err(e) => {
                warn!("pairing recovery for {mac} failed: {e}");
                hub().update("repair", |s| s.repair = Some(RepairState::Failed));
            }
        }
    });
    true
}

async fn run(mac: &str) -> Result<(), String> {
    let target: Address = mac.parse().map_err(|_| format!("bad address {mac}"))?;
    let session = bluer::Session::new().await.map_err(|e| e.to_string())?;
    let adapter = session.default_adapter().await.map_err(|e| e.to_string())?;

    // A NoInputNoOutput agent: AirPods pair with "just works", and without
    // any agent BlueZ refuses to pair at all (the old `bluetoothctl pair`
    // failure). Registered only for as long as this runs.
    let _agent = session
        .register_agent(Agent::default())
        .await
        .map_err(|e| format!("agent: {e}"))?;
    let was_pairable = adapter.is_pairable().await.unwrap_or(false);
    let _ = adapter.set_pairable(true).await;
    let result = pair_when_ready(&adapter, target, mac).await;
    if !was_pairable {
        let _ = adapter.set_pairable(false).await;
    }
    result?;

    // bluetoothctl copes better than bluer with BlueZ's own reconnect race.
    for _ in 0..3 {
        if crate::ipc::bluetoothctl("connect", mac).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    // Paired and trusted is what matters: they connect on their own when
    // taken out of the case.
    Ok(())
}

/// Forget our stale half of the bond, then pair as soon as the AirPods turn
/// up. They turn up when an inquiry finds them (pairing mode) or when they
/// page us; a pairing attempt that fails just goes back to waiting.
async fn pair_when_ready(adapter: &bluer::Adapter, target: Address, mac: &str) -> Result<(), String> {
    let deadline = Instant::now() + wait_for_pairing_mode();
    let mut last_error = String::from("the case never went into pairing mode");
    info!("waiting for {mac} to enter pairing mode");
    while Instant::now() < deadline {
        // While BlueZ holds a record for them it neither reports them as
        // found nor lets us pair; the removal also drops the dead
        // connection, which the usual disconnect signal doesn't report.
        if adapter.device_addresses().await.unwrap_or_default().contains(&target) {
            adapter
                .remove_device(target)
                .await
                .map_err(|e| format!("could not forget the old pairing: {e}"))?;
            hub().mark_disconnected();
        }

        // Same scan as `bluetoothctl scan on` (inquiry + LE), which finds a
        // case in pairing mode within seconds.
        let events = adapter.discover_devices().await.map_err(|e| e.to_string())?;
        tokio::pin!(events);
        let found = loop {
            if CANCEL.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            tokio::select! {
                ev = events.next() => match ev {
                    Some(AdapterEvent::DeviceAdded(addr)) if addr == target => break true,
                    Some(_) => {}
                    // discovery stopped under us (adapter reset, Bluetooth off): restart it
                    None => break false,
                },
                _ = sleep_until(deadline) => return Err(last_error),
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        };
        drop(events); // ends the discovery session: inquiry disturbs pairing
        if !found {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        info!("{mac} showed up, pairing");
        hub().update("repair", |s| s.repair = Some(RepairState::Pairing));
        let device = adapter.device(target).map_err(|e| e.to_string())?;
        match device.pair().await {
            Ok(()) => {}
            Err(e) if e.kind == bluer::ErrorKind::AlreadyExists => {}
            Err(e) => {
                last_error = format!("pairing failed: {e}");
                warn!("{last_error}; waiting for pairing mode again");
                hub().update("repair", |s| s.repair = Some(RepairState::Waiting));
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        }
        let _ = device.set_trusted(true).await;
        return Ok(());
    }
    Err(last_error)
}
