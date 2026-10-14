use crate::bluetooth::aacp::ControlCommandIdentifiers;
use crate::bluetooth::aacp::{BatteryStatus, EarDetectionStatus};
use crate::hub::{Battery, ListeningMode, hub};
use std::sync::atomic::{AtomicBool, Ordering};
use crate::bluetooth::aacp::{AACPEvent, AACPManager, AirPodsLEKeys, ProximityKeyType};
use crate::media_controller::MediaController;
use crate::ui::messages::BluetoothUIMessage;
use crate::ui::tray::MyTray;
use bluer::Address;
use ksni::Handle;
use log::{debug, error, info};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};
use crate::utils::get_app_settings_path;

pub struct AirPodsDevice {
    pub mac_address: Address,
    pub aacp_manager: AACPManager,
    // pub att_manager: ATTManager,
    pub media_controller: Arc<Mutex<MediaController>>,
    // pub command_tx: Option<tokio::sync::mpsc::UnboundedSender<(ControlCommandIdentifiers, Vec<u8>)>>,
}

impl AirPodsDevice {
    pub async fn new(
        mac_address: Address,
        tray_handle: Option<Handle<MyTray>>,
        ui_tx: tokio::sync::mpsc::UnboundedSender<BluetoothUIMessage>,
    ) -> Self {
        info!("Creating new AirPodsDevice for {}", mac_address);
        let mut aacp_manager = AACPManager::new();
        aacp_manager.connect(mac_address).await;

        // Tell the hub (CLI, Waybar, popup). The "connected" event itself waits
        // for the first battery report (or 1.5 s) so cards show real levels.
        let control_up = aacp_manager.state.lock().await.sender.is_some();
        let stored_name = aacp_manager
            .state
            .lock()
            .await
            .devices
            .get(&mac_address.to_string())
            .map(|d| d.name.clone())
            .filter(|n| !n.is_empty() && *n != mac_address.to_string());
        hub().update("status", |s| {
            s.connected = control_up;
            s.mac = Some(mac_address.to_string());
            s.other_host = false;
            if let Some(n) = &stored_name {
                s.name = Some(n.clone());
            }
        });
        if !control_up {
            hub().clear_quiet_connect();
        }
        let announced = Arc::new(AtomicBool::new(!control_up));
        {
            let announced = announced.clone();
            tokio::spawn(async move {
                sleep(Duration::from_millis(1500)).await;
                if !announced.swap(true, Ordering::SeqCst) {
                    hub().announce_connected();
                }
            });
        }

        // let mut att_manager = ATTManager::new();
        // att_manager.connect(mac_address).await.expect("Failed to connect ATT");

        if let Some(handle) = &tray_handle {
            handle
                .update(|tray: &mut MyTray| {
                    tray.connected = true;
                    tray.battery_headphone = None;
                    tray.battery_headphone_status = None;
                    tray.battery_l = None;
                    tray.battery_l_status = None;
                    tray.battery_r = None;
                    tray.battery_r_status = None;
                    tray.battery_c = None;
                    tray.battery_c_status = None;
                })
                .await;
        }

        // Experimental (librepods#782): on firmware 9A348 the control link is
        // dropped ~40 s after connecting in Apple-Device-ID mode. The core
        // handshake already matches macOS; LIBREPODS_AACP_STRICT=1 additionally
        // sends *only* the documented third-party sequence — handshake,
        // host-capabilities (0x4D), notification register (0x0F) — and skips
        // the extra 0x29 capability packet and the eager BLE-key request, both
        // of which that firmware is suspected to object to. Trade-off: BLE
        // address resolution won't refresh until the next reconnect, so
        // nearby/advert battery may lag while connected. Capture the result
        // with LIBREPODS_AACP_TRACE to see whether the link stays up.
        let strict = std::env::var_os("LIBREPODS_AACP_STRICT").is_some();
        if strict {
            info!("AACP strict handshake (experimental): documented macOS sequence only");
        }

        info!("Sending handshake");
        if let Err(e) = aacp_manager.send_handshake().await {
            error!("Failed to send handshake to AirPods device: {}", e);
        }

        sleep(Duration::from_millis(300)).await;

        info!("Setting feature flags");
        if let Err(e) = aacp_manager.send_set_feature_flags_packet().await {
            error!("Failed to set feature flags: {}", e);
        }

        sleep(Duration::from_millis(300)).await;

        info!("Requesting notifications");
        if let Err(e) = aacp_manager.send_notification_request().await {
            error!("Failed to request notifications: {}", e);
        }

        if !strict {
            info!("sending some packet");
            if let Err(e) = aacp_manager.send_some_packet().await {
                error!("Failed to send some packet: {}", e);
            }

            info!("Requesting Proximity Keys: IRK and ENC_KEY");
            if let Err(e) = aacp_manager
                .send_proximity_keys_request(vec![ProximityKeyType::Irk, ProximityKeyType::EncKey])
                .await
            {
                error!("Failed to request proximity keys: {}", e);
            }
        } else {
            info!("strict: skipping the 0x29 capability packet and the eager BLE-key request");
        }

        let app_settings_path = get_app_settings_path();
        let settings = std::fs::read_to_string(&app_settings_path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        let stem_control = settings
            .clone()
            .and_then(|v| v.get("stem_control").cloned())
            .and_then(|s| serde_json::from_value(s).ok())
            .unwrap_or(false);

        if stem_control {
            // Enable stem press detection (double and triple tap)
            // StemConfig bitmask for the control command: single=0x01, double=0x02, triple=0x04, long=0x08
            // We want double and triple: 0x02 | 0x04 = 0x06
            // Note: these bitmask values differ from the StemPressType event enum values (0x05–0x08)
            info!("Enabling stem press detection for double and triple tap");
            if let Err(e) = aacp_manager
                .send_control_command(ControlCommandIdentifiers::StemConfig, &[0x06])
                .await
            {
                error!("Failed to enable stem press detection: {}", e);
            }
        }

        let session = bluer::Session::new()
            .await
            .expect("Failed to get bluer session");
        let adapter = session
            .default_adapter()
            .await
            .expect("Failed to get default adapter");
        let local_mac = adapter
            .address()
            .await
            .expect("Failed to get adapter address")
            .to_string();

        let media_controller = Arc::new(Mutex::new(MediaController::new(
            mac_address.to_string(),
            local_mac.clone(),
        )));
        let mc_clone = media_controller.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel();
        hub().set_command_tx(Some(command_tx.clone()));

        aacp_manager.set_event_channel(tx.clone()).await;
        // The buds report battery and ear state right after the handshake,
        // before this channel existed; replay what the manager already has
        // (upstream dropped these, leaving battery unknown for minutes).
        {
            let st = aacp_manager.state.lock().await;
            if !st.battery_info.is_empty() {
                let _ = tx.send(AACPEvent::BatteryInfo(st.battery_info.clone()));
            }
            if !st.ear_detection_status.is_empty() {
                let _ = tx.send(AACPEvent::EarDetection(
                    st.old_ear_detection_status.clone(),
                    st.ear_detection_status.clone(),
                ));
            }
        }
        if let Some(handle) = &tray_handle {
            handle
                .update(|tray: &mut MyTray| tray.command_tx = Some(command_tx.clone()))
                .await;
        }

        let aacp_manager_clone = aacp_manager.clone();
        tokio::spawn(async move {
            while let Some((id, value)) = command_rx.recv().await {
                if let Err(e) = aacp_manager_clone.send_control_command(id, &value).await {
                    log::error!("Failed to send control command: {}", e);
                }
            }
        });

        // PipeWire can leave the card on the "off" profile when it appears
        // before its A2DP transport is ready, and nothing revisits that choice
        // until playback starts - so freshly connected buds stay silent, and
        // the microphone has no transport either. Claim a profile right away.
        let mc_profile = media_controller.clone();
        tokio::spawn(async move {
            // Clone out of the mutex: activation can take seconds and must not
            // block ear-detection handling meanwhile.
            let mc = mc_profile.lock().await.clone();
            mc.activate_a2dp_profile().await;
        });

        let mc_listener = media_controller.lock().await;
        let aacp_manager_clone_listener = aacp_manager.clone();
        mc_listener
            .start_playback_listener(aacp_manager_clone_listener, command_tx.clone())
            .await;
        drop(mc_listener);

        let (listening_mode_tx, mut listening_mode_rx) = tokio::sync::mpsc::unbounded_channel();
        aacp_manager
            .subscribe_to_control_command(
                ControlCommandIdentifiers::ListeningMode,
                listening_mode_tx,
            )
            .await;
        let tray_handle_clone = tray_handle.clone();
        tokio::spawn(async move {
            while let Some(value) = listening_mode_rx.recv().await {
                let mode = ListeningMode::from_byte(value[0]);
                let prev = hub().snapshot().listening_mode;
                let ev = if prev.is_some() && prev != mode { "mode" } else { "status" };
                hub().update(ev, |s| s.listening_mode = mode);
                if let Some(handle) = &tray_handle_clone {
                    handle
                        .update(|tray: &mut MyTray| {
                            tray.listening_mode = Some(value[0]);
                        })
                        .await;
                }
            }
        });

        let (allow_off_tx, mut allow_off_rx) = tokio::sync::mpsc::unbounded_channel();
        aacp_manager
            .subscribe_to_control_command(ControlCommandIdentifiers::AllowOffOption, allow_off_tx)
            .await;
        let tray_handle_clone = tray_handle.clone();
        tokio::spawn(async move {
            while let Some(value) = allow_off_rx.recv().await {
                hub().update("status", |s| s.allow_off = Some(value[0] == 0x01));
                if let Some(handle) = &tray_handle_clone {
                    handle
                        .update(|tray: &mut MyTray| {
                            tray.allow_off_option = Some(value[0]);
                        })
                        .await;
                }
            }
        });

        let (conversation_detect_tx, mut conversation_detect_rx) =
            tokio::sync::mpsc::unbounded_channel();
        aacp_manager
            .subscribe_to_control_command(
                ControlCommandIdentifiers::ConversationDetectConfig,
                conversation_detect_tx,
            )
            .await;
        let tray_handle_clone = tray_handle.clone();
        tokio::spawn(async move {
            while let Some(value) = conversation_detect_rx.recv().await {
                let on = Some(value[0] == 0x01);
                let prev = hub().snapshot().conversational_awareness;
                let ev = if prev.is_some() && prev != on { "ca" } else { "status" };
                hub().update(ev, |s| s.conversational_awareness = on);
                if let Some(handle) = &tray_handle_clone {
                    handle
                        .update(|tray: &mut MyTray| {
                            tray.conversation_detect_enabled = Some(value[0] == 0x01);
                        })
                        .await;
                }
            }
        });

        let (owns_connection_tx, mut owns_connection_rx) = tokio::sync::mpsc::unbounded_channel();
        aacp_manager
            .subscribe_to_control_command(
                ControlCommandIdentifiers::OwnsConnection,
                owns_connection_tx,
            )
            .await;
        let mc_clone_owns = media_controller.clone();
        tokio::spawn(async move {
            while let Some(value) = owns_connection_rx.recv().await {
                let owns = value.first().copied().unwrap_or(0) != 0;
                if !owns {
                    info!("Lost ownership, pausing media and disconnecting audio");
                    let controller = mc_clone_owns.lock().await;
                    controller.pause_all_media().await;
                    controller.deactivate_a2dp_profile().await;
                }
            }
        });

        let aacp_manager_clone_events = aacp_manager.clone();
        let announced_ev = announced.clone();
        let local_mac_events = local_mac.clone();
        let ui_tx_clone = ui_tx.clone();
        let command_tx_clone = command_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let event_clone = event.clone();
                match event {
                    AACPEvent::EarDetection(old_status, new_status) => {
                        debug!(
                            "Received EarDetection event: old_status={:?}, new_status={:?}",
                            old_status, new_status
                        );
                        // AACP reports [primary, secondary]; BLE tells which side leads.
                        let primary_left = hub().snapshot().primary_left.unwrap_or(true);
                        let ear = |i: usize| {
                            new_status.get(i).map(|st| *st == EarDetectionStatus::InEar)
                        };
                        let case = |i: usize| {
                            new_status.get(i).map(|st| *st == EarDetectionStatus::InCase)
                        };
                        let (pi, si) = if primary_left { (0, 1) } else { (1, 0) };
                        hub().update("status", |s| {
                            s.in_ear.left = ear(pi);
                            s.in_ear.right = ear(si);
                            s.in_case.left = case(pi);
                            s.in_case.right = case(si);
                        });
                        let controller = mc_clone.lock().await.clone();
                        debug!(
                            "Calling handle_ear_detection with old_status: {:?}, new_status: {:?}",
                            old_status, new_status
                        );
                        controller
                            .handle_ear_detection(old_status, new_status)
                            .await;
                    }
                    AACPEvent::BatteryInfo(battery_info) => {
                        debug!("Received BatteryInfo event: {:?}", battery_info);
                        hub().update("status", |s| {
                            for b in &battery_info {
                                let v = (b.status != BatteryStatus::Disconnected).then_some(Battery {
                                    level: b.level,
                                    charging: b.status == BatteryStatus::Charging,
                                });
                                match b.component as u8 {
                                    0x01 => s.headphone = v,
                                    0x02 => s.right = v,
                                    0x04 => s.left = v,
                                    // Out of the case the buds report it as
                                    // disconnected; keep the BLE reading then.
                                    0x08 => s.case = v.or(s.case),
                                    _ => {}
                                }
                            }
                        });
                        if !announced_ev.swap(true, Ordering::SeqCst) {
                            hub().announce_connected();
                        }
                        if let Some(handle) = &tray_handle {
                            handle
                                .update(|tray: &mut MyTray| {
                                    for b in &battery_info {
                                        match b.component as u8 {
                                            0x01 => {
                                                tray.battery_headphone = Some(b.level);
                                                tray.battery_headphone_status = Some(b.status);
                                            }
                                            0x02 => {
                                                tray.battery_r = Some(b.level);
                                                tray.battery_r_status = Some(b.status);
                                            }
                                            0x04 => {
                                                tray.battery_l = Some(b.level);
                                                tray.battery_l_status = Some(b.status);
                                            }
                                            0x08 => {
                                                tray.battery_c = Some(b.level);
                                                tray.battery_c_status = Some(b.status);
                                            }
                                            _ => {}
                                        }
                                    }
                                })
                                .await;
                        }
                        debug!("Updated tray with new battery info");

                        let _ = ui_tx_clone.send(BluetoothUIMessage::AACPUIEvent(
                            mac_address.to_string(),
                            event_clone,
                        ));
                        debug!("Sent BatteryInfo event to UI");
                    }
                    AACPEvent::ControlCommand(status) => {
                        debug!("Received ControlCommand event: {:?}", status);
                        let _ = ui_tx_clone.send(BluetoothUIMessage::AACPUIEvent(
                            mac_address.to_string(),
                            event_clone,
                        ));
                        debug!("Sent ControlCommand event to UI");
                    }
                    AACPEvent::ConversationalAwareness(status) => {
                        debug!("Received ConversationalAwareness event: {}", status);
                        let controller = mc_clone.lock().await;
                        controller.handle_conversational_awareness(status).await;
                    }
                    AACPEvent::ConnectedDevices(old_devices, new_devices) => {
                        let local_mac = local_mac_events.clone();
                        let new_devices_filtered = new_devices.iter().filter(|new_device| {
                            let not_in_old = old_devices
                                .iter()
                                .all(|old_device| old_device.mac != new_device.mac);
                            let not_local = new_device.mac != local_mac;
                            not_in_old && not_local
                        });

                        for device in new_devices_filtered {
                            info!(
                                "New connected device: {}, info1: {}, info2: {}",
                                device.mac, device.info1, device.info2
                            );
                            info!(
                                "Sending new Tipi packet for device {}, and sending media info to the device",
                                device.mac
                            );
                            let aacp_manager_clone = aacp_manager_clone_events.clone();
                            let local_mac_clone = local_mac.clone();
                            let device_mac_clone = device.mac.clone();
                            tokio::spawn(async move {
                                if let Err(e) = aacp_manager_clone
                                    .send_media_information_new_device(
                                        &local_mac_clone,
                                        &device_mac_clone,
                                    )
                                    .await
                                {
                                    error!("Failed to send media info new device: {}", e);
                                }
                                if let Err(e) = aacp_manager_clone
                                    .send_add_tipi_device(&local_mac_clone, &device_mac_clone)
                                    .await
                                {
                                    error!("Failed to send add tipi device: {}", e);
                                }
                            });
                        }
                    }
                    AACPEvent::OwnershipToFalseRequest => {
                        info!(
                            "Received ownership to false request. Setting ownership to false and pausing media."
                        );
                        let _ = command_tx_clone
                            .send((ControlCommandIdentifiers::OwnsConnection, vec![0x00]));
                        let controller = mc_clone.lock().await;
                        controller.pause_all_media().await;
                        controller.deactivate_a2dp_profile().await;
                    }
                    AACPEvent::StemPress(press_type, bud_type) => {
                        use crate::bluetooth::aacp::StemPressType;
                        info!(
                            "Received Stem Press: {:?} on {:?}",
                            press_type, bud_type
                        );
                        if stem_control {
                            let controller = mc_clone.lock().await;
                            match press_type {
                                StemPressType::DoublePress => {
                                    info!("Double press detected, skipping to next track");
                                    controller.next_track().await;
                                }
                                StemPressType::TriplePress => {
                                    info!("Triple press detected, going to previous track");
                                    controller.previous_track().await;
                                }
                                _ => {
                                    debug!("Unhandled stem press type: {:?}", press_type);
                                }
                            }
                        } else {
                            debug!("Stem control disabled, ignoring stem press event");
                        }
                    }
                    _ => {
                        debug!("Received unhandled AACP event: {:?}", event);
                        let _ = ui_tx_clone.send(BluetoothUIMessage::AACPUIEvent(
                            mac_address.to_string(),
                            event_clone,
                        ));
                        debug!("Sent unhandled AACP event to UI");
                    }
                }
            }
        });

        AirPodsDevice {
            mac_address,
            aacp_manager,
            // att_manager,
            media_controller,
            // command_tx: Some(command_tx.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AirPodsInformation {
    pub name: String,
    pub model_number: String,
    pub manufacturer: String,
    pub serial_number: String,
    pub version1: String,
    pub version2: String,
    pub hardware_revision: String,
    pub updater_identifier: String,
    pub left_serial_number: String,
    pub right_serial_number: String,
    pub version3: String,
    pub le_keys: AirPodsLEKeys,
}
