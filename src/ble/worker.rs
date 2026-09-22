use crate::ble::model::{
    AdapterInfo, BleCommand, BleEvent, CharacteristicInfo, CharacteristicKey,
    CharacteristicProperties, DescriptorInfo, DescriptorKey, DeviceInfo, GattSnapshot,
    ManufacturerDataEntry, ServiceDataEntry, ServiceInfo,
};
use anyhow::{Context, Result, anyhow};
use btleplug::{
    api::{
        Central, CentralEvent, CentralState, CharPropFlags, Characteristic, Descriptor,
        Manager as _, Peripheral as _, ScanFilter, WriteType,
    },
    platform::{Adapter, Manager, Peripheral},
};
use chrono::Local;
use futures::StreamExt;
use std::{future::pending, sync::mpsc::Sender, time::Duration};
use tokio::{
    sync::mpsc::UnboundedReceiver,
    task::JoinHandle,
    time::{Instant, sleep_until},
};
use tracing::{debug, info, warn};
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RECONNECT_DELAY_MS: u64 = 10_000;

pub async fn run(
    mut commands: UnboundedReceiver<BleCommand>,
    events: Sender<BleEvent>,
) -> Result<()> {
    let manager = Manager::new()
        .await
        .context("创建 Bluetooth Manager 失败")?;
    let adapters = manager
        .adapters()
        .await
        .context("枚举 Bluetooth Adapter 失败")?;

    if adapters.is_empty() {
        return Err(anyhow!("没有找到 Bluetooth Adapter"));
    }

    let adapter_infos = collect_adapter_infos(&adapters).await;
    let mut selected_adapter = 0usize;
    let mut adapter = adapters[selected_adapter].clone();
    let mut central_events = adapter
        .events()
        .await
        .context("创建 Bluetooth Adapter 事件流失败")?;

    emit(
        &events,
        BleEvent::Ready {
            adapters: adapter_infos.clone(),
            selected_adapter,
        },
    );

    let mut scanning = false;
    let mut peripheral: Option<Peripheral> = None;
    let mut connected = false;
    let mut notification_task: Option<JoinHandle<()>> = None;
    let mut target_id: Option<String> = None;
    let mut auto_reconnect = true;
    let mut reconnect_attempt = 0u32;
    let mut reconnect_deadline: Option<Instant> = None;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    info!("all BLE command senders dropped");
                    break;
                };

                match command {
                    BleCommand::SelectAdapter { index } => {
                        if index >= adapters.len() {
                            emit(&events, BleEvent::Error(format!("Adapter index {index} 不存在")));
                            continue;
                        }
                        if index == selected_adapter {
                            continue;
                        }

                        if scanning {
                            if let Err(error) = adapter.stop_scan().await {
                                warn!(%error, "failed to stop scan before adapter switch");
                            }
                            scanning = false;
                            emit(&events, BleEvent::ScanStopped);
                        }

                        reconnect_deadline = None;
                        reconnect_attempt = 0;
                        target_id = None;

                        if let Some(task) = notification_task.take() {
                            task.abort();
                        }
                        if let Some(current) = peripheral.as_ref()
                            && current.is_connected().await.unwrap_or(false) {
                                let _ = current.disconnect().await;
                            }
                        peripheral = None;
                        connected = false;
                        emit(
                            &events,
                            BleEvent::Disconnected {
                                peripheral_id: None,
                                unexpected: false,
                            },
                        );

                        let new_adapter = adapters[index].clone();
                        match new_adapter.events().await {
                            Ok(new_events) => {
                                selected_adapter = index;
                                adapter = new_adapter;
                                central_events = new_events;
                                emit(&events, BleEvent::DevicesCleared);
                                emit(
                                    &events,
                                    BleEvent::AdapterSelected {
                                        adapter: adapter_infos[index].clone(),
                                    },
                                );
                            }
                            Err(error) => {
                                emit(
                                    &events,
                                    BleEvent::Error(format!("切换 Adapter 失败: {error:#}")),
                                );
                            }
                        }
                    }
                    BleCommand::StartScan { service_uuids } => {
                        if !scanning {
                            let services = match parse_scan_services(&service_uuids) {
                                Ok(value) => value,
                                Err(error) => {
                                    emit(
                                        &events,
                                        BleEvent::Error(format!("扫描 Service UUID 无效: {error:#}")),
                                    );
                                    continue;
                                }
                            };
                            let filter = ScanFilter { services };
                            match adapter.start_scan(filter).await {
                                Ok(()) => {
                                    scanning = true;
                                    emit(&events, BleEvent::ScanStarted);
                                    if let Err(error) = emit_cached_devices(&adapter, &events).await {
                                        warn!(%error, "failed to emit cached BLE devices");
                                    }
                                }
                                Err(error) => emit(
                                    &events,
                                    BleEvent::Error(format!("启动 BLE 扫描失败: {error:#}")),
                                ),
                            }
                        }
                    }
                    BleCommand::StopScan => {
                        if scanning {
                            match adapter.stop_scan().await {
                                Ok(()) => {
                                    scanning = false;
                                    emit(&events, BleEvent::ScanStopped);
                                }
                                Err(error) => emit(
                                    &events,
                                    BleEvent::Error(format!("停止 BLE 扫描失败: {error:#}")),
                                ),
                            }
                        }
                    }
                    BleCommand::Connect { peripheral_id } => {
                        reconnect_deadline = None;
                        reconnect_attempt = 0;

                        let is_same_target = peripheral
                            .as_ref()
                            .is_some_and(|value| value.id().to_string() == peripheral_id);

                        if !is_same_target {
                            if let Some(task) = notification_task.take() {
                                task.abort();
                            }
                            if let Some(current) = peripheral.as_ref()
                                && current.is_connected().await.unwrap_or(false) {
                                    let _ = current.disconnect().await;
                                }
                            connected = false;
                            peripheral = None;

                            match find_peripheral(&adapter, &peripheral_id).await {
                                Ok(found) => peripheral = Some(found),
                                Err(error) => {
                                    emit(&events, BleEvent::Error(format!("{error:#}")));
                                    continue;
                                }
                            }
                        }

                        target_id = Some(peripheral_id.clone());
                        let Some(current) = peripheral.as_ref() else {
                            emit(&events, BleEvent::Error("目标 Peripheral 不可用".to_owned()));
                            continue;
                        };

                        match establish_connection(current, &events, false, 0).await {
                            Ok(()) => {
                                connected = true;
                                reconnect_attempt = 0;
                                if notification_task
                                    .as_ref()
                                    .is_none_or(|task| task.is_finished())
                                {
                                    notification_task = start_notification_forwarder(
                                        current.clone(),
                                        events.clone(),
                                    )
                                    .await;
                                }
                            }
                            Err(error) => {
                                connected = false;
                                emit(&events, BleEvent::Error(format!("连接失败: {error:#}")));
                                if auto_reconnect {
                                    schedule_reconnect(
                                        &events,
                                        &mut reconnect_attempt,
                                        &mut reconnect_deadline,
                                    );
                                }
                            }
                        }
                    }
                    BleCommand::Disconnect => {
                        reconnect_deadline = None;
                        reconnect_attempt = 0;
                        target_id = None;
                        if let Some(task) = notification_task.take() {
                            task.abort();
                        }
                        let id = peripheral.as_ref().map(|value| value.id().to_string());
                        if let Some(current) = peripheral.as_ref()
                            && current.is_connected().await.unwrap_or(false)
                                && let Err(error) = current.disconnect().await {
                                    emit(
                                        &events,
                                        BleEvent::Error(format!("断开 BLE 设备失败: {error:#}")),
                                    );
                                }
                        peripheral = None;
                        connected = false;
                        emit(
                            &events,
                            BleEvent::Disconnected {
                                peripheral_id: id,
                                unexpected: false,
                            },
                        );
                    }
                    BleCommand::SetAutoReconnect { enabled } => {
                        auto_reconnect = enabled;
                        if !enabled {
                            reconnect_deadline = None;
                            reconnect_attempt = 0;
                        } else if !connected && peripheral.is_some() && target_id.is_some() {
                            schedule_reconnect(
                                &events,
                                &mut reconnect_attempt,
                                &mut reconnect_deadline,
                            );
                        }
                        emit(
                            &events,
                            BleEvent::Status(if enabled {
                                crate::i18n::LocalizedText::new("自动重连已开启", &[])
                            } else {
                                crate::i18n::LocalizedText::new("自动重连已关闭", &[])
                            }),
                        );
                    }
                    BleCommand::SubscribeAll => {
                        if let Err(error) = subscribe_all(
                            connected.then_some(peripheral.as_ref()).flatten(),
                            &events,
                        )
                        .await
                        {
                            emit(&events, BleEvent::Error(format!("{error:#}")));
                        }
                    }
                    BleCommand::Subscribe { characteristic } => {
                        if let Err(error) = subscribe_one(
                            connected.then_some(peripheral.as_ref()).flatten(),
                            &events,
                            &characteristic,
                        )
                        .await
                        {
                            emit(&events, BleEvent::Error(format!("{error:#}")));
                        }
                    }
                    BleCommand::Read { characteristic } => {
                        if let Err(error) = read_characteristic(
                            connected.then_some(peripheral.as_ref()).flatten(),
                            &events,
                            &characteristic,
                        )
                        .await
                        {
                            emit(&events, BleEvent::Error(format!("{error:#}")));
                        }
                    }
                    BleCommand::Write {
                        characteristic,
                        data,
                        with_response,
                    } => {
                        if let Err(error) = write_characteristic(
                            connected.then_some(peripheral.as_ref()).flatten(),
                            &events,
                            &characteristic,
                            &data,
                            with_response,
                        )
                        .await
                        {
                            emit(&events, BleEvent::Error(format!("{error:#}")));
                        }
                    }
                    BleCommand::ReadDescriptor { descriptor } => {
                        if let Err(error) = read_descriptor(
                            connected.then_some(peripheral.as_ref()).flatten(),
                            &events,
                            &descriptor,
                        )
                        .await
                        {
                            emit(&events, BleEvent::Error(format!("{error:#}")));
                        }
                    }
                    BleCommand::WriteDescriptor { descriptor, data } => {
                        if let Err(error) = write_descriptor(
                            connected.then_some(peripheral.as_ref()).flatten(),
                            &events,
                            &descriptor,
                            &data,
                        )
                        .await
                        {
                            emit(&events, BleEvent::Error(format!("{error:#}")));
                        }
                    }
                    BleCommand::Shutdown => {
                        if scanning {
                            let _ = adapter.stop_scan().await;
                        }
                        if let Some(task) = notification_task.take() {
                            task.abort();
                        }
                        if let Some(current) = peripheral.as_ref()
                            && current.is_connected().await.unwrap_or(false) {
                                let _ = current.disconnect().await;
                            }
                        info!("BLE worker shutdown requested");
                        break;
                    }
                }
            }

            central_event = central_events.next() => {
                let Some(central_event) = central_event else {
                    return Err(anyhow!("Bluetooth Adapter 事件流已结束"));
                };

                match central_event {
                    CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) => {
                        if let Err(error) = emit_device_by_id(&adapter, &events, &id).await {
                            debug!(%error, "could not refresh BLE peripheral properties");
                        }
                    }
                    CentralEvent::RssiUpdate { id, rssi } => {
                        emit(
                            &events,
                            BleEvent::DeviceRssi {
                                peripheral_id: id.to_string(),
                                rssi,
                            },
                        );
                    }
                    CentralEvent::DeviceDisconnected(id) => {
                        let id_string = id.to_string();
                        let is_current = peripheral
                            .as_ref()
                            .is_some_and(|current| current.id().to_string() == id_string);

                        if is_current && connected {
                            connected = false;
                            emit(
                                &events,
                                BleEvent::Disconnected {
                                    peripheral_id: Some(id_string),
                                    unexpected: true,
                                },
                            );
                            if auto_reconnect && target_id.is_some() {
                                schedule_reconnect(
                                    &events,
                                    &mut reconnect_attempt,
                                    &mut reconnect_deadline,
                                );
                            }
                        }
                    }
                    CentralEvent::DeviceServicesModified(id) => {
                        let id_string = id.to_string();
                        if connected
                            && peripheral
                                .as_ref()
                                .is_some_and(|current| current.id().to_string() == id_string)
                            && let Some(current) = peripheral.as_ref() {
                                match current.discover_services_with_timeout(CONNECT_TIMEOUT).await {
                                    Ok(()) => emit(
                                        &events,
                                        BleEvent::GattDiscovered {
                                            snapshot: build_gatt_snapshot(current),
                                        },
                                    ),
                                    Err(error) => emit(
                                        &events,
                                        BleEvent::Error(format!("刷新 GATT 服务失败: {error:#}")),
                                    ),
                                }
                            }
                    }
                    CentralEvent::StateUpdate(state) => {
                        emit(
                            &events,
                            BleEvent::AdapterState {
                                state: central_state_label(&state).to_owned(),
                            },
                        );
                    }
                    CentralEvent::ManufacturerDataAdvertisement { id, .. }
                    | CentralEvent::ServiceDataAdvertisement { id, .. }
                    | CentralEvent::ServicesAdvertisement { id, .. } => {
                        if let Err(error) = emit_device_by_id(&adapter, &events, &id).await {
                            debug!(%error, "could not refresh advertisement snapshot");
                        }
                    }
                    CentralEvent::DeviceConnected(_) => {}
                }
            }

            _ = wait_for_reconnect(reconnect_deadline) => {
                reconnect_deadline = None;

                if !auto_reconnect || connected || target_id.is_none() {
                    continue;
                }

                if peripheral.is_none()
                    && let Some(id) = target_id.as_deref() {
                        match find_peripheral(&adapter, id).await {
                            Ok(found) => peripheral = Some(found),
                            Err(error) => {
                                emit(
                                    &events,
                                    BleEvent::ReconnectFailed {
                                        attempt: reconnect_attempt,
                                        error: format!("{error:#}"),
                                    },
                                );
                                schedule_reconnect(
                                    &events,
                                    &mut reconnect_attempt,
                                    &mut reconnect_deadline,
                                );
                                continue;
                            }
                        }
                    }

                let Some(current) = peripheral.as_ref() else {
                    continue;
                };

                let attempt = reconnect_attempt.max(1);
                match establish_connection(current, &events, true, attempt).await {
                    Ok(()) => {
                        connected = true;
                        reconnect_attempt = 0;
                        if notification_task
                            .as_ref()
                            .is_none_or(|task| task.is_finished())
                        {
                            notification_task = start_notification_forwarder(
                                current.clone(),
                                events.clone(),
                            )
                            .await;
                        }
                    }
                    Err(error) => {
                        connected = false;
                        emit(
                            &events,
                            BleEvent::ReconnectFailed {
                                attempt,
                                error: format!("{error:#}"),
                            },
                        );
                        schedule_reconnect(
                            &events,
                            &mut reconnect_attempt,
                            &mut reconnect_deadline,
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

async fn collect_adapter_infos(adapters: &[Adapter]) -> Vec<AdapterInfo> {
    let mut infos = Vec::with_capacity(adapters.len());

    for (index, adapter) in adapters.iter().enumerate() {
        let name = adapter
            .adapter_info()
            .await
            .unwrap_or_else(|_| format!("Bluetooth Adapter {index}"));
        let address = adapter
            .adapter_address()
            .await
            .ok()
            .flatten()
            .map(|value| value.to_string());
        let state = adapter
            .adapter_state()
            .await
            .map(|value| central_state_label(&value).to_owned())
            .unwrap_or_else(|_| "Unknown".to_owned());

        infos.push(AdapterInfo {
            index,
            name,
            address,
            state,
        });
    }

    infos
}

async fn emit_cached_devices(adapter: &Adapter, events: &Sender<BleEvent>) -> Result<()> {
    for peripheral in adapter
        .peripherals()
        .await
        .context("读取 Peripheral 缓存失败")?
    {
        if let Ok(device) = device_info(&peripheral).await {
            emit(events, BleEvent::DeviceUpsert { device });
        }
    }
    Ok(())
}

async fn emit_device_by_id(
    adapter: &Adapter,
    events: &Sender<BleEvent>,
    id: &btleplug::platform::PeripheralId,
) -> Result<()> {
    let peripheral = adapter
        .peripheral(id)
        .await
        .context("读取 Peripheral 失败")?;
    let device = device_info(&peripheral).await?;
    emit(events, BleEvent::DeviceUpsert { device });
    Ok(())
}

async fn device_info(peripheral: &Peripheral) -> Result<DeviceInfo> {
    let properties = peripheral.properties().await.unwrap_or(None);

    if let Some(properties) = properties {
        let name = properties
            .local_name
            .or(properties.advertisement_name)
            .unwrap_or_else(|| "(unknown)".to_owned());

        let mut manufacturer_data = properties
            .manufacturer_data
            .into_iter()
            .map(|(company_id, data)| ManufacturerDataEntry { company_id, data })
            .collect::<Vec<_>>();
        manufacturer_data.sort_by_key(|entry| entry.company_id);

        let mut service_data = properties
            .service_data
            .into_iter()
            .map(|(service_uuid, data)| ServiceDataEntry {
                service_uuid: service_uuid.to_string(),
                data,
            })
            .collect::<Vec<_>>();
        service_data.sort_by(|left, right| left.service_uuid.cmp(&right.service_uuid));

        let mut advertised_services = properties
            .services
            .into_iter()
            .map(|uuid| uuid.to_string())
            .collect::<Vec<_>>();
        advertised_services.sort();

        Ok(DeviceInfo {
            id: peripheral.id().to_string(),
            name,
            address: properties.address.to_string(),
            address_type: properties.address_type.map(|value| format!("{value:?}")),
            rssi: properties.rssi,
            tx_power: properties.tx_power_level,
            appearance: properties.appearance,
            manufacturer_data,
            service_data,
            advertised_services,
        })
    } else {
        Ok(DeviceInfo {
            id: peripheral.id().to_string(),
            name: "(unknown)".to_owned(),
            address: peripheral.address().to_string(),
            address_type: None,
            rssi: None,
            tx_power: None,
            appearance: None,
            manufacturer_data: Vec::new(),
            service_data: Vec::new(),
            advertised_services: Vec::new(),
        })
    }
}

async fn find_peripheral(adapter: &Adapter, target_id: &str) -> Result<Peripheral> {
    adapter
        .peripherals()
        .await
        .context("读取 Peripheral 列表失败")?
        .into_iter()
        .find(|peripheral| peripheral.id().to_string() == target_id)
        .ok_or_else(|| anyhow!("设备已不在 Peripheral 缓存中，请保持扫描后重试"))
}

async fn establish_connection(
    peripheral: &Peripheral,
    events: &Sender<BleEvent>,
    reconnect: bool,
    attempt: u32,
) -> Result<()> {
    emit(
        events,
        BleEvent::Connecting {
            peripheral_id: peripheral.id().to_string(),
            reconnect,
            attempt,
        },
    );

    if !peripheral.is_connected().await.unwrap_or(false) {
        peripheral
            .connect_with_timeout(CONNECT_TIMEOUT)
            .await
            .context("连接 BLE 设备失败")?;
    }

    peripheral
        .discover_services_with_timeout(CONNECT_TIMEOUT)
        .await
        .context("发现 GATT 服务失败")?;

    let name = peripheral
        .properties()
        .await
        .ok()
        .flatten()
        .and_then(|properties| properties.local_name.or(properties.advertisement_name))
        .unwrap_or_else(|| "(unknown)".to_owned());

    emit(
        events,
        BleEvent::Connected {
            peripheral_id: peripheral.id().to_string(),
            name,
            mtu: peripheral.mtu(),
        },
    );
    emit(
        events,
        BleEvent::GattDiscovered {
            snapshot: build_gatt_snapshot(peripheral),
        },
    );

    Ok(())
}

async fn start_notification_forwarder(
    peripheral: Peripheral,
    events: Sender<BleEvent>,
) -> Option<JoinHandle<()>> {
    let mut notifications = match peripheral.notifications().await {
        Ok(stream) => stream,
        Err(error) => {
            emit(
                &events,
                BleEvent::Error(format!("无法创建 Notify/Indicate 数据流: {error:#}")),
            );
            return None;
        }
    };

    Some(tokio::spawn(async move {
        while let Some(notification) = notifications.next().await {
            emit(
                &events,
                BleEvent::Notification {
                    characteristic: CharacteristicKey {
                        service_uuid: notification.service_uuid.to_string(),
                        characteristic_uuid: notification.uuid.to_string(),
                    },
                    data: notification.value,
                    timestamp: timestamp(),
                },
            );
        }
        debug!("notification stream ended");
    }))
}

fn schedule_reconnect(
    events: &Sender<BleEvent>,
    attempt: &mut u32,
    deadline: &mut Option<Instant>,
) {
    *attempt = (*attempt).saturating_add(1);
    let exponent = (*attempt).saturating_sub(1).min(4);
    let delay_ms = (1_000u64 << exponent).min(MAX_RECONNECT_DELAY_MS);
    *deadline = Some(Instant::now() + Duration::from_millis(delay_ms));
    emit(
        events,
        BleEvent::ReconnectScheduled {
            attempt: *attempt,
            delay_ms,
        },
    );
}

async fn wait_for_reconnect(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => pending::<()>().await,
    }
}

async fn subscribe_all(peripheral: Option<&Peripheral>, events: &Sender<BleEvent>) -> Result<()> {
    let peripheral = require_peripheral(peripheral)?;
    let mut count = 0usize;

    for characteristic in peripheral.characteristics() {
        if characteristic.properties.contains(CharPropFlags::NOTIFY)
            || characteristic.properties.contains(CharPropFlags::INDICATE)
        {
            match peripheral.subscribe(&characteristic).await {
                Ok(()) => {
                    count += 1;
                    emit(
                        events,
                        BleEvent::Subscribed {
                            characteristic: key_from_characteristic(&characteristic),
                        },
                    );
                }
                Err(error) => {
                    warn!(uuid = %characteristic.uuid, %error, "subscribe failed");
                }
            }
        }
    }

    emit(
        events,
        BleEvent::Status(crate::i18n::LocalizedText::new(
            "已订阅 {count} 个 Notify/Indicate Characteristic",
            &[count.to_string()],
        )),
    );
    Ok(())
}

async fn subscribe_one(
    peripheral: Option<&Peripheral>,
    events: &Sender<BleEvent>,
    key: &CharacteristicKey,
) -> Result<()> {
    let peripheral = require_peripheral(peripheral)?;
    let characteristic = find_characteristic(peripheral, key)?;

    peripheral
        .subscribe(&characteristic)
        .await
        .with_context(|| format!("订阅 {} 失败", characteristic.uuid))?;

    emit(
        events,
        BleEvent::Subscribed {
            characteristic: key.clone(),
        },
    );
    Ok(())
}

async fn read_characteristic(
    peripheral: Option<&Peripheral>,
    events: &Sender<BleEvent>,
    key: &CharacteristicKey,
) -> Result<()> {
    let peripheral = require_peripheral(peripheral)?;
    let characteristic = find_characteristic(peripheral, key)?;
    let data = peripheral
        .read(&characteristic)
        .await
        .with_context(|| format!("读取 {} 失败", characteristic.uuid))?;

    emit(
        events,
        BleEvent::ReadResult {
            characteristic: key.clone(),
            data,
            timestamp: timestamp(),
        },
    );
    Ok(())
}

async fn write_characteristic(
    peripheral: Option<&Peripheral>,
    events: &Sender<BleEvent>,
    key: &CharacteristicKey,
    data: &[u8],
    with_response: bool,
) -> Result<()> {
    let peripheral = require_peripheral(peripheral)?;
    let characteristic = find_characteristic(peripheral, key)?;
    let write_type = if with_response {
        WriteType::WithResponse
    } else {
        WriteType::WithoutResponse
    };

    peripheral
        .write(&characteristic, data, write_type)
        .await
        .with_context(|| format!("写入 {} 失败", characteristic.uuid))?;

    emit(
        events,
        BleEvent::WriteComplete {
            characteristic: key.clone(),
            data: data.to_vec(),
            timestamp: timestamp(),
        },
    );
    Ok(())
}

async fn read_descriptor(
    peripheral: Option<&Peripheral>,
    events: &Sender<BleEvent>,
    key: &DescriptorKey,
) -> Result<()> {
    let peripheral = require_peripheral(peripheral)?;
    let descriptor = find_descriptor(peripheral, key)?;
    let data = peripheral
        .read_descriptor(&descriptor)
        .await
        .with_context(|| format!("读取 Descriptor {} 失败", descriptor.uuid))?;

    emit(
        events,
        BleEvent::DescriptorReadResult {
            descriptor: key.clone(),
            data,
            timestamp: timestamp(),
        },
    );
    Ok(())
}

async fn write_descriptor(
    peripheral: Option<&Peripheral>,
    events: &Sender<BleEvent>,
    key: &DescriptorKey,
    data: &[u8],
) -> Result<()> {
    let peripheral = require_peripheral(peripheral)?;
    let descriptor = find_descriptor(peripheral, key)?;

    peripheral
        .write_descriptor(&descriptor, data)
        .await
        .with_context(|| format!("写入 Descriptor {} 失败", descriptor.uuid))?;

    emit(
        events,
        BleEvent::DescriptorWriteComplete {
            descriptor: key.clone(),
            data: data.to_vec(),
            timestamp: timestamp(),
        },
    );
    Ok(())
}

fn require_peripheral(peripheral: Option<&Peripheral>) -> Result<&Peripheral> {
    peripheral.ok_or_else(|| anyhow!("当前没有已连接 BLE 设备"))
}

fn find_characteristic(peripheral: &Peripheral, key: &CharacteristicKey) -> Result<Characteristic> {
    peripheral
        .characteristics()
        .into_iter()
        .find(|characteristic| {
            characteristic.service_uuid.to_string() == key.service_uuid
                && characteristic.uuid.to_string() == key.characteristic_uuid
        })
        .ok_or_else(|| {
            anyhow!(
                "找不到 Characteristic {} / {}",
                key.service_uuid,
                key.characteristic_uuid
            )
        })
}

fn find_descriptor(peripheral: &Peripheral, key: &DescriptorKey) -> Result<Descriptor> {
    let characteristic = peripheral
        .characteristics()
        .into_iter()
        .find(|characteristic| {
            characteristic.service_uuid.to_string() == key.service_uuid
                && characteristic.uuid.to_string() == key.characteristic_uuid
        })
        .ok_or_else(|| {
            anyhow!(
                "找不到 Descriptor 所属 Characteristic {} / {}",
                key.service_uuid,
                key.characteristic_uuid
            )
        })?;

    characteristic
        .descriptors
        .into_iter()
        .find(|descriptor| descriptor.uuid.to_string() == key.descriptor_uuid)
        .ok_or_else(|| anyhow!("找不到 Descriptor {}", key.descriptor_uuid))
}

fn build_gatt_snapshot(peripheral: &Peripheral) -> GattSnapshot {
    let services = peripheral
        .services()
        .into_iter()
        .map(|service| ServiceInfo {
            uuid: service.uuid.to_string(),
            primary: service.primary,
            characteristics: service
                .characteristics
                .into_iter()
                .map(|characteristic| {
                    let descriptors = characteristic
                        .descriptors
                        .iter()
                        .map(|descriptor| DescriptorInfo {
                            key: DescriptorKey {
                                service_uuid: descriptor.service_uuid.to_string(),
                                characteristic_uuid: descriptor.characteristic_uuid.to_string(),
                                descriptor_uuid: descriptor.uuid.to_string(),
                            },
                        })
                        .collect();

                    CharacteristicInfo {
                        key: key_from_characteristic(&characteristic),
                        properties: CharacteristicProperties {
                            read: characteristic.properties.contains(CharPropFlags::READ),
                            write: characteristic.properties.contains(CharPropFlags::WRITE),
                            write_without_response: characteristic
                                .properties
                                .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE),
                            notify: characteristic.properties.contains(CharPropFlags::NOTIFY),
                            indicate: characteristic.properties.contains(CharPropFlags::INDICATE),
                        },
                        descriptors,
                    }
                })
                .collect(),
        })
        .collect();

    GattSnapshot { services }
}

fn key_from_characteristic(characteristic: &Characteristic) -> CharacteristicKey {
    CharacteristicKey {
        service_uuid: characteristic.service_uuid.to_string(),
        characteristic_uuid: characteristic.uuid.to_string(),
    }
}

fn central_state_label(state: &CentralState) -> &'static str {
    match state {
        CentralState::Unknown => "Unknown",
        CentralState::PoweredOn => "Powered On",
        CentralState::PoweredOff => "Powered Off",
    }
}

fn parse_scan_services(values: &[String]) -> Result<Vec<Uuid>> {
    values
        .iter()
        .filter(|value| !value.trim().is_empty())
        .map(|value| parse_ble_uuid(value.trim()))
        .collect()
}

fn parse_ble_uuid(value: &str) -> Result<Uuid> {
    let compact = value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    match compact.len() {
        4 if compact.chars().all(|ch| ch.is_ascii_hexdigit()) => {
            let value = u16::from_str_radix(compact, 16)
                .with_context(|| format!("无法解析 16-bit UUID {value}"))?;
            Ok(btleplug::api::bleuuid::uuid_from_u16(value))
        }
        8 if compact.chars().all(|ch| ch.is_ascii_hexdigit()) => {
            let value = u32::from_str_radix(compact, 16)
                .with_context(|| format!("无法解析 32-bit UUID {value}"))?;
            Ok(btleplug::api::bleuuid::uuid_from_u32(value))
        }
        _ => Uuid::parse_str(value).with_context(|| format!("无法解析 UUID {value}")),
    }
}

fn emit(events: &Sender<BleEvent>, event: BleEvent) {
    let _ = events.send(event);
}

fn timestamp() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_short_and_full_scan_uuids() {
        let short = parse_ble_uuid("180D").unwrap();
        assert_eq!(short.to_string(), "0000180d-0000-1000-8000-00805f9b34fb");

        let full = parse_ble_uuid("0000180d-0000-1000-8000-00805f9b34fb").unwrap();
        assert_eq!(short, full);

        let short32 = parse_ble_uuid("12345678").unwrap();
        assert_eq!(short32.to_string(), "12345678-0000-1000-8000-00805f9b34fb");
    }
}
