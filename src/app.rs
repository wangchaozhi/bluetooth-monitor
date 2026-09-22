use crate::i18n::{self, Language, LocalizedText};
use crate::{
    ble::model::{
        AdapterInfo, BleCommand, BleEvent, CharacteristicInfo, CharacteristicKey, DescriptorInfo,
        DeviceInfo, GattSnapshot,
    },
    capture::{CapturePaths, CaptureRecord, CaptureSession, read_bmon},
    codec::{format_ascii, format_hex, parse_hex},
    plotting::{PlotChannel, PlotChannelConfig, PlotValueType, default_channels},
    profile::{self, DeviceProfile, StoredProfile},
    protocol::{
        CrcMode, CrcStatus, DecodedField, Endian, FieldDefinition, FrameMode, ProtocolConfig,
        ProtocolDecoder, StreamProtocolDecoder,
    },
    protocol_export::{self, ProtocolExportFrame},
    protocol_preset::{self, ProtocolPreset, StoredProtocolPreset},
    replay::{ReplayController, format_duration},
    workspace::{self, Bookmark, SessionKind, WorkspaceFile, WorkspaceLayout, WorkspaceSession},
};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};
use tokio::sync::mpsc::UnboundedSender;

const MAX_LOG_ENTRIES: usize = 20_000;
const MAX_PROTOCOL_FRAMES: usize = 5_000;
const MAX_TX_HISTORY: usize = 100;
const PREFERENCES_KEY: &str = "bluetooth-monitor.preferences";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct AppPreferences {
    language: Language,
    auto_scroll: bool,
    show_ascii: bool,
    write_with_response: bool,
    auto_reconnect: bool,
    show_rx: bool,
    show_read: bool,
    show_tx: bool,
    plot_max_points: usize,
    plot_channels: Vec<PlotChannelConfig>,
    protocol_config: ProtocolConfig,
    periodic_interval_ms: u64,
    scan_name_filter: String,
    scan_service_filter: String,
    scan_min_rssi: i16,
    layout: WorkspaceLayout,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            language: Language::default(),
            auto_scroll: true,
            show_ascii: true,
            write_with_response: true,
            auto_reconnect: true,
            show_rx: true,
            show_read: true,
            show_tx: true,
            plot_max_points: 2_000,
            plot_channels: default_channels(),
            protocol_config: ProtocolConfig::default(),
            periodic_interval_ms: 1_000,
            scan_name_filter: String::new(),
            scan_service_filter: String::new(),
            scan_min_rssi: -127,
            layout: WorkspaceLayout::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionPhase {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}

impl ConnectionPhase {
    fn label(self) -> &'static str {
        match self {
            Self::Disconnected => "Disconnected",
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Reconnecting => "Reconnecting",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogDirection {
    Rx,
    Read,
    Tx,
}

impl LogDirection {
    fn label(self) -> &'static str {
        match self {
            Self::Rx => "RX",
            Self::Read => "RD",
            Self::Tx => "TX",
        }
    }
}

#[derive(Debug, Clone)]
struct LogEntry {
    timestamp: String,
    direction: LogDirection,
    service_uuid: String,
    characteristic_uuid: String,
    data: Vec<u8>,
}

#[derive(Debug, Clone)]
struct ProtocolFrameEntry {
    sequence: u64,
    timestamp: String,
    data: Vec<u8>,
    crc: CrcStatus,
    fields: Vec<DecodedField>,
}

#[derive(Debug, Default, Clone, Copy)]
struct TrafficStats {
    rx_packets: u64,
    rx_bytes: u64,
    read_packets: u64,
    read_bytes: u64,
    tx_packets: u64,
    tx_bytes: u64,
    paused_rx_packets: u64,
}

impl TrafficStats {
    fn record(&mut self, direction: LogDirection, bytes: usize) {
        let bytes = bytes as u64;
        match direction {
            LogDirection::Rx => {
                self.rx_packets += 1;
                self.rx_bytes += bytes;
            }
            LogDirection::Read => {
                self.read_packets += 1;
                self.read_bytes += bytes;
            }
            LogDirection::Tx => {
                self.tx_packets += 1;
                self.tx_bytes += bytes;
            }
        }
    }
}

struct RuntimeSession {
    meta: WorkspaceSession,
    logs: VecDeque<LogEntry>,
    stats: TrafficStats,
    protocol_decoder: StreamProtocolDecoder,
    protocol_frames: VecDeque<ProtocolFrameEntry>,
    protocol_sequence: u64,
    replay: Option<ReplayController>,
}

impl RuntimeSession {
    fn new(meta: WorkspaceSession) -> Self {
        Self {
            meta,
            logs: VecDeque::new(),
            stats: TrafficStats::default(),
            protocol_decoder: StreamProtocolDecoder::default(),
            protocol_frames: VecDeque::new(),
            protocol_sequence: 0,
            replay: None,
        }
    }

    fn live(id: u64) -> Self {
        Self::new(WorkspaceSession {
            id,
            name: format!("Live {id}"),
            kind: SessionKind::Live,
            created_at: workspace::now_string(),
            ..WorkspaceSession::default()
        })
    }

    fn replay(id: u64, path: PathBuf, replay: ReplayController) -> Self {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Replay")
            .to_owned();
        let mut session = Self::new(WorkspaceSession {
            id,
            name,
            kind: SessionKind::Replay,
            source_path: Some(path),
            created_at: workspace::now_string(),
            ..WorkspaceSession::default()
        });
        session.replay = Some(replay);
        session
    }
}

pub struct BluetoothMonitorApp {
    language: Language,
    commands: UnboundedSender<BleCommand>,
    events: Receiver<BleEvent>,

    adapters: Vec<AdapterInfo>,
    selected_adapter: usize,
    adapter_state: String,
    status: LocalizedText,
    last_error: Option<LocalizedText>,

    scanning: bool,
    scan_name_filter: String,
    scan_service_filter: String,
    scan_min_rssi: i16,
    connection_phase: ConnectionPhase,
    auto_reconnect: bool,
    devices: Vec<DeviceInfo>,
    selected_device_id: Option<String>,
    target_id: Option<String>,
    connected_id: Option<String>,
    connected_name: Option<String>,
    mtu: Option<u16>,

    gatt: GattSnapshot,
    selected_characteristic: Option<CharacteristicInfo>,
    selected_descriptor: Option<DescriptorInfo>,

    sessions: Vec<RuntimeSession>,
    active_session: usize,
    live_session: usize,
    next_session_id: u64,
    next_bookmark_id: u64,
    workspace_name: String,
    workspace_path: Option<PathBuf>,
    layout: WorkspaceLayout,
    selected_protocol_frame_sequence: Option<u64>,
    monitor_paused: bool,
    filter: String,
    show_rx: bool,
    show_read: bool,
    show_tx: bool,

    tx_hex: String,
    descriptor_hex: String,
    write_with_response: bool,
    auto_scroll: bool,
    show_ascii: bool,

    plot_channels: Vec<PlotChannel>,
    plot_max_points: usize,

    protocol_source: Option<CharacteristicKey>,
    protocol_config: ProtocolConfig,
    protocol_delimiter_hex: String,
    protocol_presets: Vec<StoredProtocolPreset>,
    selected_protocol_preset: Option<usize>,
    protocol_preset_name: String,

    tx_history: VecDeque<String>,
    periodic_enabled: bool,
    periodic_interval_ms: u64,
    periodic_next: Option<Instant>,
    periodic_sent: u64,

    profiles: Vec<StoredProfile>,
    selected_profile_index: Option<usize>,
    profile_name: String,
    pending_profile_characteristic: Option<CharacteristicKey>,

    capture: Option<CaptureSession>,
    last_capture_paths: Option<CapturePaths>,
    capture_error: Option<LocalizedText>,
}

impl BluetoothMonitorApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        commands: UnboundedSender<BleCommand>,
        events: Receiver<BleEvent>,
    ) -> Self {
        cc.egui_ctx.set_zoom_factor(1.0);
        i18n::install_fonts(&cc.egui_ctx);
        let preferences = cc
            .storage
            .and_then(|storage| eframe::get_value::<AppPreferences>(storage, PREFERENCES_KEY))
            .unwrap_or_default();

        let language = preferences.language;
        let _ = commands.send(BleCommand::SetAutoReconnect {
            enabled: preferences.auto_reconnect,
        });
        let profiles = profile::load_profiles().unwrap_or_default();

        Self {
            language,
            commands,
            events,
            adapters: Vec::new(),
            selected_adapter: 0,
            adapter_state: "Unknown".to_owned(),
            status: LocalizedText::new("等待 Bluetooth Worker", &[]),
            last_error: None,
            scanning: false,
            scan_name_filter: preferences.scan_name_filter,
            scan_service_filter: preferences.scan_service_filter,
            scan_min_rssi: preferences.scan_min_rssi,
            connection_phase: ConnectionPhase::Disconnected,
            auto_reconnect: preferences.auto_reconnect,
            devices: Vec::new(),
            selected_device_id: None,
            target_id: None,
            connected_id: None,
            connected_name: None,
            mtu: None,
            gatt: GattSnapshot::default(),
            selected_characteristic: None,
            selected_descriptor: None,
            sessions: vec![RuntimeSession::live(1)],
            active_session: 0,
            live_session: 0,
            next_session_id: 2,
            next_bookmark_id: 1,
            workspace_name: "Untitled Workspace".to_owned(),
            workspace_path: None,
            layout: preferences.layout,
            selected_protocol_frame_sequence: None,
            monitor_paused: false,
            filter: String::new(),
            show_rx: preferences.show_rx,
            show_read: preferences.show_read,
            show_tx: preferences.show_tx,
            tx_hex: String::new(),
            descriptor_hex: String::new(),
            write_with_response: preferences.write_with_response,
            auto_scroll: preferences.auto_scroll,
            show_ascii: preferences.show_ascii,
            plot_channels: if preferences.plot_channels.is_empty() {
                default_channels()
            } else {
                preferences.plot_channels
            }
            .into_iter()
            .map(PlotChannel::new)
            .collect(),
            plot_max_points: preferences.plot_max_points.max(100),
            protocol_source: None,
            protocol_delimiter_hex: match &preferences.protocol_config.frame_mode {
                FrameMode::Delimiter { delimiter, .. } => format_hex(delimiter),
                _ => "0D 0A".to_owned(),
            },
            protocol_config: preferences.protocol_config,
            protocol_presets: protocol_preset::load_all(),
            selected_protocol_preset: None,
            protocol_preset_name: String::new(),
            tx_history: VecDeque::new(),
            periodic_enabled: false,
            periodic_interval_ms: preferences.periodic_interval_ms.max(100),
            periodic_next: None,
            periodic_sent: 0,
            profiles,
            selected_profile_index: None,
            profile_name: String::new(),
            pending_profile_characteristic: None,
            capture: None,
            last_capture_paths: None,
            capture_error: None,
        }
    }

    fn send(&mut self, command: BleCommand) {
        if self.commands.send(command).is_err() {
            self.last_error = Some(LocalizedText::new("Bluetooth Worker 已停止", &[]));
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            changed = true;
            match event {
                BleEvent::Ready {
                    adapters,
                    selected_adapter,
                } => {
                    self.adapters = adapters;
                    self.selected_adapter = selected_adapter;
                    if let Some(adapter) = self.adapters.get(selected_adapter) {
                        self.adapter_state = adapter.state.clone();
                        self.status = LocalizedText::new(
                            "Bluetooth 已就绪：{}",
                            std::slice::from_ref(&adapter.name),
                        );
                    } else {
                        self.status = LocalizedText::new("Bluetooth 已就绪", &[]);
                    }
                }
                BleEvent::AdapterSelected { adapter } => {
                    self.selected_adapter = adapter.index;
                    self.adapter_state = adapter.state;
                    self.status = LocalizedText::new(
                        "已切换 Adapter：{}",
                        std::slice::from_ref(&adapter.name),
                    );
                    self.connection_phase = ConnectionPhase::Disconnected;
                    self.reset_connection_view(true);
                }
                BleEvent::AdapterState { state } => {
                    self.adapter_state = state;
                }
                BleEvent::DevicesCleared => {
                    self.devices.clear();
                    self.selected_device_id = None;
                }
                BleEvent::ScanStarted => {
                    self.scanning = true;
                    self.status = LocalizedText::new("正在实时扫描 BLE 设备…", &[]);
                    self.last_error = None;
                }
                BleEvent::ScanStopped => {
                    self.scanning = false;
                    self.status = LocalizedText::new(
                        "扫描已停止：{} 个设备",
                        &[self.devices.len().to_string()],
                    );
                }
                BleEvent::DeviceUpsert { device } => {
                    self.upsert_device(device);
                }
                BleEvent::DeviceRssi {
                    peripheral_id,
                    rssi,
                } => {
                    if let Some(device) = self
                        .devices
                        .iter_mut()
                        .find(|device| device.id == peripheral_id)
                    {
                        device.rssi = Some(rssi);
                        self.sort_devices();
                    }
                }
                BleEvent::Connecting {
                    peripheral_id,
                    reconnect,
                    attempt,
                } => {
                    self.target_id = Some(peripheral_id.clone());
                    self.connection_phase = if reconnect {
                        ConnectionPhase::Reconnecting
                    } else {
                        ConnectionPhase::Connecting
                    };
                    self.last_error = None;
                    self.status = if reconnect {
                        LocalizedText::new(
                            "自动重连 #{attempt}：{peripheral_id}",
                            &[attempt.to_string(), peripheral_id.to_string()],
                        )
                    } else {
                        LocalizedText::new(
                            "正在连接 {peripheral_id}",
                            std::slice::from_ref(&peripheral_id),
                        )
                    };
                }
                BleEvent::Connected {
                    peripheral_id,
                    name,
                    mtu,
                } => {
                    self.connection_phase = ConnectionPhase::Connected;
                    self.target_id = Some(peripheral_id.clone());
                    self.connected_id = Some(peripheral_id.clone());
                    self.connected_name = Some(name.clone());
                    if let Some(session) = self.sessions.get_mut(self.live_session) {
                        session.meta.device_id = Some(peripheral_id.clone());
                        session.meta.device_name = Some(name.clone());
                        session.meta.name = format!("Live · {name}");
                    }
                    self.mtu = Some(mtu);
                    self.last_error = None;
                    self.status = LocalizedText::new(
                        "已连接 {name} · MTU {mtu}",
                        &[name.to_string(), mtu.to_string()],
                    );
                }
                BleEvent::Disconnected {
                    peripheral_id,
                    unexpected,
                } => {
                    tracing::debug!(?peripheral_id, unexpected, "BLE disconnected");
                    self.connected_id = None;
                    self.mtu = None;
                    if unexpected {
                        self.connection_phase = if self.auto_reconnect {
                            ConnectionPhase::Reconnecting
                        } else {
                            ConnectionPhase::Disconnected
                        };
                        self.status = LocalizedText::new("连接意外断开", &[]);
                    } else {
                        self.connection_phase = ConnectionPhase::Disconnected;
                        self.target_id = None;
                        self.reset_connection_view(true);
                        self.status = LocalizedText::new("已断开", &[]);
                    }
                }
                BleEvent::ReconnectScheduled { attempt, delay_ms } => {
                    self.connection_phase = ConnectionPhase::Reconnecting;
                    self.status = LocalizedText::new(
                        "将在 {:.1}s 后进行第 {attempt} 次重连",
                        &[
                            format!("{:.1}", delay_ms as f32 / 1000.0),
                            attempt.to_string(),
                        ],
                    );
                }
                BleEvent::ReconnectFailed { attempt, error } => {
                    self.connection_phase = ConnectionPhase::Reconnecting;
                    self.last_error = Some(LocalizedText::new(
                        "重连 #{attempt} 失败：{error}",
                        &[attempt.to_string(), error.to_string()],
                    ));
                }
                BleEvent::GattDiscovered { snapshot } => {
                    self.gatt = snapshot;
                    self.status = LocalizedText::new(
                        "GATT：{} 个 Service",
                        &[self.gatt.services.len().to_string()],
                    );
                    if let Some(key) = self.pending_profile_characteristic.take()
                        && let Some(characteristic) = self.find_characteristic_info(&key)
                    {
                        self.selected_characteristic = Some(characteristic);
                    }
                    self.send(BleCommand::SubscribeAll);
                }
                BleEvent::Subscribed { characteristic } => {
                    self.status = LocalizedText::new(
                        "已订阅 {}",
                        std::slice::from_ref(&characteristic.characteristic_uuid),
                    );
                }
                BleEvent::Notification {
                    characteristic,
                    data,
                    timestamp,
                } => {
                    self.ingest_live_log(LogEntry {
                        timestamp,
                        direction: LogDirection::Rx,
                        service_uuid: characteristic.service_uuid,
                        characteristic_uuid: characteristic.characteristic_uuid,
                        data,
                    });
                }
                BleEvent::ReadResult {
                    characteristic,
                    data,
                    timestamp,
                } => {
                    self.ingest_live_log(LogEntry {
                        timestamp,
                        direction: LogDirection::Read,
                        service_uuid: characteristic.service_uuid,
                        characteristic_uuid: characteristic.characteristic_uuid,
                        data,
                    });
                }
                BleEvent::WriteComplete {
                    characteristic,
                    data,
                    timestamp,
                } => {
                    self.ingest_live_log(LogEntry {
                        timestamp,
                        direction: LogDirection::Tx,
                        service_uuid: characteristic.service_uuid,
                        characteristic_uuid: characteristic.characteristic_uuid,
                        data,
                    });
                }
                BleEvent::DescriptorReadResult {
                    descriptor,
                    data,
                    timestamp,
                } => {
                    self.ingest_live_log(LogEntry {
                        timestamp,
                        direction: LogDirection::Read,
                        service_uuid: descriptor.service_uuid,
                        characteristic_uuid: format!(
                            "{} / desc {}",
                            descriptor.characteristic_uuid, descriptor.descriptor_uuid
                        ),
                        data,
                    });
                }
                BleEvent::DescriptorWriteComplete {
                    descriptor,
                    data,
                    timestamp,
                } => {
                    self.ingest_live_log(LogEntry {
                        timestamp,
                        direction: LogDirection::Tx,
                        service_uuid: descriptor.service_uuid,
                        characteristic_uuid: format!(
                            "{} / desc {}",
                            descriptor.characteristic_uuid, descriptor.descriptor_uuid
                        ),
                        data,
                    });
                }
                BleEvent::Status(status) => self.status = status,
                BleEvent::Error(error) => {
                    self.last_error = Some(error.into());
                }
                BleEvent::Fatal(error) => {
                    self.scanning = false;
                    self.connection_phase = ConnectionPhase::Disconnected;
                    self.status = LocalizedText::new("Bluetooth Worker 已停止", &[]);
                    self.last_error = Some(error.into());
                }
            }
        }

        if changed {
            ctx.request_repaint();
        }
    }

    fn reset_connection_view(&mut self, clear_gatt: bool) {
        self.connected_id = None;
        self.connected_name = None;
        self.mtu = None;
        if clear_gatt {
            self.gatt = GattSnapshot::default();
            self.selected_characteristic = None;
            self.selected_descriptor = None;
        }
    }

    fn upsert_device(&mut self, device: DeviceInfo) {
        if let Some(existing) = self
            .devices
            .iter_mut()
            .find(|existing| existing.id == device.id)
        {
            *existing = device;
        } else {
            if self.selected_device_id.is_none() {
                self.selected_device_id = Some(device.id.clone());
            }
            self.devices.push(device);
        }
        self.sort_devices();
    }

    fn sort_devices(&mut self) {
        self.devices.sort_by(|left, right| {
            right
                .rssi
                .unwrap_or(i16::MIN)
                .cmp(&left.rssi.unwrap_or(i16::MIN))
                .then_with(|| left.name.cmp(&right.name))
        });
    }

    fn active_runtime_session(&self) -> &RuntimeSession {
        &self.sessions[self.active_session]
    }

    fn active_runtime_session_mut(&mut self) -> &mut RuntimeSession {
        &mut self.sessions[self.active_session]
    }

    fn reset_active_protocol(&mut self) {
        let session = self.active_runtime_session_mut();
        session.protocol_decoder.reset();
        session.protocol_frames.clear();
        session.protocol_sequence = 0;
        self.selected_protocol_frame_sequence = None;
    }

    fn ingest_live_log(&mut self, entry: LogEntry) {
        let index = self.live_session;
        self.ingest_log_inner(index, entry, true);
    }

    fn ingest_log_inner(&mut self, session_index: usize, entry: LogEntry, write_capture: bool) {
        if session_index >= self.sessions.len() {
            return;
        }

        self.sessions[session_index]
            .stats
            .record(entry.direction, entry.data.len());

        if session_index == self.active_session {
            self.capture_plot_sample(&entry);
        }
        self.capture_protocol_frames(session_index, &entry);

        if write_capture {
            let capture_error = self.capture.as_mut().and_then(|capture| {
                let record = CaptureRecord {
                    timestamp: &entry.timestamp,
                    direction: entry.direction.label(),
                    service_uuid: &entry.service_uuid,
                    characteristic_uuid: &entry.characteristic_uuid,
                    data: &entry.data,
                };
                capture.write(record).err()
            });
            if let Some(error) = capture_error {
                self.capture_error = Some(LocalizedText::new(
                    "捕获写入失败：{error:#}",
                    &[format!("{:#}", error)],
                ));
                self.capture = None;
            }
        }

        if session_index == self.active_session
            && self.monitor_paused
            && entry.direction == LogDirection::Rx
        {
            self.sessions[session_index].stats.paused_rx_packets = self.sessions[session_index]
                .stats
                .paused_rx_packets
                .saturating_add(1);
            return;
        }

        let logs = &mut self.sessions[session_index].logs;
        logs.push_back(entry);
        while logs.len() > MAX_LOG_ENTRIES {
            logs.pop_front();
        }
    }

    fn capture_plot_sample(&mut self, entry: &LogEntry) {
        if entry.direction != LogDirection::Rx {
            return;
        }
        let key = CharacteristicKey {
            service_uuid: entry.service_uuid.clone(),
            characteristic_uuid: entry.characteristic_uuid.clone(),
        };
        for channel in &mut self.plot_channels {
            channel.ingest(&key, &entry.data, self.plot_max_points);
        }
    }

    fn capture_protocol_frames(&mut self, session_index: usize, entry: &LogEntry) {
        if entry.direction != LogDirection::Rx || !self.protocol_config.enabled {
            return;
        }
        let Some(source) = self.protocol_source.as_ref() else {
            return;
        };
        if entry.service_uuid != source.service_uuid
            || entry.characteristic_uuid != source.characteristic_uuid
        {
            return;
        }

        let config = self.protocol_config.clone();
        let session = &mut self.sessions[session_index];
        session.protocol_decoder.configure(&config);
        for frame in session.protocol_decoder.push(&entry.data) {
            session.protocol_sequence = session.protocol_sequence.saturating_add(1);
            session.protocol_frames.push_back(ProtocolFrameEntry {
                sequence: session.protocol_sequence,
                timestamp: entry.timestamp.clone(),
                data: frame.data,
                crc: frame.crc,
                fields: frame.fields,
            });
        }
        while session.protocol_frames.len() > MAX_PROTOCOL_FRAMES {
            session.protocol_frames.pop_front();
        }
    }

    fn remember_tx(&mut self, value: &str) {
        let value = value.trim();
        if value.is_empty() {
            return;
        }
        if self
            .tx_history
            .front()
            .is_some_and(|existing| existing == value)
        {
            return;
        }
        self.tx_history.retain(|existing| existing != value);
        self.tx_history.push_front(value.to_owned());
        while self.tx_history.len() > MAX_TX_HISTORY {
            self.tx_history.pop_back();
        }
    }

    fn tick_periodic_send(&mut self, ctx: &egui::Context) {
        if !self.periodic_enabled {
            self.periodic_next = None;
            return;
        }

        let interval = Duration::from_millis(self.periodic_interval_ms.max(100));
        let now = Instant::now();
        let deadline = match self.periodic_next {
            Some(deadline) => deadline,
            None => {
                let deadline = now + interval;
                self.periodic_next = Some(deadline);
                ctx.request_repaint_after(interval);
                return;
            }
        };

        if now < deadline {
            ctx.request_repaint_after(deadline.saturating_duration_since(now));
            return;
        }

        self.periodic_next = Some(now + interval);
        ctx.request_repaint_after(interval);

        if self.connected_id.is_none() {
            return;
        }
        let Some(characteristic) = self.selected_characteristic.clone() else {
            return;
        };
        if !(characteristic.properties.write || characteristic.properties.write_without_response) {
            return;
        }
        let Ok(data) = parse_hex(&self.tx_hex) else {
            return;
        };
        if data.is_empty() {
            return;
        }

        let tx = self.tx_hex.clone();
        let with_response = if characteristic.properties.write
            && characteristic.properties.write_without_response
        {
            self.write_with_response
        } else {
            characteristic.properties.write
        };
        self.send(BleCommand::Write {
            characteristic: characteristic.key,
            data,
            with_response,
        });
        self.remember_tx(&tx);
        self.periodic_sent = self.periodic_sent.saturating_add(1);
    }

    fn start_capture(&mut self) {
        match CaptureSession::start_default() {
            Ok(session) => {
                self.last_capture_paths = Some(session.paths().clone());
                if let Some(live) = self.sessions.get_mut(self.live_session) {
                    live.meta.source_path = Some(session.paths().raw.clone());
                }
                self.capture = Some(session);
                self.capture_error = None;
                self.status = LocalizedText::new("已开始 CSV + BMON 捕获", &[]);
            }
            Err(error) => {
                self.capture_error = Some(LocalizedText::new(
                    "无法开始捕获：{error:#}",
                    &[format!("{:#}", error)],
                ));
            }
        }
    }

    fn stop_capture(&mut self) {
        if let Some(mut capture) = self.capture.take() {
            let records = capture.records();
            match capture.flush() {
                Ok(()) => {
                    self.status =
                        LocalizedText::new("捕获已保存：{records} records", &[records.to_string()]);
                }
                Err(error) => {
                    self.capture_error = Some(LocalizedText::new(
                        "刷新捕获文件失败：{error:#}",
                        &[format!("{:#}", error)],
                    ));
                }
            }
        }
    }

    fn find_characteristic_info(&self, key: &CharacteristicKey) -> Option<CharacteristicInfo> {
        self.gatt
            .services
            .iter()
            .flat_map(|service| service.characteristics.iter())
            .find(|characteristic| characteristic.key == *key)
            .cloned()
    }

    fn selected_device(&self) -> Option<&DeviceInfo> {
        let id = self.selected_device_id.as_deref()?;
        self.devices.iter().find(|device| device.id == id)
    }

    fn open_replay(&mut self) {
        let language = self.language;
        let Some(path) = rfd::FileDialog::new()
            .add_filter(language.text("Bluetooth Monitor capture"), &["bmon"])
            .pick_file()
        else {
            return;
        };

        match read_bmon(&path) {
            Ok(records) => {
                let count = records.len();
                let id = self.next_session_id;
                self.next_session_id = self.next_session_id.saturating_add(1);
                let replay = ReplayController::new(path.clone(), records);
                self.sessions
                    .push(RuntimeSession::replay(id, path.clone(), replay));
                self.active_session = self.sessions.len() - 1;
                self.clear_plot_samples();
                self.status = LocalizedText::new(
                    "已载入 {count} 条回放记录：{}",
                    &[count.to_string(), path.display().to_string()],
                );
            }
            Err(error) => {
                self.last_error = Some(LocalizedText::new(
                    "无法读取 BMON：{error:#}",
                    &[format!("{:#}", error)],
                ));
            }
        }
    }

    fn ingest_replay_record(&mut self, session_index: usize, record: crate::capture::ReplayRecord) {
        let direction = match record.direction.as_str() {
            "RX" => LogDirection::Rx,
            "RD" => LogDirection::Read,
            "TX" => LogDirection::Tx,
            _ => return,
        };
        self.ingest_log_inner(
            session_index,
            LogEntry {
                timestamp: record.timestamp,
                direction,
                service_uuid: record.service_uuid,
                characteristic_uuid: record.characteristic_uuid,
                data: record.data,
            },
            false,
        );
    }

    fn tick_replay(&mut self, ctx: &egui::Context) {
        let session_index = self.active_session;
        let due = self.sessions[session_index]
            .replay
            .as_mut()
            .map(ReplayController::tick)
            .unwrap_or_default();
        for record in due {
            self.ingest_replay_record(session_index, record);
        }
        if self.sessions[session_index]
            .replay
            .as_ref()
            .is_some_and(ReplayController::is_playing)
        {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn clear_plot_samples(&mut self) {
        for channel in &mut self.plot_channels {
            channel.clear();
        }
    }

    fn scan_service_tokens(&self) -> Vec<String> {
        self.scan_service_filter
            .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';'))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    fn device_matches_scan_filter(&self, device: &DeviceInfo) -> bool {
        match device.rssi {
            Some(rssi) if rssi < self.scan_min_rssi => return false,
            None if self.scan_min_rssi > -127 => return false,
            _ => {}
        }

        let query = self.scan_name_filter.trim().to_ascii_lowercase();
        if !query.is_empty()
            && !device.name.to_ascii_lowercase().contains(&query)
            && !device.address.to_ascii_lowercase().contains(&query)
            && !device.id.to_ascii_lowercase().contains(&query)
        {
            return false;
        }

        let service_queries = self.scan_service_tokens();
        if !service_queries.is_empty()
            && !service_queries.iter().any(|query| {
                device
                    .advertised_services
                    .iter()
                    .any(|service| ble_uuid_matches(service, query))
                    || device
                        .service_data
                        .iter()
                        .any(|entry| ble_uuid_matches(&entry.service_uuid, query))
            })
        {
            return false;
        }

        true
    }

    fn protocol_export_frames(&self) -> Vec<ProtocolExportFrame> {
        self.active_runtime_session()
            .protocol_frames
            .iter()
            .map(|frame| ProtocolExportFrame {
                sequence: frame.sequence,
                timestamp: frame.timestamp.clone(),
                crc: frame.crc.label().to_owned(),
                hex: format_hex(&frame.data),
                fields: frame
                    .fields
                    .iter()
                    .map(|field| (field.name.clone(), field.value))
                    .collect::<BTreeMap<_, _>>(),
            })
            .collect()
    }

    fn export_protocol_csv(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name("protocol-frames.csv")
            .save_file()
        else {
            return;
        };
        let frames = self.protocol_export_frames();
        match protocol_export::export_csv(&path, &frames) {
            Ok(()) => {
                self.status =
                    LocalizedText::new("协议字段 CSV 已导出：{}", &[path.display().to_string()])
            }
            Err(error) => {
                self.last_error = Some(LocalizedText::new(
                    "导出协议 CSV 失败：{error:#}",
                    &[format!("{:#}", error)],
                ))
            }
        }
    }

    fn export_protocol_json(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .set_file_name("protocol-frames.json")
            .save_file()
        else {
            return;
        };
        let frames = self.protocol_export_frames();
        match protocol_export::export_json(&path, &frames) {
            Ok(()) => {
                self.status =
                    LocalizedText::new("协议字段 JSON 已导出：{}", &[path.display().to_string()])
            }
            Err(error) => {
                self.last_error = Some(LocalizedText::new(
                    "导出协议 JSON 失败：{error:#}",
                    &[format!("{:#}", error)],
                ))
            }
        }
    }

    fn apply_protocol_preset(&mut self, index: usize) {
        let Some(stored) = self.protocol_presets.get(index).cloned() else {
            return;
        };
        self.selected_protocol_preset = Some(index);
        self.protocol_preset_name = stored.preset.name.clone();
        self.protocol_config = stored.preset.config;
        self.protocol_delimiter_hex = match &self.protocol_config.frame_mode {
            FrameMode::Delimiter { delimiter, .. } => format_hex(delimiter),
            _ => self.protocol_delimiter_hex.clone(),
        };
        self.reset_active_protocol();
        self.status = LocalizedText::new(
            "已应用协议预设：{}",
            std::slice::from_ref(&stored.preset.name),
        );
    }

    fn save_protocol_preset(&mut self) {
        let name = if self.protocol_preset_name.trim().is_empty() {
            "Custom Protocol".to_owned()
        } else {
            self.protocol_preset_name.trim().to_owned()
        };
        let preset = ProtocolPreset {
            name: name.clone(),
            description: "用户保存的协议配置".to_owned(),
            config: self.protocol_config.clone(),
            saved_at: String::new(),
        };
        match protocol_preset::save_custom(preset) {
            Ok(path) => {
                self.protocol_presets = protocol_preset::load_all();
                self.selected_protocol_preset = self
                    .protocol_presets
                    .iter()
                    .position(|stored| stored.path.as_ref() == Some(&path));
                self.protocol_preset_name = name;
                self.status =
                    LocalizedText::new("协议预设已保存：{}", &[path.display().to_string()]);
            }
            Err(error) => {
                self.last_error = Some(LocalizedText::new(
                    "保存协议预设失败：{error:#}",
                    &[format!("{:#}", error)],
                ));
            }
        }
    }

    fn save_current_profile(&mut self) {
        let device = self.selected_device().cloned();
        let name = if self.profile_name.trim().is_empty() {
            device
                .as_ref()
                .map(|value| value.name.clone())
                .filter(|value| value != "(unknown)")
                .unwrap_or_else(|| "BLE Profile".to_owned())
        } else {
            self.profile_name.trim().to_owned()
        };

        let profile = DeviceProfile {
            name: name.clone(),
            device_id: device.as_ref().map(|value| value.id.clone()),
            address: device.as_ref().map(|value| value.address.clone()),
            characteristic: self
                .selected_characteristic
                .as_ref()
                .map(|value| value.key.clone()),
            auto_reconnect: self.auto_reconnect,
            write_with_response: self.write_with_response,
            tx_hex: self.tx_hex.clone(),
            protocol_source: self.protocol_source.clone(),
            protocol: self.protocol_config.clone(),
            plot_channels: self
                .plot_channels
                .iter()
                .map(|channel| channel.config.clone())
                .collect(),
            periodic_interval_ms: self.periodic_interval_ms,
            tx_history: self.tx_history.iter().cloned().collect(),
            scan_name_filter: self.scan_name_filter.clone(),
            scan_service_filter: self.scan_service_filter.clone(),
            scan_min_rssi: self.scan_min_rssi,
            saved_at: String::new(),
        };

        match profile::save_profile(profile) {
            Ok(path) => {
                self.profile_name = name;
                self.profiles = profile::load_profiles().unwrap_or_default();
                self.selected_profile_index =
                    self.profiles.iter().position(|stored| stored.path == path);
                self.status =
                    LocalizedText::new("Profile 已保存：{}", &[path.display().to_string()]);
            }
            Err(error) => {
                self.last_error = Some(LocalizedText::new(
                    "保存 Profile 失败：{error:#}",
                    &[format!("{:#}", error)],
                ));
            }
        }
    }

    fn apply_profile(&mut self, index: usize) {
        let Some(stored) = self.profiles.get(index).cloned() else {
            return;
        };
        let profile = stored.profile;
        self.selected_profile_index = Some(index);
        self.profile_name = profile.name.clone();
        self.tx_hex = profile.tx_hex;
        self.write_with_response = profile.write_with_response;
        self.pending_profile_characteristic = profile.characteristic.clone();
        self.protocol_source = profile.protocol_source.clone();
        self.protocol_config = profile.protocol;
        self.reset_active_protocol();
        if !profile.plot_channels.is_empty() {
            self.plot_channels = profile
                .plot_channels
                .into_iter()
                .map(PlotChannel::new)
                .collect();
        }
        self.periodic_interval_ms = profile.periodic_interval_ms.max(100);
        self.tx_history = profile
            .tx_history
            .into_iter()
            .take(MAX_TX_HISTORY)
            .collect();
        self.scan_name_filter = profile.scan_name_filter;
        self.scan_service_filter = profile.scan_service_filter;
        self.scan_min_rssi = profile.scan_min_rssi;

        if self.auto_reconnect != profile.auto_reconnect {
            self.auto_reconnect = profile.auto_reconnect;
            self.send(BleCommand::SetAutoReconnect {
                enabled: self.auto_reconnect,
            });
        }

        if let Some(device) = self.devices.iter().find(|device| {
            profile.device_id.as_deref() == Some(device.id.as_str())
                || profile.address.as_deref() == Some(device.address.as_str())
        }) {
            self.selected_device_id = Some(device.id.clone());
        }

        if let Some(key) = profile.characteristic
            && let Some(characteristic) = self.find_characteristic_info(&key)
        {
            self.selected_characteristic = Some(characteristic);
            self.pending_profile_characteristic = None;
        }

        self.status = LocalizedText::new("已加载 Profile：{}", std::slice::from_ref(&profile.name));
    }

    fn ensure_live_session_for_device(&mut self, device_id: &str, device_name: Option<&str>) {
        let rotate = self.sessions.get(self.live_session).is_some_and(|session| {
            session
                .meta
                .device_id
                .as_deref()
                .is_some_and(|current| current != device_id)
                && (!session.logs.is_empty() || !session.protocol_frames.is_empty())
        });
        if rotate {
            if let Some(previous) = self.sessions.get_mut(self.live_session) {
                previous.meta.kind = SessionKind::Capture;
                if let Some(name) = previous.meta.device_name.as_deref() {
                    previous.meta.name = format!("Capture · {name}");
                }
            }
            let id = self.next_session_id;
            self.next_session_id = self.next_session_id.saturating_add(1);
            self.sessions.push(RuntimeSession::live(id));
            self.live_session = self.sessions.len() - 1;
        }
        if let Some(session) = self.sessions.get_mut(self.live_session) {
            session.meta.device_id = Some(device_id.to_owned());
            if let Some(name) = device_name {
                session.meta.device_name = Some(name.to_owned());
                session.meta.name = format!("Live · {name}");
            }
        }
        self.active_session = self.live_session;
        self.selected_protocol_frame_sequence = None;
        self.clear_plot_samples();
    }

    fn workspace_snapshot(&self) -> WorkspaceFile {
        WorkspaceFile {
            version: workspace::WORKSPACE_VERSION,
            name: self.workspace_name.clone(),
            saved_at: String::new(),
            active_session_id: self
                .sessions
                .get(self.active_session)
                .map(|session| session.meta.id),
            sessions: self
                .sessions
                .iter()
                .map(|session| session.meta.clone())
                .collect(),
            layout: self.layout.clone(),
        }
    }

    fn save_workspace_to(&mut self, path: PathBuf) {
        let snapshot = self.workspace_snapshot();
        match workspace::save_workspace(&path, &snapshot) {
            Ok(()) => {
                self.workspace_path = Some(path.clone());
                self.status =
                    LocalizedText::new("Workspace 已保存：{}", &[path.display().to_string()]);
            }
            Err(error) => {
                self.last_error = Some(LocalizedText::new(
                    "保存 Workspace 失败：{error:#}",
                    &[format!("{:#}", error)],
                ))
            }
        }
    }

    fn save_workspace(&mut self, force_dialog: bool) {
        let language = self.language;
        if !force_dialog && let Some(path) = self.workspace_path.clone() {
            self.save_workspace_to(path);
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter(language.text("Bluetooth Monitor workspace"), &["json"])
            .set_file_name("bluetooth-workspace.bmw.json")
            .save_file()
        else {
            return;
        };
        self.save_workspace_to(path);
    }

    fn open_workspace(&mut self) {
        let language = self.language;
        let Some(path) = rfd::FileDialog::new()
            .add_filter(language.text("Bluetooth Monitor workspace"), &["json"])
            .pick_file()
        else {
            return;
        };
        match workspace::load_workspace(&path) {
            Ok(file) => self.apply_workspace(path, file),
            Err(error) => {
                self.last_error = Some(LocalizedText::new(
                    "打开 Workspace 失败：{error:#}",
                    &[format!("{:#}", error)],
                ))
            }
        }
    }

    fn apply_workspace(&mut self, path: PathBuf, file: WorkspaceFile) {
        self.stop_capture();
        self.send(BleCommand::Disconnect);

        let saved_at = file.saved_at.clone();
        let active_id = file.active_session_id;
        let mut sessions = Vec::new();
        for meta in file.sessions {
            let mut runtime = if matches!(meta.kind, SessionKind::Replay | SessionKind::Capture) {
                match meta.source_path.clone().and_then(|source| {
                    read_bmon(&source)
                        .ok()
                        .map(|records| (source.clone(), ReplayController::new(source, records)))
                }) {
                    Some((source, replay)) => RuntimeSession::replay(meta.id, source, replay),
                    None => RuntimeSession::new(meta.clone()),
                }
            } else {
                RuntimeSession::new(meta.clone())
            };
            runtime.meta = meta;
            sessions.push(runtime);
        }
        if sessions.is_empty() {
            sessions.push(RuntimeSession::live(1));
        }
        let live_session = if let Some(index) = sessions
            .iter()
            .position(|session| session.meta.kind == SessionKind::Live)
        {
            index
        } else {
            let id = sessions
                .iter()
                .map(|session| session.meta.id)
                .max()
                .unwrap_or(0)
                + 1;
            sessions.push(RuntimeSession::live(id));
            sessions.len() - 1
        };
        let active_session = active_id
            .and_then(|id| sessions.iter().position(|session| session.meta.id == id))
            .unwrap_or(live_session);
        let max_session_id = sessions
            .iter()
            .map(|session| session.meta.id)
            .max()
            .unwrap_or(0);
        let max_bookmark_id = sessions
            .iter()
            .flat_map(|session| session.meta.bookmarks.iter())
            .map(|bookmark| bookmark.id)
            .max()
            .unwrap_or(0);

        self.sessions = sessions;
        self.live_session = live_session;
        self.active_session = active_session;
        self.next_session_id = max_session_id.saturating_add(1);
        self.next_bookmark_id = max_bookmark_id.saturating_add(1);
        self.workspace_name = file.name;
        self.workspace_path = Some(path.clone());
        self.layout = file.layout;
        self.selected_protocol_frame_sequence = None;
        self.clear_plot_samples();
        self.status = if saved_at.is_empty() {
            LocalizedText::new("Workspace 已打开：{}", &[path.display().to_string()])
        } else {
            LocalizedText::new(
                "Workspace 已打开：{} · saved {saved_at}",
                &[path.display().to_string(), saved_at.to_string()],
            )
        };
    }

    fn close_session(&mut self, index: usize) {
        if index >= self.sessions.len() || index == self.live_session {
            return;
        }
        self.sessions.remove(index);
        if self.live_session > index {
            self.live_session -= 1;
        }
        if self.active_session == index {
            self.active_session = self.live_session;
        } else if self.active_session > index {
            self.active_session -= 1;
        }
        self.selected_protocol_frame_sequence = None;
        self.clear_plot_samples();
    }

    fn add_log_bookmark(&mut self, entry: &LogEntry) {
        let replay_position_ms = self
            .active_runtime_session()
            .replay
            .as_ref()
            .map(ReplayController::position_ms);
        let id = self.next_bookmark_id;
        self.next_bookmark_id = self.next_bookmark_id.saturating_add(1);
        self.active_runtime_session_mut()
            .meta
            .bookmarks
            .push(Bookmark {
                id,
                label: format!("{} {}", entry.direction.label(), entry.timestamp),
                timestamp: entry.timestamp.clone(),
                source: "log".to_owned(),
                sequence: None,
                replay_position_ms,
                service_uuid: entry.service_uuid.clone(),
                characteristic_uuid: entry.characteristic_uuid.clone(),
                hex: format_hex(&entry.data),
            });
    }

    fn add_protocol_bookmark(&mut self, frame: &ProtocolFrameEntry) {
        let replay_position_ms = self
            .active_runtime_session()
            .replay
            .as_ref()
            .map(ReplayController::position_ms);
        let id = self.next_bookmark_id;
        self.next_bookmark_id = self.next_bookmark_id.saturating_add(1);
        let service_uuid = self
            .protocol_source
            .as_ref()
            .map(|key| key.service_uuid.clone())
            .unwrap_or_default();
        let characteristic_uuid = self
            .protocol_source
            .as_ref()
            .map(|key| key.characteristic_uuid.clone())
            .unwrap_or_default();
        self.active_runtime_session_mut()
            .meta
            .bookmarks
            .push(Bookmark {
                id,
                label: format!("Protocol frame #{}", frame.sequence),
                timestamp: frame.timestamp.clone(),
                source: "protocol".to_owned(),
                sequence: Some(frame.sequence),
                replay_position_ms,
                service_uuid,
                characteristic_uuid,
                hex: format_hex(&frame.data),
            });
    }

    fn render_workspace_bar(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.horizontal_wrapped(|ui| {
            ui.strong(language.text("Workspace"));
            ui.add(egui::TextEdit::singleline(&mut self.workspace_name).desired_width(180.0));
            if ui.button(language.text("Open")).clicked() {
                self.open_workspace();
            }
            if ui.button(language.text("Save")).clicked() {
                self.save_workspace(false);
            }
            if ui.button(language.text("Save As")).clicked() {
                self.save_workspace(true);
            }
            ui.separator();
            ui.label(crate::plugin::host_api_label());
        });

        let mut activate = None;
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            ui.strong(language.text("Sessions"));
            for (index, session) in self.sessions.iter().enumerate() {
                let label = language.format(
                    "{} {} · {} logs · {} frames",
                    &[
                        language.text(session.meta.kind.label()).to_owned(),
                        session.meta.name.to_string(),
                        session.logs.len().to_string(),
                        session.protocol_frames.len().to_string(),
                    ],
                );
                let details = language.format(
                    "created {}\\ndevice {}\\nsource {}",
                    &[
                        session.meta.created_at.to_string(),
                        session
                            .meta
                            .device_name
                            .as_deref()
                            .unwrap_or("-")
                            .to_string(),
                        session
                            .meta
                            .source_path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "-".to_owned())
                            .to_string(),
                    ],
                );
                if ui
                    .selectable_label(self.active_session == index, label)
                    .on_hover_text(details)
                    .clicked()
                {
                    activate = Some(index);
                }
                if index != self.live_session && ui.small_button("×").clicked() {
                    close = Some(index);
                    break;
                }
            }
        });
        if let Some(index) = activate
            && index != self.active_session
        {
            if let Some(replay) = self.sessions[self.active_session].replay.as_mut() {
                replay.pause();
            }
            self.active_session = index;
            self.selected_protocol_frame_sequence = None;
            self.clear_plot_samples();
        }
        if let Some(index) = close {
            self.close_session(index);
        }

        ui.horizontal_wrapped(|ui| {
            ui.strong(language.text("View"));
            ui.checkbox(
                &mut self.layout.show_devices_gatt,
                language.text("Devices/GATT"),
            );
            ui.checkbox(&mut self.layout.show_plot, language.text("Plot"));
            ui.checkbox(&mut self.layout.show_protocol, language.text("Protocol"));
            ui.checkbox(&mut self.layout.show_replay, language.text("Replay"));
            ui.checkbox(&mut self.layout.show_bookmarks, language.text("Bookmarks"));
            ui.checkbox(&mut self.layout.show_monitor, language.text("Monitor"));
        });
    }

    fn render_bookmarks(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.horizontal_wrapped(|ui| {
            ui.heading(language.text("Bookmarks & Notes"));
            let count = self.active_runtime_session().meta.bookmarks.len();
            ui.label(language.format("{count} bookmarks", &[count.to_string()]));
        });
        ui.label(language.text("Session notes"));
        ui.add(
            egui::TextEdit::multiline(&mut self.active_runtime_session_mut().meta.notes)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );

        let bookmarks = self.active_runtime_session().meta.bookmarks.clone();
        let mut remove_id = None;
        let mut seek_to = None;
        let mut select_sequence = None;
        let mut edits = Vec::new();
        egui::ScrollArea::vertical()
            .id_salt(("bookmarks", self.active_runtime_session().meta.id))
            .max_height(160.0)
            .show(ui, |ui| {
                for bookmark in &bookmarks {
                    let mut label = bookmark.label.clone();
                    ui.horizontal_wrapped(|ui| {
                        ui.monospace(format!("#{}", bookmark.id));
                        if ui
                            .add(egui::TextEdit::singleline(&mut label).desired_width(180.0))
                            .changed()
                        {
                            edits.push((bookmark.id, label.clone()));
                        }
                        ui.monospace(&bookmark.timestamp);
                        ui.label(&bookmark.source);
                        if !bookmark.characteristic_uuid.is_empty() {
                            ui.monospace(&bookmark.characteristic_uuid);
                        }
                        if !bookmark.hex.is_empty() {
                            ui.monospace(&bookmark.hex);
                        }
                        if !bookmark.service_uuid.is_empty() {
                            ui.label("ⓘ").on_hover_text(language.format(
                                "Service {}",
                                std::slice::from_ref(&bookmark.service_uuid),
                            ));
                        }
                        if let Some(sequence) = bookmark.sequence
                            && ui
                                .button(
                                    language.format("Frame #{sequence}", &[sequence.to_string()]),
                                )
                                .clicked()
                        {
                            select_sequence = Some(sequence);
                        }
                        if let Some(offset) = bookmark.replay_position_ms
                            && ui
                                .button(
                                    language
                                        .format("Go {}", &[format_duration(offset).to_string()]),
                                )
                                .clicked()
                        {
                            seek_to = Some(offset);
                        }
                        if ui.small_button(language.text("Delete")).clicked() {
                            remove_id = Some(bookmark.id);
                        }
                    });
                }
            });
        for (id, label) in edits {
            if let Some(bookmark) = self
                .active_runtime_session_mut()
                .meta
                .bookmarks
                .iter_mut()
                .find(|bookmark| bookmark.id == id)
            {
                bookmark.label = label;
            }
        }
        if let Some(sequence) = select_sequence {
            self.selected_protocol_frame_sequence = Some(sequence);
        }
        if let Some(offset) = seek_to
            && let Some(replay) = self.active_runtime_session_mut().replay.as_mut()
        {
            replay.seek(offset);
        }
        if let Some(id) = remove_id {
            self.active_runtime_session_mut()
                .meta
                .bookmarks
                .retain(|bookmark| bookmark.id != id);
        }
    }

    fn render_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(self.language.text("Language"));
            egui::ComboBox::from_id_salt("language-selector")
                .selected_text(self.language.native_name())
                .show_ui(ui, |ui| {
                    for language in Language::ALL {
                        ui.selectable_value(&mut self.language, language, language.native_name());
                    }
                });
        });
        let language = self.language;
        ui.horizontal_wrapped(|ui| {
            ui.strong("Bluetooth Monitor v0.7");
            ui.separator();
            ui.label(language.format(
                "State: {}",
                &[language.text(self.connection_phase.label()).to_string()],
            ));
            ui.separator();
            ui.label(self.status.render(language));
        });

        if let Some(error) = &self.last_error {
            ui.colored_label(ui.visuals().error_fg_color, error.render(language));
        }

        ui.horizontal_wrapped(|ui| {
            ui.label(language.text("Adapter"));

            let selected_text = self
                .adapters
                .get(self.selected_adapter)
                .map(AdapterInfo::display_name)
                .unwrap_or_else(|| language.text("初始化中…").to_owned());
            let mut requested_adapter = None;
            egui::ComboBox::from_id_salt("adapter-selector")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for adapter in &self.adapters {
                        let selected = self.selected_adapter == adapter.index;
                        if ui
                            .selectable_label(selected, adapter.display_name())
                            .clicked()
                        {
                            requested_adapter = Some(adapter.index);
                        }
                    }
                });

            if let Some(index) = requested_adapter
                && index != self.selected_adapter
            {
                self.send(BleCommand::SelectAdapter { index });
            }

            ui.label(language.format(
                "Adapter state: {}",
                &[language.text(&self.adapter_state).to_owned()],
            ));
            ui.separator();

            if ui
                .button(if self.scanning {
                    language.text("停止扫描")
                } else {
                    language.text("开始扫描")
                })
                .clicked()
            {
                if self.scanning {
                    self.send(BleCommand::StopScan);
                } else {
                    self.send(BleCommand::StartScan {
                        service_uuids: self.scan_service_tokens(),
                    });
                }
            }

            let can_connect = self.connection_phase != ConnectionPhase::Connecting
                && self.connected_id.is_none()
                && self.selected_device_id.is_some();
            if ui
                .add_enabled(can_connect, egui::Button::new(language.text("连接")))
                .clicked()
                && let Some(peripheral_id) = self.selected_device_id.clone()
            {
                let device_name = self
                    .devices
                    .iter()
                    .find(|device| device.id == peripheral_id)
                    .map(|device| device.name.clone());
                self.ensure_live_session_for_device(&peripheral_id, device_name.as_deref());
                self.send(BleCommand::Connect { peripheral_id });
            }

            if ui
                .add_enabled(
                    self.target_id.is_some(),
                    egui::Button::new(language.text("断开/取消重连")),
                )
                .clicked()
            {
                self.send(BleCommand::Disconnect);
            }

            let old_auto_reconnect = self.auto_reconnect;
            ui.checkbox(&mut self.auto_reconnect, language.text("自动重连"));
            if self.auto_reconnect != old_auto_reconnect {
                self.send(BleCommand::SetAutoReconnect {
                    enabled: self.auto_reconnect,
                });
            }

            if let Some(name) = &self.connected_name {
                let mtu = self
                    .mtu
                    .map(|value| format!(" · MTU {value}"))
                    .unwrap_or_default();
                ui.label(
                    language.format("设备: {name}{mtu}", &[name.to_string(), mtu.to_string()]),
                );
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label(language.text("Profile"));
            ui.add(
                egui::TextEdit::singleline(&mut self.profile_name)
                    .hint_text(language.text("profile name"))
                    .desired_width(160.0),
            );

            let profile_label = self
                .selected_profile_index
                .and_then(|index| self.profiles.get(index))
                .map(|stored| stored.profile.name.clone())
                .unwrap_or_else(|| language.text("选择 Profile").to_owned());
            let mut requested_profile = None;
            egui::ComboBox::from_id_salt("profile-selector")
                .selected_text(profile_label)
                .show_ui(ui, |ui| {
                    for (index, stored) in self.profiles.iter().enumerate() {
                        if ui
                            .selectable_label(
                                self.selected_profile_index == Some(index),
                                &stored.profile.name,
                            )
                            .clicked()
                        {
                            requested_profile = Some(index);
                        }
                    }
                });
            if let Some(index) = requested_profile {
                self.apply_profile(index);
            }

            if ui.button(language.text("保存 Profile")).clicked() {
                self.save_current_profile();
            }
            if ui
                .add_enabled(
                    self.selected_profile_index.is_some(),
                    egui::Button::new(language.text("重新加载 Profile")),
                )
                .clicked()
                && let Some(index) = self.selected_profile_index
            {
                self.apply_profile(index);
            }

            ui.separator();
            if ui.button(language.text("打开 .bmon 回放")).clicked() {
                self.open_replay();
            }
        });
    }

    fn render_devices(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        let visible_devices = self
            .devices
            .iter()
            .filter(|device| self.device_matches_scan_filter(device))
            .cloned()
            .collect::<Vec<_>>();

        ui.horizontal(|ui| {
            ui.heading(language.text("Devices"));
            ui.label(language.format(
                "{} / {} visible",
                &[
                    visible_devices.len().to_string(),
                    self.devices.len().to_string(),
                ],
            ));
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(language.text("Name/Address"));
            ui.add(
                egui::TextEdit::singleline(&mut self.scan_name_filter)
                    .hint_text("ESP32 / AA:BB")
                    .desired_width(140.0),
            );
            ui.label(language.text("Service UUID"));
            ui.add(
                egui::TextEdit::singleline(&mut self.scan_service_filter)
                    .hint_text(language.text("180D or full UUID"))
                    .desired_width(190.0),
            );
            ui.label(language.text("Min RSSI"));
            ui.add(egui::DragValue::new(&mut self.scan_min_rssi).range(-127..=20));
        });
        ui.small(language.text("Service UUID 会传给系统扫描过滤器，同时在设备列表中再次过滤；修改 Service UUID 后重新开始扫描生效。"));
        ui.separator();

        let mut requested_device = None;
        egui::ScrollArea::vertical()
            .id_salt("devices")
            .max_height(260.0)
            .show(ui, |ui| {
                for device in &visible_devices {
                    let selected = self.selected_device_id.as_deref() == Some(device.id.as_str());
                    let connected = self.connected_id.as_deref() == Some(device.id.as_str());
                    let rssi = device
                        .rssi
                        .map(|value| format!("{value} dBm"))
                        .unwrap_or_else(|| "RSSI -".to_owned());
                    let marker = if connected { "● " } else { "" };
                    let text = format!("{marker}{}\n{}  {}", device.name, rssi, device.address);
                    if ui.selectable_label(selected, text).clicked() {
                        requested_device = Some(device.id.clone());
                    }
                    ui.add_space(4.0);
                }
            });

        if let Some(id) = requested_device {
            self.selected_device_id = Some(id);
        }

        ui.separator();
        ui.heading(language.text("Advertisement"));
        let Some(device) = self.selected_device().cloned() else {
            ui.label(language.text("选择设备后查看广播数据"));
            return;
        };

        ui.monospace(language.format("Name:       {}\\nAddress:    {}\\nAddressType:{}\\nRSSI:       {}\\nTX Power:   {}\\nAppearance: {}", &[device.name.to_string(), device.address.to_string(), device.address_type.as_deref().unwrap_or("-").to_string(), device.rssi.map(|value| value.to_string()).unwrap_or_else(|| "-".to_owned()).to_string(), device.tx_power.map(|value| value.to_string()).unwrap_or_else(|| "-".to_owned()).to_string(), device
                .appearance
                .map(|value| format!("0x{value:04X} ({value})"))
                .unwrap_or_else(|| "-".to_owned()).to_string()]));

        egui::ScrollArea::vertical()
            .id_salt("advertisement")
            .max_height(230.0)
            .show(ui, |ui| {
                if !device.advertised_services.is_empty() {
                    egui::CollapsingHeader::new(language.format(
                        "Advertised Services ({})",
                        &[device.advertised_services.len().to_string()],
                    ))
                    .default_open(false)
                    .show(ui, |ui| {
                        for uuid in &device.advertised_services {
                            ui.monospace(uuid);
                        }
                    });
                }

                if !device.manufacturer_data.is_empty() {
                    egui::CollapsingHeader::new(language.format(
                        "Manufacturer Data ({})",
                        &[device.manufacturer_data.len().to_string()],
                    ))
                    .default_open(true)
                    .show(ui, |ui| {
                        for entry in &device.manufacturer_data {
                            ui.monospace(format!(
                                "0x{:04X}: {}",
                                entry.company_id,
                                format_hex(&entry.data)
                            ));
                        }
                    });
                }

                if !device.service_data.is_empty() {
                    egui::CollapsingHeader::new(language.format(
                        "Service Data ({})",
                        &[device.service_data.len().to_string()],
                    ))
                    .default_open(true)
                    .show(ui, |ui| {
                        for entry in &device.service_data {
                            ui.monospace(format!(
                                "{}: {}",
                                entry.service_uuid,
                                format_hex(&entry.data)
                            ));
                        }
                    });
                }

                if device.advertised_services.is_empty()
                    && device.manufacturer_data.is_empty()
                    && device.service_data.is_empty()
                {
                    ui.label(language.text("当前广播快照没有额外 Manufacturer / Service Data"));
                }
            });
    }

    fn render_gatt(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.heading("GATT");
        ui.separator();

        if self.gatt.services.is_empty() {
            ui.label(language.text("连接设备后显示 Service / Characteristic / Descriptor"));
            return;
        }

        let mut requested_characteristic = None;
        let mut requested_descriptor = None;

        egui::ScrollArea::vertical()
            .id_salt("gatt")
            .max_height(330.0)
            .show(ui, |ui| {
                for service in self.gatt.services.clone() {
                    let title = if service.primary {
                        language
                            .format("Service {}  [primary]", std::slice::from_ref(&service.uuid))
                    } else {
                        language.format("Service {}", std::slice::from_ref(&service.uuid))
                    };

                    egui::CollapsingHeader::new(title)
                        .default_open(true)
                        .show(ui, |ui| {
                            for characteristic in service.characteristics {
                                let selected =
                                    self.selected_characteristic.as_ref() == Some(&characteristic);
                                let label = format!(
                                    "{}   [{}]",
                                    characteristic.key.characteristic_uuid,
                                    characteristic.properties.labels()
                                );
                                if ui.selectable_label(selected, label).clicked() {
                                    requested_characteristic = Some(characteristic.clone());
                                }

                                if !characteristic.descriptors.is_empty() {
                                    ui.indent(
                                        format!(
                                            "descriptor-indent-{}-{}",
                                            characteristic.key.service_uuid,
                                            characteristic.key.characteristic_uuid
                                        ),
                                        |ui| {
                                            for descriptor in &characteristic.descriptors {
                                                let selected = self.selected_descriptor.as_ref()
                                                    == Some(descriptor);
                                                let label = language.format(
                                                    "Descriptor {}",
                                                    std::slice::from_ref(
                                                        &descriptor.key.descriptor_uuid,
                                                    ),
                                                );
                                                if ui.selectable_label(selected, label).clicked() {
                                                    requested_characteristic =
                                                        Some(characteristic.clone());
                                                    requested_descriptor = Some(descriptor.clone());
                                                }
                                            }
                                        },
                                    );
                                }
                            }
                        });
                }
            });

        if let Some(characteristic) = requested_characteristic {
            self.write_with_response = characteristic.properties.write;
            self.selected_characteristic = Some(characteristic);
            if requested_descriptor.is_none() {
                self.selected_descriptor = None;
            }
        }
        if let Some(descriptor) = requested_descriptor {
            self.selected_descriptor = Some(descriptor);
        }
    }

    fn render_characteristic_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.separator();
        ui.heading(language.text("Characteristic"));

        let Some(characteristic) = self.selected_characteristic.clone() else {
            ui.label(language.text("从 GATT 树选择一个 Characteristic"));
            return;
        };

        ui.monospace(language.format(
            "Service: {}\\nChar:    {}\\nProps:   {}",
            &[
                characteristic.key.service_uuid.to_string(),
                characteristic.key.characteristic_uuid.to_string(),
                characteristic.properties.labels().to_string(),
            ],
        ));

        let connected = self.connected_id.is_some();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    connected && characteristic.properties.read,
                    egui::Button::new(language.text("Read")),
                )
                .clicked()
            {
                self.send(BleCommand::Read {
                    characteristic: characteristic.key.clone(),
                });
            }

            let can_subscribe = connected
                && (characteristic.properties.notify || characteristic.properties.indicate);
            if ui
                .add_enabled(can_subscribe, egui::Button::new(language.text("Subscribe")))
                .clicked()
            {
                self.send(BleCommand::Subscribe {
                    characteristic: characteristic.key.clone(),
                });
            }

            if ui.button(language.text("设为协议源")).clicked() {
                self.protocol_source = Some(characteristic.key.clone());
                self.reset_active_protocol();
            }

            if ui.button(language.text("设为 CH1 数据源")).clicked()
                && let Some(channel) = self.plot_channels.first_mut()
            {
                channel.config.source = Some(characteristic.key.clone());
                channel.config.enabled = true;
                channel.clear();
            }
        });

        ui.horizontal(|ui| {
            ui.label(language.text("TX HEX"));
            ui.add(
                egui::TextEdit::singleline(&mut self.tx_hex)
                    .hint_text("01 03 00 00 00 02 C4 0B")
                    .desired_width(f32::INFINITY),
            );
        });

        let can_write_with_response = characteristic.properties.write;
        let can_write_without_response = characteristic.properties.write_without_response;

        if can_write_with_response && can_write_without_response {
            ui.checkbox(
                &mut self.write_with_response,
                language.text("Write With Response"),
            );
        } else if can_write_with_response {
            self.write_with_response = true;
            ui.label(language.text("Write mode: With Response"));
        } else if can_write_without_response {
            self.write_with_response = false;
            ui.label(language.text("Write mode: Without Response"));
        }

        let parsed = parse_hex(&self.tx_hex);
        if let Err(error) = &parsed
            && !self.tx_hex.trim().is_empty()
        {
            ui.colored_label(ui.visuals().error_fg_color, error.to_string());
        }

        let write_supported = can_write_with_response || can_write_without_response;
        let can_send =
            connected && write_supported && parsed.as_ref().is_ok_and(|data| !data.is_empty());
        if ui
            .add_enabled(can_send, egui::Button::new(language.text("Send")))
            .clicked()
            && let Ok(data) = parsed
        {
            let tx = self.tx_hex.clone();
            self.send(BleCommand::Write {
                characteristic: characteristic.key.clone(),
                data,
                with_response: self.write_with_response,
            });
            self.remember_tx(&tx);
        }

        ui.horizontal_wrapped(|ui| {
            let changed = ui
                .checkbox(&mut self.periodic_enabled, language.text("周期发送"))
                .changed();
            if changed {
                self.periodic_next = None;
                self.periodic_sent = 0;
            }
            ui.label(language.text("间隔 ms"));
            if ui
                .add(egui::DragValue::new(&mut self.periodic_interval_ms).range(100..=3_600_000))
                .changed()
            {
                self.periodic_interval_ms = self.periodic_interval_ms.max(100);
                self.periodic_next = None;
            }
            ui.monospace(language.format("sent {}", &[self.periodic_sent.to_string()]));

            let mut selected_history = None;
            egui::ComboBox::from_id_salt("tx-history")
                .selected_text(language.text("发送历史"))
                .show_ui(ui, |ui| {
                    for item in &self.tx_history {
                        if ui.selectable_label(false, item).clicked() {
                            selected_history = Some(item.clone());
                        }
                    }
                });
            if let Some(value) = selected_history {
                self.tx_hex = value;
            }
            if ui.button(language.text("清空历史")).clicked() {
                self.tx_history.clear();
            }
        });

        ui.separator();
        ui.heading(language.text("Descriptor"));
        let Some(descriptor) = self.selected_descriptor.clone() else {
            if characteristic.descriptors.is_empty() {
                ui.label(language.text("该 Characteristic 没有 Descriptor"));
            } else {
                ui.label(language.text("在 GATT 树中选择 Descriptor 后可 Read / Write"));
            }
            return;
        };

        ui.monospace(language.format(
            "Service: {}\\nChar:    {}\\nDesc:    {}",
            &[
                descriptor.key.service_uuid.to_string(),
                descriptor.key.characteristic_uuid.to_string(),
                descriptor.key.descriptor_uuid.to_string(),
            ],
        ));

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    connected,
                    egui::Button::new(language.text("Read Descriptor")),
                )
                .clicked()
            {
                self.send(BleCommand::ReadDescriptor {
                    descriptor: descriptor.key.clone(),
                });
            }
        });

        ui.horizontal(|ui| {
            ui.label(language.text("Descriptor HEX"));
            ui.add(
                egui::TextEdit::singleline(&mut self.descriptor_hex)
                    .hint_text("01 00")
                    .desired_width(f32::INFINITY),
            );
        });

        let parsed_descriptor = parse_hex(&self.descriptor_hex);
        if let Err(error) = &parsed_descriptor
            && !self.descriptor_hex.trim().is_empty()
        {
            ui.colored_label(ui.visuals().error_fg_color, error.to_string());
        }

        let can_write_descriptor = connected
            && parsed_descriptor
                .as_ref()
                .is_ok_and(|data| !data.is_empty());
        if ui
            .add_enabled(
                can_write_descriptor,
                egui::Button::new(language.text("Write Descriptor")),
            )
            .clicked()
            && let Ok(data) = parsed_descriptor
        {
            self.send(BleCommand::WriteDescriptor {
                descriptor: descriptor.key,
                data,
            });
        }
    }

    fn render_monitor(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        let active_is_live = self.active_session == self.live_session;
        ui.horizontal_wrapped(|ui| {
            ui.heading(language.text("Data Monitor"));
            if ui.button(language.text("清空显示")).clicked() {
                self.active_runtime_session_mut().logs.clear();
            }
            ui.checkbox(&mut self.monitor_paused, language.text("暂停 RX 显示"));
            ui.checkbox(&mut self.auto_scroll, language.text("自动滚动"));
            ui.checkbox(&mut self.show_ascii, "ASCII");

            ui.separator();
            ui.checkbox(&mut self.show_rx, "RX");
            ui.checkbox(&mut self.show_read, "RD");
            ui.checkbox(&mut self.show_tx, "TX");

            ui.separator();
            if self.capture.is_some() {
                if ui.button(language.text("停止捕获")).clicked() {
                    self.stop_capture();
                }
            } else if ui
                .add_enabled(
                    active_is_live,
                    egui::Button::new(language.text("开始捕获 CSV + BMON")),
                )
                .clicked()
            {
                self.start_capture();
            }

            if ui.button(language.text("回放 BMON")).clicked() {
                self.open_replay();
            }
        });

        let stats = self.active_runtime_session().stats;
        ui.horizontal_wrapped(|ui| {
            ui.label(language.text("过滤"));
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(language.text("UUID / HEX / ASCII / 时间"))
                    .desired_width(280.0),
            );
            ui.monospace(language.format(
                "RX {} pkts / {} B   RD {} / {} B   TX {} / {} B",
                &[
                    stats.rx_packets.to_string(),
                    stats.rx_bytes.to_string(),
                    stats.read_packets.to_string(),
                    stats.read_bytes.to_string(),
                    stats.tx_packets.to_string(),
                    stats.tx_bytes.to_string(),
                ],
            ));
            if stats.paused_rx_packets > 0 {
                ui.label(
                    language.format("暂停期间隐藏 {} RX", &[stats.paused_rx_packets.to_string()]),
                );
            }
        });

        if let Some(capture) = &self.capture {
            ui.monospace(language.format(
                "CAPTURE ● {} records · CSV {} · BMON {}",
                &[
                    capture.records().to_string(),
                    capture.paths().csv.display().to_string(),
                    capture.paths().raw.display().to_string(),
                ],
            ));
        } else if let Some(paths) = &self.last_capture_paths {
            ui.monospace(language.format(
                "Last capture: CSV {} · BMON {}",
                &[
                    paths.csv.display().to_string(),
                    paths.raw.display().to_string(),
                ],
            ));
        }

        if let Some(error) = &self.capture_error {
            ui.colored_label(ui.visuals().error_fg_color, error.render(language));
        }

        ui.separator();

        let query = self.filter.trim().to_ascii_lowercase();
        let visible_entries = self
            .active_runtime_session()
            .logs
            .iter()
            .filter(|entry| self.log_matches(entry, &query))
            .cloned()
            .collect::<Vec<_>>();
        let total_logs = self.active_runtime_session().logs.len();

        ui.label(language.format(
            "显示 {} / {} 条，内存上限 {}",
            &[
                visible_entries.len().to_string(),
                total_logs.to_string(),
                MAX_LOG_ENTRIES.to_string(),
            ],
        ));

        let mut bookmark = None;
        egui::ScrollArea::vertical()
            .id_salt(("monitor", self.active_runtime_session().meta.id))
            .stick_to_bottom(self.auto_scroll && !self.monitor_paused)
            .show_rows(ui, 20.0, visible_entries.len(), |ui, row_range| {
                for row in row_range {
                    let entry = &visible_entries[row];
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("☆")
                            .on_hover_text(language.text("Bookmark"))
                            .clicked()
                        {
                            bookmark = Some(entry.clone());
                        }
                        ui.monospace(&entry.timestamp);
                        ui.strong(entry.direction.label());
                        ui.monospace(&entry.characteristic_uuid);
                        ui.monospace(format_hex(&entry.data));
                        if self.show_ascii {
                            ui.label(format!(" | {}", format_ascii(&entry.data)));
                        }
                    });
                }
            });
        if let Some(entry) = bookmark {
            self.add_log_bookmark(&entry);
        }
    }

    fn render_plot(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.horizontal_wrapped(|ui| {
            ui.heading(language.text("Realtime Plot"));
            ui.label(language.text("Points/channel"));
            ui.add(
                egui::DragValue::new(&mut self.plot_max_points)
                    .range(100..=100_000)
                    .speed(100),
            );
            if ui.button(language.text("Clear All")).clicked() {
                for channel in &mut self.plot_channels {
                    channel.clear();
                }
            }
            if self.plot_channels.len() < 8 && ui.button(language.text("+ Channel")).clicked() {
                let index = self.plot_channels.len() + 1;
                let config = PlotChannelConfig {
                    name: format!("CH{index}"),
                    ..PlotChannelConfig::default()
                };
                self.plot_channels.push(PlotChannel::new(config));
            }
            if self.plot_channels.len() > 1 && ui.button(language.text("- Last")).clicked() {
                self.plot_channels.pop();
            }
        });

        let selected_key = self
            .selected_characteristic
            .as_ref()
            .map(|characteristic| characteristic.key.clone());

        for (index, channel) in self.plot_channels.iter_mut().enumerate() {
            ui.push_id(("plot-channel", index), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut channel.config.enabled, "");
                    ui.add(
                        egui::TextEdit::singleline(&mut channel.config.name).desired_width(70.0),
                    );
                    let source = channel
                        .config
                        .source
                        .as_ref()
                        .map(|key| key.characteristic_uuid.as_str())
                        .unwrap_or(language.text("no source"));
                    ui.monospace(source);
                    if ui
                        .add_enabled(
                            selected_key.is_some(),
                            egui::Button::new(language.text("Use selected")),
                        )
                        .clicked()
                    {
                        channel.config.source = selected_key.clone();
                        channel.config.enabled = true;
                        channel.clear();
                    }
                    ui.label(language.text("Offset"));
                    ui.add(
                        egui::DragValue::new(&mut channel.config.offset)
                            .range(0..=4096)
                            .speed(1),
                    );
                    egui::ComboBox::from_id_salt("type")
                        .selected_text(channel.config.value_type.label())
                        .show_ui(ui, |ui| {
                            for value_type in PlotValueType::ALL {
                                ui.selectable_value(
                                    &mut channel.config.value_type,
                                    value_type,
                                    value_type.label(),
                                );
                            }
                        });
                    ui.label("×");
                    ui.add(egui::DragValue::new(&mut channel.config.scale).speed(0.1));
                    ui.label("+");
                    ui.add(egui::DragValue::new(&mut channel.config.bias).speed(0.1));
                    ui.monospace(language.format("{} pts", &[channel.samples.len().to_string()]));
                });
            });
        }

        egui_plot::Plot::new("realtime-data-plot")
            .height(240.0)
            .allow_boxed_zoom(true)
            .allow_drag(true)
            .allow_scroll(true)
            .legend(egui_plot::Legend::default())
            .show(ui, |plot_ui| {
                for channel in &self.plot_channels {
                    if !channel.config.enabled || channel.samples.is_empty() {
                        continue;
                    }
                    let points: egui_plot::PlotPoints<'_> =
                        channel.samples.iter().copied().collect();
                    plot_ui.line(egui_plot::Line::new(channel.config.name.clone(), points));
                }
            });
    }

    fn render_protocol(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.horizontal_wrapped(|ui| {
            ui.heading(language.text("Protocol Analyzer"));
            if ui
                .checkbox(&mut self.protocol_config.enabled, language.text("Enable"))
                .changed()
            {
                self.reset_active_protocol();
            }
            if ui.button(language.text("Clear Frames")).clicked() {
                self.reset_active_protocol();
            }
            if let Some(characteristic) = &self.selected_characteristic
                && ui
                    .button(language.text("Use selected Characteristic"))
                    .clicked()
            {
                self.protocol_source = Some(characteristic.key.clone());
                self.reset_active_protocol();
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label(language.text("Preset"));
            let preset_label = self
                .selected_protocol_preset
                .and_then(|index| self.protocol_presets.get(index))
                .map(|stored| {
                    if stored.built_in {
                        language.format("{} (built-in)", std::slice::from_ref(&stored.preset.name))
                    } else {
                        stored.preset.name.clone()
                    }
                })
                .unwrap_or_else(|| language.text("选择协议预设").to_owned());
            let mut requested_preset = None;
            egui::ComboBox::from_id_salt("protocol-preset-selector")
                .selected_text(preset_label)
                .show_ui(ui, |ui| {
                    for (index, stored) in self.protocol_presets.iter().enumerate() {
                        let label = if stored.built_in {
                            language
                                .format("{} · built-in", std::slice::from_ref(&stored.preset.name))
                        } else {
                            stored.preset.name.clone()
                        };
                        if ui
                            .selectable_label(self.selected_protocol_preset == Some(index), label)
                            .clicked()
                        {
                            requested_preset = Some(index);
                        }
                    }
                });
            if let Some(index) = requested_preset {
                self.apply_protocol_preset(index);
            }

            ui.add(
                egui::TextEdit::singleline(&mut self.protocol_preset_name)
                    .hint_text(language.text("custom preset name"))
                    .desired_width(150.0),
            );
            if ui.button(language.text("Save Preset")).clicked() {
                self.save_protocol_preset();
            }
            ui.separator();
            if ui
                .add_enabled(
                    !self.active_runtime_session().protocol_frames.is_empty(),
                    egui::Button::new(language.text("Export CSV")),
                )
                .clicked()
            {
                self.export_protocol_csv();
            }
            if ui
                .add_enabled(
                    !self.active_runtime_session().protocol_frames.is_empty(),
                    egui::Button::new(language.text("Export JSON")),
                )
                .clicked()
            {
                self.export_protocol_json();
            }
        });

        if let Some(index) = self.selected_protocol_preset
            && let Some(stored) = self.protocol_presets.get(index)
            && !stored.preset.description.is_empty()
        {
            ui.small(&stored.preset.description);
        }

        let source = self
            .protocol_source
            .as_ref()
            .map(|key| format!("{} / {}", key.service_uuid, key.characteristic_uuid))
            .unwrap_or_else(|| language.text("未选择 Characteristic").to_owned());
        ui.monospace(
            language.format(
                "Source: {source} · buffered {} B · frames {}",
                &[
                    source.to_string(),
                    self.active_runtime_session()
                        .protocol_decoder
                        .buffered_bytes()
                        .to_string(),
                    self.active_runtime_session()
                        .protocol_frames
                        .len()
                        .to_string(),
                ],
            ),
        );

        let mut frame_kind = match &self.protocol_config.frame_mode {
            FrameMode::BlePacket => 0,
            FrameMode::FixedLength { .. } => 1,
            FrameMode::Delimiter { .. } => 2,
            FrameMode::LengthField { .. } => 3,
        };
        let previous_kind = frame_kind;
        ui.horizontal_wrapped(|ui| {
            ui.label(language.text("Framing"));
            egui::ComboBox::from_id_salt("protocol-frame-mode")
                .selected_text(language.text(self.protocol_config.frame_mode.label()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut frame_kind, 0, language.text("BLE packet"));
                    ui.selectable_value(&mut frame_kind, 1, language.text("Fixed length"));
                    ui.selectable_value(&mut frame_kind, 2, language.text("Delimiter"));
                    ui.selectable_value(&mut frame_kind, 3, language.text("Length field"));
                });

            ui.label("CRC");
            egui::ComboBox::from_id_salt("protocol-crc")
                .selected_text(language.text(self.protocol_config.crc.label()))
                .show_ui(ui, |ui| {
                    for crc in CrcMode::ALL {
                        ui.selectable_value(
                            &mut self.protocol_config.crc,
                            crc,
                            language.text(crc.label()),
                        );
                    }
                });
        });

        if frame_kind != previous_kind {
            self.protocol_config.frame_mode = match frame_kind {
                1 => FrameMode::FixedLength { length: 8 },
                2 => FrameMode::Delimiter {
                    delimiter: vec![0x0D, 0x0A],
                    include: false,
                },
                3 => FrameMode::LengthField {
                    offset: 0,
                    width: 1,
                    endian: Endian::Little,
                    adjustment: 0,
                },
                _ => FrameMode::BlePacket,
            };
            if let FrameMode::Delimiter { delimiter, .. } = &self.protocol_config.frame_mode {
                self.protocol_delimiter_hex = format_hex(delimiter);
            }
            self.reset_active_protocol();
        }

        match &mut self.protocol_config.frame_mode {
            FrameMode::BlePacket => {
                ui.label(language.text("每个 BLE Notification 直接作为一个协议帧。"));
            }
            FrameMode::FixedLength { length } => {
                ui.horizontal(|ui| {
                    ui.label(language.text("Frame length"));
                    ui.add(egui::DragValue::new(length).range(1..=65_536));
                });
            }
            FrameMode::Delimiter { delimiter, include } => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(language.text("Delimiter HEX"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.protocol_delimiter_hex)
                            .desired_width(180.0),
                    );
                    ui.checkbox(include, language.text("Include delimiter"));
                });
                match parse_hex(&self.protocol_delimiter_hex) {
                    Ok(value) if !value.is_empty() => *delimiter = value,
                    Ok(_) => {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            language.text("Delimiter 不能为空"),
                        );
                    }
                    Err(error) => {
                        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                    }
                }
            }
            FrameMode::LengthField {
                offset,
                width,
                endian,
                adjustment,
            } => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(language.text("Length offset"));
                    ui.add(egui::DragValue::new(offset).range(0..=4096));
                    ui.label(language.text("width"));
                    egui::ComboBox::from_id_salt("length-width")
                        .selected_text(width.to_string())
                        .show_ui(ui, |ui| {
                            for candidate in [1usize, 2, 4] {
                                ui.selectable_value(width, candidate, candidate.to_string());
                            }
                        });
                    egui::ComboBox::from_id_salt("length-endian")
                        .selected_text(endian.label())
                        .show_ui(ui, |ui| {
                            for candidate in Endian::ALL {
                                ui.selectable_value(endian, candidate, candidate.label());
                            }
                        });
                    ui.label(language.text("adjustment"));
                    ui.add(egui::DragValue::new(adjustment).range(-65_536..=65_536));
                });
                ui.small(
                    language.text("总帧长 = 长度字段值 + adjustment；总帧长必须覆盖长度字段本身。"),
                );
            }
        }

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.strong(language.text("Fields"));
            if ui.button(language.text("+ Field")).clicked()
                && self.protocol_config.fields.len() < 32
            {
                let index = self.protocol_config.fields.len() + 1;
                self.protocol_config.fields.push(FieldDefinition {
                    name: format!("field{index}"),
                    ..FieldDefinition::default()
                });
            }
        });

        let mut remove_field = None;
        for (index, field) in self.protocol_config.fields.iter_mut().enumerate() {
            ui.push_id(("protocol-field", index), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut field.name).desired_width(90.0));
                    ui.label(language.text("offset"));
                    ui.add(egui::DragValue::new(&mut field.offset).range(0..=65_536));
                    egui::ComboBox::from_id_salt("type")
                        .selected_text(field.value_type.label())
                        .show_ui(ui, |ui| {
                            for value_type in PlotValueType::ALL {
                                ui.selectable_value(
                                    &mut field.value_type,
                                    value_type,
                                    value_type.label(),
                                );
                            }
                        });
                    ui.label("×");
                    ui.add(egui::DragValue::new(&mut field.scale).speed(0.1));
                    ui.label("+");
                    ui.add(egui::DragValue::new(&mut field.bias).speed(0.1));
                    if ui.button("×").clicked() {
                        remove_field = Some(index);
                    }
                });
            });
        }
        if let Some(index) = remove_field {
            self.protocol_config.fields.remove(index);
        }

        ui.separator();
        let frames = self
            .active_runtime_session()
            .protocol_frames
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        ui.label(language.format(
            "Decoded frames: {} / {} retained",
            &[frames.len().to_string(), MAX_PROTOCOL_FRAMES.to_string()],
        ));

        let mut selected = self.selected_protocol_frame_sequence;
        let mut bookmark = None;
        egui::ScrollArea::vertical()
            .id_salt(("protocol-frames", self.active_runtime_session().meta.id))
            .max_height(220.0)
            .show_rows(ui, 22.0, frames.len(), |ui, row_range| {
                for row in row_range {
                    let frame = &frames[row];
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .small_button("☆")
                            .on_hover_text(language.text("Bookmark frame"))
                            .clicked()
                        {
                            bookmark = Some(frame.clone());
                        }
                        if ui
                            .selectable_label(
                                selected == Some(frame.sequence),
                                format!("#{:<6}", frame.sequence),
                            )
                            .clicked()
                        {
                            selected = Some(frame.sequence);
                        }
                        ui.monospace(&frame.timestamp);
                        ui.monospace(format!("CRC {}", frame.crc.label()));
                        ui.monospace(format_hex(&frame.data));
                    });
                }
            });
        self.selected_protocol_frame_sequence = selected;
        if let Some(frame) = bookmark {
            self.add_protocol_bookmark(&frame);
        }

        if let Some(sequence) = self.selected_protocol_frame_sequence
            && let Some(frame) = frames.iter().find(|frame| frame.sequence == sequence)
        {
            ui.separator();
            ui.strong(language.format("Fields · frame #{sequence}", &[sequence.to_string()]));
            egui::Grid::new(("protocol-field-values", sequence))
                .striped(true)
                .show(ui, |ui| {
                    ui.strong(language.text("Field"));
                    ui.strong(language.text("Value"));
                    ui.end_row();
                    for field in &frame.fields {
                        ui.monospace(&field.name);
                        match field.value {
                            Some(value) => ui.monospace(format!("{value:.9}")),
                            None => ui.monospace(language.text("N/A")),
                        };
                        ui.end_row();
                    }
                });
        }
    }

    fn render_replay(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        let session_index = self.active_session;
        let has_replay = self.sessions[session_index].replay.is_some();
        let mut close_session = false;
        ui.horizontal_wrapped(|ui| {
            ui.heading(language.text("BMON Replay"));
            if ui.button(language.text("Open .bmon")).clicked() {
                self.open_replay();
            }
            if has_replay
                && session_index != self.live_session
                && ui.button(language.text("Close Session")).clicked()
            {
                close_session = true;
            }
        });
        if close_session {
            self.close_session(session_index);
            return;
        }

        let Some(replay) = self.sessions[session_index].replay.as_mut() else {
            ui.label(
                language
                    .text("当前 Session 不是可回放的 .bmon；打开文件后会创建独立 Replay Session。"),
            );
            return;
        };

        ui.monospace(language.format(
            "{} · {} records · duration {}",
            &[
                replay.path.display().to_string(),
                replay.len().to_string(),
                format_duration(replay.duration_ms()).to_string(),
            ],
        ));

        let mut stepped = None;
        let mut bookmark_position = None;
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(if replay.is_playing() {
                    language.text("Pause")
                } else {
                    language.text("Play")
                })
                .clicked()
            {
                if replay.is_playing() {
                    replay.pause();
                } else {
                    replay.play();
                }
            }
            if ui.button(language.text("Stop")).clicked() {
                replay.stop();
            }
            if ui.button(language.text("Prev Event")).clicked() {
                replay.previous_event();
            }
            if ui.button(language.text("Next Event")).clicked() {
                replay.next_event();
            }
            if ui.button(language.text("Step")).clicked() {
                stepped = replay.step_one();
            }
            if ui.button(language.text("Bookmark Position")).clicked() {
                bookmark_position = Some(replay.position_ms());
            }

            ui.label(language.text("Speed"));
            let mut speed = replay.speed();
            egui::ComboBox::from_id_salt("replay-speed")
                .selected_text(format!("{speed}×"))
                .show_ui(ui, |ui| {
                    for candidate in [0.25f32, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0] {
                        ui.selectable_value(&mut speed, candidate, format!("{candidate}×"));
                    }
                });
            if (speed - replay.speed()).abs() > f32::EPSILON {
                replay.set_speed(speed);
            }
        });

        let mut position = replay.position_ms();
        let max = replay.duration_ms().max(1);
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Slider::new(&mut position, 0..=max)
                        .show_value(false)
                        .text(language.text("timeline")),
                )
                .changed()
            {
                replay.seek(position);
            }
            let event_label = replay
                .current_event_index()
                .map(|index| {
                    language.format(
                        "event {} / {}",
                        &[(index + 1).to_string(), replay.len().to_string()],
                    )
                })
                .unwrap_or_else(|| language.text("event 0 / 0").to_owned());
            ui.monospace(format!(
                "{} / {} · {}",
                format_duration(replay.position_ms()),
                format_duration(replay.duration_ms()),
                event_label
            ));
        });

        if let Some(record) = stepped {
            self.ingest_replay_record(session_index, record);
        }
        if let Some(offset) = bookmark_position {
            let id = self.next_bookmark_id;
            self.next_bookmark_id = self.next_bookmark_id.saturating_add(1);
            self.sessions[session_index].meta.bookmarks.push(Bookmark {
                id,
                label: format!("Replay {}", format_duration(offset)),
                timestamp: String::new(),
                source: "replay".to_owned(),
                sequence: None,
                replay_position_ms: Some(offset),
                service_uuid: String::new(),
                characteristic_uuid: String::new(),
                hex: String::new(),
            });
        }
    }

    fn log_matches(&self, entry: &LogEntry, query: &str) -> bool {
        let direction_visible = match entry.direction {
            LogDirection::Rx => self.show_rx,
            LogDirection::Read => self.show_read,
            LogDirection::Tx => self.show_tx,
        };
        if !direction_visible {
            return false;
        }
        if query.is_empty() {
            return true;
        }

        entry.timestamp.to_ascii_lowercase().contains(query)
            || entry.service_uuid.to_ascii_lowercase().contains(query)
            || entry
                .characteristic_uuid
                .to_ascii_lowercase()
                .contains(query)
            || format_hex(&entry.data).to_ascii_lowercase().contains(query)
            || format_ascii(&entry.data)
                .to_ascii_lowercase()
                .contains(query)
    }
}

