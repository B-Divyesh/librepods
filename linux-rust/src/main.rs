mod audio;
mod bluetooth;
mod devices;
mod hub;
mod ipc;
mod media_controller;
mod notify;
mod ui;
mod utils;

use crate::bluetooth::discovery::{find_connected_airpods, find_other_managed_devices};
use crate::bluetooth::le::start_le_monitor;
use crate::bluetooth::managers::DeviceManagers;
use crate::devices::enums::DeviceData;
use crate::ui::messages::BluetoothUIMessage;
use crate::ui::tray::MyTray;
use crate::utils::{ensure_device_registered, get_app_settings_path, get_devices_path};
use bluer::{Address, InternalErrorKind};
use clap::Parser;
use dbus::arg::{RefArg, Variant};
use dbus::blocking::Connection;
use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
use dbus::message::MatchRule;
use devices::airpods::AirPodsDevice;
use ksni::TrayMethods;
use log::{debug, info, warn};
use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicBool};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

#[derive(Parser)]
struct Args {
    #[arg(long, short = 'd', help = "Enable debug logging")]
    debug: bool,
    #[arg(
        long,
        help = "Disable system tray, useful if your environment doesn't support AppIndicator or StatusNotifier"
    )]
    no_tray: bool,
    #[arg(long, help = "Start the application minimized to tray")]
    start_minimized: bool,
    #[arg(
        long,
        help = "Enable Bluetooth LE debug logging. Only use when absolutely necessary; this produces a lot of logs."
    )]
    le_debug: bool,
    #[arg(long, help = "Don't show the tray icon (the window and everything else still run)")]
    hide_tray_icon: bool,
    #[arg(long, short = 'v', help = "Show application version and exit")]
    version: bool
}

fn main() -> iced::Result {
    // `librepods ctl …` talks to a running instance and exits.
    let raw: Vec<String> = env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some("ctl") {
        std::process::exit(ipc::run_client(&raw[1..]));
    }

    let args = Args::parse();

    if args.version {
        println!(
            "You are running LibrePods version {}",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }

    let log_level = if args.debug { "debug" } else { "info" };
    // let wayland_display = env::var("WAYLAND_DISPLAY").is_ok();
    // if wayland_display && env::var("WGPU_BACKEND").is_err() {
    //     unsafe { env::set_var("WGPU_BACKEND", "gl") };
    // }
    if env::var("RUST_LOG").is_err() {
        unsafe {
            env::set_var(
                "RUST_LOG",
                log_level.to_owned()
                    + &format!(
                        ",zbus=warn,winit=warn,tracing=warn,iced_wgpu=warn,wgpu_hal=warn,wgpu_core=warn,cosmic_text=warn,naga=warn,iced_winit=warn,librepods::bluetooth::le={}",
                        if args.le_debug { "debug" } else { "info" }
                    ),
            )
        };
    }
    env_logger::init();

    let (ui_tx, ui_rx) = unbounded_channel::<BluetoothUIMessage>();

    let device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Load stem_control initial value from settings JSON, then apply CLI override.
    if args.no_tray {
        // Run headless without UI
        info!("Running in headless mode (no GUI)");
        let rt = tokio::runtime::Runtime::new().unwrap();
        if let Err(e) = rt.block_on(async_main(ui_tx, device_managers)) {
            log::error!("LibrePods could not start: {e}");
            std::process::exit(1);
        }
        Ok(())
    } else {
        // Run with UI
        let device_managers_clone = device_managers.clone();
        std::thread::spawn(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            if let Err(e) = rt.block_on(async_main(ui_tx, device_managers_clone)) {
                log::error!("LibrePods could not start: {e}");
                std::process::exit(1);
            }
        });

        ui::window::start_ui(ui_rx, args.start_minimized, device_managers)
    }
}

