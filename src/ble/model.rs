use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInfo {
    pub index: usize,
    pub name: String,
    pub address: Option<String>,
    pub state: String,
}

impl AdapterInfo {
    pub fn display_name(&self) -> String {
        match &self.address {
            Some(address) => format!("{} ({address})", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManufacturerDataEntry {
    pub company_id: u16,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDataEntry {
    pub service_uuid: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub address: String,
    pub address_type: Option<String>,
    pub rssi: Option<i16>,
    pub tx_power: Option<i16>,
    pub appearance: Option<u16>,
    pub manufacturer_data: Vec<ManufacturerDataEntry>,
    pub service_data: Vec<ServiceDataEntry>,
    pub advertised_services: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CharacteristicKey {
    pub service_uuid: String,
    pub characteristic_uuid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DescriptorKey {
    pub service_uuid: String,
    pub characteristic_uuid: String,
    pub descriptor_uuid: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CharacteristicProperties {
    pub read: bool,
    pub write: bool,
    pub write_without_response: bool,
    pub notify: bool,
    pub indicate: bool,
}

impl CharacteristicProperties {
    pub fn labels(self) -> String {
        let mut labels = Vec::new();
        if self.read {
            labels.push("R");
        }
        if self.write {
            labels.push("W");
        }
        if self.write_without_response {
            labels.push("WNR");
        }
        if self.notify {
            labels.push("N");
        }
        if self.indicate {
            labels.push("I");
        }

        if labels.is_empty() {
            "-".to_owned()
        } else {
            labels.join(" ")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptorInfo {
    pub key: DescriptorKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacteristicInfo {
    pub key: CharacteristicKey,
    pub properties: CharacteristicProperties,
    pub descriptors: Vec<DescriptorInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    pub uuid: String,
    pub primary: bool,
    pub characteristics: Vec<CharacteristicInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GattSnapshot {
    pub services: Vec<ServiceInfo>,
}

#[derive(Debug, Clone)]
pub enum BleCommand {
    SelectAdapter {
        index: usize,
    },
    StartScan {
        service_uuids: Vec<String>,
    },
    StopScan,
    Connect {
        peripheral_id: String,
    },
    Disconnect,
    SetAutoReconnect {
        enabled: bool,
    },
    SubscribeAll,
    Subscribe {
        characteristic: CharacteristicKey,
    },
    Read {
        characteristic: CharacteristicKey,
    },
    Write {
        characteristic: CharacteristicKey,
        data: Vec<u8>,
        with_response: bool,
    },
    ReadDescriptor {
        descriptor: DescriptorKey,
    },
    WriteDescriptor {
        descriptor: DescriptorKey,
        data: Vec<u8>,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum BleEvent {
    Ready {
        adapters: Vec<AdapterInfo>,
        selected_adapter: usize,
    },
    AdapterSelected {
        adapter: AdapterInfo,
    },
    AdapterState {
        state: String,
    },
    DevicesCleared,
    ScanStarted,
    ScanStopped,
    DeviceUpsert {
        device: DeviceInfo,
    },
    DeviceRssi {
        peripheral_id: String,
        rssi: i16,
    },
    Connecting {
        peripheral_id: String,
        reconnect: bool,
        attempt: u32,
    },
    Connected {
        peripheral_id: String,
        name: String,
        mtu: u16,
    },
    Disconnected {
        peripheral_id: Option<String>,
        unexpected: bool,
    },
    ReconnectScheduled {
        attempt: u32,
        delay_ms: u64,
    },
    ReconnectFailed {
        attempt: u32,
        error: String,
    },
    GattDiscovered {
        snapshot: GattSnapshot,
    },
    Subscribed {
        characteristic: CharacteristicKey,
    },
    Notification {
        characteristic: CharacteristicKey,
        data: Vec<u8>,
        timestamp: String,
    },
    ReadResult {
        characteristic: CharacteristicKey,
        data: Vec<u8>,
        timestamp: String,
    },
    WriteComplete {
        characteristic: CharacteristicKey,
        data: Vec<u8>,
        timestamp: String,
    },
    DescriptorReadResult {
        descriptor: DescriptorKey,
        data: Vec<u8>,
        timestamp: String,
    },
    DescriptorWriteComplete {
        descriptor: DescriptorKey,
        data: Vec<u8>,
        timestamp: String,
    },
    Status(String),
    Error(String),
    Fatal(String),
}