fn ble_uuid_matches(service: &str, query: &str) -> bool {
    fn compact(value: &str) -> String {
        value
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X")
            .chars()
            .filter(|ch| *ch != '-')
            .flat_map(|ch| ch.to_lowercase())
            .collect()
    }

    let service = compact(service);
    let mut query = compact(query);
    if query.len() == 4 && query.chars().all(|ch| ch.is_ascii_hexdigit()) {
        query = format!("0000{query}00001000800000805f9b34fb");
    } else if query.len() == 8 && query.chars().all(|ch| ch.is_ascii_hexdigit()) {
        query = format!("{query}00001000800000805f9b34fb");
    }
    service == query
}

impl eframe::App for BluetoothMonitorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events(ui.ctx());
        self.tick_periodic_send(ui.ctx());
        self.tick_replay(ui.ctx());
        ui.ctx().request_repaint_after(Duration::from_millis(100));

        egui::CentralPanel::default().show(ui, |ui| {
            self.render_toolbar(ui);
            ui.separator();
            self.render_workspace_bar(ui);

            if self.layout.show_devices_gatt {
                ui.separator();
                ui.columns(2, |columns| {
                    columns[0].set_min_width(300.0);
                    self.render_devices(&mut columns[0]);

                    self.render_gatt(&mut columns[1]);
                    self.render_characteristic_controls(&mut columns[1]);
                });
            }

            if self.layout.show_plot {
                ui.separator();
                self.render_plot(ui);
            }
            if self.layout.show_protocol {
                ui.separator();
                self.render_protocol(ui);
            }
            if self.layout.show_replay {
                ui.separator();
                self.render_replay(ui);
            }
            if self.layout.show_bookmarks {
                ui.separator();
                self.render_bookmarks(ui);
            }
            if self.layout.show_monitor {
                ui.separator();
                self.render_monitor(ui);
            }
        });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let preferences = AppPreferences {
            language: self.language,
            auto_scroll: self.auto_scroll,
            show_ascii: self.show_ascii,
            write_with_response: self.write_with_response,
            auto_reconnect: self.auto_reconnect,
            show_rx: self.show_rx,
            show_read: self.show_read,
            show_tx: self.show_tx,
            plot_max_points: self.plot_max_points,
            plot_channels: self
                .plot_channels
                .iter()
                .map(|channel| channel.config.clone())
                .collect(),
            protocol_config: self.protocol_config.clone(),
            periodic_interval_ms: self.periodic_interval_ms,
            scan_name_filter: self.scan_name_filter.clone(),
            scan_service_filter: self.scan_service_filter.clone(),
            scan_min_rssi: self.scan_min_rssi,
            layout: self.layout.clone(),
        };
        eframe::set_value(storage, PREFERENCES_KEY, &preferences);
    }

    fn on_exit(&mut self) {
        self.stop_capture();
        let _ = self.commands.send(BleCommand::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_preferences_load_and_language_choice_is_saved() {
        let preferences: AppPreferences =
            serde_json::from_str(r#"{"auto_scroll":false,"scan_name_filter":"ESP32"}"#).unwrap();
        assert_eq!(preferences.language, Language::SimplifiedChinese);
        assert!(!preferences.auto_scroll);
        assert_eq!(preferences.scan_name_filter, "ESP32");
        let preferences = AppPreferences {
            language: Language::English,
            ..preferences
        };
        let saved = serde_json::to_string(&preferences).unwrap();
        let restored: AppPreferences = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.language, Language::English);
        assert_eq!(restored.scan_name_filter, "ESP32");
    }

    #[test]
    fn short_ble_uuid_filter_matches_bluetooth_base_uuid() {
        assert!(ble_uuid_matches(
            "0000180d-0000-1000-8000-00805f9b34fb",
            "180D"
        ));
        assert!(ble_uuid_matches(
            "12345678-0000-1000-8000-00805f9b34fb",
            "0x12345678"
        ));
        assert!(!ble_uuid_matches(
            "0000180f-0000-1000-8000-00805f9b34fb",
            "180D"
        ));
    }
}