async fn async_main(
    ui_tx: tokio::sync::mpsc::UnboundedSender<BluetoothUIMessage>,
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
) -> bluer::Result<()> {
    let args = Args::parse();

    let mut managed_devices_mac: Vec<String> = Vec::new(); // includes ony non-AirPods. AirPods handled separately.

    let devices_path = get_devices_path();
    let devices_json = std::fs::read_to_string(&devices_path).unwrap_or_else(|e| {
        log::error!("Failed to read devices file: {}", e);
        "{}".to_string()
    });
    let devices_list: HashMap<String, DeviceData> = serde_json::from_str(&devices_json)
        .unwrap_or_else(|e| {
            log::error!("Deserialization failed: {}", e);
            HashMap::new()
        });
    for (mac, device_data) in devices_list.iter() {
        if device_data.type_ == devices::enums::DeviceType::Nothing {
            managed_devices_mac.push(mac.clone());
        }
    }

    let (shutdown_tx, shutdown_rx) = unbounded_channel::<()>();
    spawn_shutdown_handler(device_managers.clone(), shutdown_rx);

    if !args.no_tray {
        hub::hub().set_ui_tx(ui_tx.clone());
    }
    tokio::spawn(ipc::serve());
    tokio::spawn(notify::run());
    spawn_popup();

    let tray_handle = if args.no_tray
        || args.hide_tray_icon
        || crate::utils::AppSettings::load().hide_tray_icon
    {
        None
    } else {
        let tray = MyTray {
            conversation_detect_enabled: None,
            battery_headphone: None,
            battery_headphone_status: None,
            battery_l: None,
            battery_l_status: None,
            battery_r: None,
            battery_r_status: None,
            battery_c: None,
            battery_c_status: None,
            connected: false,
            listening_mode: None,
            allow_off_option: None,
            command_tx: None,
            ui_tx: Some(ui_tx.clone()),
            shutdown_tx: Some(shutdown_tx.clone()),
        };
        match tray.spawn().await {
            Ok(handle) => Some(handle),
            Err(e) => {
                log::warn!(
                    "Failed to start system tray ({e}); continuing without tray. \
                     Your environment may lack a StatusNotifier/AppIndicator watcher."
                );
                None
            }
        }
    };

    let session = bluer::Session::new().await.inspect_err(|e| {
        log::error!(
            "Cannot talk to BlueZ over D-Bus: {e}. Is the bluetooth service running? \
             Check with `systemctl status bluetooth`."
        )
    })?;
    let adapter = session.default_adapter().await.inspect_err(|e| {
        log::error!(
            "No Bluetooth adapter available: {e}. Make sure an adapter is present \
             and not blocked - see `rfkill list bluetooth`."
        )
    })?;
    // Respect a user who turned Bluetooth off (Mac-like); BlueZ's AutoEnable
    // powers it at boot anyway.
    let powered = adapter.is_powered().await.unwrap_or(false);
    if !powered {
        info!("Bluetooth is off; waiting for it to be turned on");
    }
    hub::hub().update("status", |s| s.bluetooth_on = powered);


    let le_tray_clone = tray_handle.clone();
    let le_ui_tx = ui_tx.clone();
    // Supervise the LE monitor: restart it when it fails and whenever the
    // adapter is powered back on (monitors don't survive a power cycle).
    tokio::spawn(async move {
        loop {
            let mut rx = hub::hub().subscribe();
            info!("Starting LE monitor...");
            tokio::select! {
                r = start_le_monitor(le_tray_clone.clone(), le_ui_tx.clone()) => {
                    if let Err(e) = r {
                        log::warn!("LE monitor stopped: {}", e);
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                _ = async {
                    loop {
                        match rx.recv().await {
                            Ok(ev) if ev.event == "bluetooth_on" => break,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                std::future::pending::<()>().await
                            }
                            _ => {}
                        }
                    }
                } => info!("Bluetooth powered on, restarting LE monitor"),
            }
        }
    });

    info!("Listening for new connections.");

    info!("Checking for connected devices...");
    match find_connected_airpods(&adapter).await {
        Ok(device) => {
            let name = device
                .name()
                .await?
                .unwrap_or_else(|| "Unknown".to_string());
            info!("Found connected AirPods: {}, initializing.", name);
            hub::hub().set_quiet_connect();
            ensure_device_registered(
                &device.address().to_string(),
                &name,
                devices::enums::DeviceType::AirPods,
            );
            let airpods_device =
                AirPodsDevice::new(device.address(), tray_handle.clone(), ui_tx.clone()).await;

            let hires_aacp = airpods_device.aacp_manager.clone();
            let mut managers = device_managers.write().await;
            // let dev_managers = DeviceManagers::with_both(airpods_device.aacp_manager.clone(), airpods_device.att_manager.clone());
            let dev_managers = DeviceManagers::with_aacp(airpods_device.aacp_manager.clone());
            managers
                .entry(device.address().to_string())
                .or_insert(dev_managers)
                .set_aacp(airpods_device.aacp_manager);
            drop(managers);
            if let Err(e) = ui_tx.send(BluetoothUIMessage::DeviceConnected(
                device.address().to_string(),
            )) {
                warn!("Failed to send DeviceConnected UI message: {:?}", e);
            }
            // LIBREPODS_HIRES_MIC=1: enable the hi-res mic feature headlessly.
            if env::var("LIBREPODS_HIRES_MIC").is_ok() {
                tokio::spawn(async move {
                    hires_aacp.set_hires_mic_enabled(true).await;
                });
            }
        }
        Err(_) => {
            info!("No connected AirPods found.");
        }
    }

    match find_other_managed_devices(&adapter, managed_devices_mac.clone()).await {
        Ok(devices) => {
            for device in devices {
                let addr_str = device.address().to_string();
                info!(
                    "Found connected managed device: {}, initializing.",
                    addr_str
                );
                let type_ = devices_list.get(&addr_str).unwrap().type_.clone();
                let ui_tx_clone = ui_tx.clone();
                let device_managers = device_managers.clone();
                tokio::spawn(async move {
                    let mut managers = device_managers.write().await;
                    if type_ == devices::enums::DeviceType::Nothing {
                        let dev = devices::nothing::NothingDevice::new(
                            device.address(),
                            ui_tx_clone.clone(),
                        )
                        .await;
                        let dev_managers = DeviceManagers::with_att(dev.att_manager.clone());
                        managers
                            .entry(addr_str.clone())
                            .or_insert(dev_managers)
                            .set_att(dev.att_manager);
                        if let Err(e) = ui_tx_clone.send(BluetoothUIMessage::DeviceConnected(addr_str)) {
                            warn!("Failed to send DeviceConnected UI message: {:?}", e);
                        }
                    }
                    drop(managers)
                });
            }
        }
        Err(e) => {
            log::debug!("type of error: {:?}", e.kind);
            if e.kind
                != bluer::ErrorKind::Internal(InternalErrorKind::Io(std::io::ErrorKind::NotFound))
            {
                log::error!("Error finding other managed devices: {}", e);
            } else {
                info!("No other managed devices found.");
            }
        }
    }

    let conn = Connection::new_system()?;
    let rule = MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged");
    conn.add_match(rule, move |_: (), conn, msg| {
        let Some(path) = msg.path() else {
            return true;
        };
        // Adapter power (Bluetooth on/off toggles anywhere in the system).
        if path.starts_with("/org/bluez/hci") && !path.contains("/dev_") {
            if let Ok((iface, changed, _)) =
                msg.read3::<String, HashMap<String, Variant<Box<dyn RefArg>>>, Vec<String>>()
                && iface == "org.bluez.Adapter1"
                && let Some(p) = changed.get("Powered").and_then(|v| v.0.as_u64())
            {
                let on = p != 0;
                info!("Bluetooth adapter powered {}", if on { "on" } else { "off" });
                hub::hub().update(if on { "bluetooth_on" } else { "bluetooth_off" }, |s| {
                    s.bluetooth_on = on;
                    if !on {
                        s.nearby = false;
                        s.lid_open = None;
                    }
                });
            }
            return true;
        }
        if !path.contains("/org/bluez/hci") || !path.contains("/dev_") {
            return true;
        }
        // debug!("PropertiesChanged signal for path: {}", path);
        let Ok((iface, changed, _)) =
            msg.read3::<String, HashMap<String, Variant<Box<dyn RefArg>>>, Vec<String>>()
        else {
            return true;
        };
        if iface != "org.bluez.Device1" {
            return true;
        }
        let Some(connected_var) = changed.get("Connected") else {
            return true;
        };
        let Some(is_connected) = connected_var.0.as_ref().as_u64() else {
            return true;
        };
        let proxy = conn.with_proxy("org.bluez", path, std::time::Duration::from_millis(5000));
        let Ok(uuids) = proxy.get::<Vec<String>>("org.bluez.Device1", "UUIDs") else {
            return true;
        };
        let target_uuid = "74ec2172-0bad-4d01-8f77-997b2be0722a";

        let Ok(addr_str) = proxy.get::<String>("org.bluez.Device1", "Address") else {
            return true;
        };
        let Ok(addr) = addr_str.parse::<Address>() else {
            return true;
        };
        if is_connected==0 {
            if hub::hub().snapshot().mac.as_deref() == Some(addr_str.as_str()) {
                info!("AirPods {} disconnected", addr_str);
                hub::hub().set_command_tx(None);
                hub::hub().update("disconnected", |s| {
                    s.connected = false;
                    s.listening_mode = None;
                    s.conversational_awareness = None;
                    s.allow_off = None;
                    s.headphone = None;
                    s.in_ear = Default::default();
                });
                if let Some(handle) = tray_handle.clone() {
                    tokio::spawn(async move {
                        handle
                            .update(|t: &mut MyTray| {
                                t.connected = false;
                                t.listening_mode = None;
                                t.conversation_detect_enabled = None;
                                t.command_tx = None;
                                t.battery_headphone = None;
                                t.battery_l = None;
                                t.battery_r = None;
                                t.battery_c = None;
                            })
                            .await;
                    });
                }
            }
            if let Err(e) = ui_tx.send(BluetoothUIMessage::DeviceDisconnected(addr_str.clone())) {
                warn!("Failed to send DeviceConnected UI message: {:?}", e);
            }
            return true
        }
        if managed_devices_mac.contains(&addr_str) {
            info!("Managed device connected: {}, initializing", addr_str);
            let type_ = devices_list.get(&addr_str).unwrap().type_.clone();
            if type_ == devices::enums::DeviceType::Nothing {
                let ui_tx_clone = ui_tx.clone();
                let device_managers = device_managers.clone();
                tokio::spawn(async move {
                    let mut managers = device_managers.write().await;
                    let dev = devices::nothing::NothingDevice::new(addr, ui_tx_clone.clone()).await;
                    let dev_managers = DeviceManagers::with_att(dev.att_manager.clone());
                    managers
                        .entry(addr_str.clone())
                        .or_insert(dev_managers)
                        .set_att(dev.att_manager);
                    drop(managers);
                    if let Err(e) = ui_tx_clone.send(BluetoothUIMessage::DeviceConnected(addr_str.clone())) {
                        warn!("Failed to send DeviceConnected UI message: {:?}", e);
                    }
                });
            }
            return true;
        }

        if !uuids.iter().any(|u| u.to_lowercase() == target_uuid) {
            return true;
        }
        let name = proxy
            .get::<String>("org.bluez.Device1", "Name")
            .unwrap_or_else(|_| "Unknown".to_string());
        info!("AirPods connected: {}, initializing", name);
        ensure_device_registered(&addr_str, &name, devices::enums::DeviceType::AirPods);
        let handle_clone = tray_handle.clone();
        let ui_tx_clone = ui_tx.clone();
        let device_managers = device_managers.clone();
        tokio::spawn(async move {
            let airpods_device = AirPodsDevice::new(addr, handle_clone, ui_tx_clone.clone()).await;
            let mut managers = device_managers.write().await;
            // let dev_managers = DeviceManagers::with_both(airpods_device.aacp_manager.clone(), airpods_device.att_manager.clone());
            let dev_managers = DeviceManagers::with_aacp(airpods_device.aacp_manager.clone());
            managers
                .entry(addr_str.clone())
                .or_insert(dev_managers)
                .set_aacp(airpods_device.aacp_manager);
            drop(managers);
            if let Err(e) = ui_tx_clone.send(BluetoothUIMessage::DeviceConnected(addr_str.clone())) {
                warn!("Failed to send DeviceConnected UI message: {:?}", e);
            }
        });
        true
    })?;

    info!("Listening for Bluetooth connections via D-Bus...");
    loop {
        conn.process(std::time::Duration::from_millis(1000))?;
    }
}

fn spawn_shutdown_handler(
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    mut shutdown_rx: UnboundedReceiver<()>,
) {
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).ok();
        let mut sigint = signal(SignalKind::interrupt()).ok();
        let sigterm = async {
            match sigterm.as_mut() {
                Some(s) => s.recv().await.map(|_| ()),
                None => std::future::pending().await,
            }
        };
        let sigint = async {
            match sigint.as_mut() {
                Some(s) => s.recv().await.map(|_| ()),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = shutdown_rx.recv() => {}
            _ = sigterm => {}
            _ = sigint => {}
        }

        info!("Shutting down: tearing down hi-res mic streams...");
        let cleanup = async {
            let managers = device_managers.read().await;
            for dm in managers.values() {
                if let Some(aacp) = dm.get_aacp() {
                    aacp.disarm_hires_mic().await;
                }
            }
        };
        if tokio::time::timeout(std::time::Duration::from_secs(3), cleanup)
            .await
            .is_err()
        {
            warn!("Shutdown cleanup timed out; exiting anyway");
        }
        std::process::exit(0);
    });
}

/// Runs librepods-popup (the iOS-style case card) next to us and restarts it
/// if it dies. Without it, notify.rs falls back to plain notifications.
fn spawn_popup() {
    let beside = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("librepods-popup")));
    let in_path = env::var("PATH").ok().and_then(|p| {
        p.split(':')
            .map(|d| std::path::Path::new(d).join("librepods-popup"))
            .find(|c| c.exists())
    });
    let Some(path) = beside.filter(|p| p.exists()).or(in_path) else {
        info!("librepods-popup not found; using plain notifications");
        return;
    };
    tokio::spawn(async move {
        use std::sync::atomic::Ordering::Relaxed;
        let mut backoff = std::time::Duration::from_secs(5);
        loop {
            match tokio::process::Command::new(&path).kill_on_drop(true).spawn() {
                Ok(mut child) => {
                    let started = std::time::Instant::now();
                    notify::POPUP_RUNNING.store(true, Relaxed);
                    let status = child.wait().await;
                    if started.elapsed() < std::time::Duration::from_secs(3) {
                        // Exited at once: most likely another copy already
                        // owns the session lock. It still shows the cards, so
                        // keep POPUP_RUNNING and check back slowly.
                        backoff = (backoff * 2).min(std::time::Duration::from_secs(300));
                        debug!("librepods-popup exited immediately ({:?}); retry in {:?}", status, backoff);
                    } else {
                        notify::POPUP_RUNNING.store(false, Relaxed);
                        backoff = std::time::Duration::from_secs(5);
                        warn!("librepods-popup exited ({:?}), restarting in 5 s", status);
                    }
                }
                Err(e) => {
                    warn!("Cannot start {}: {}", path.display(), e);
                    return;
                }
            }
            tokio::time::sleep(backoff).await;
        }
    });
}
