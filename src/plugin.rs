use serde::{Deserialize, Serialize};

pub const PROTOCOL_PLUGIN_API_MAJOR: u16 = 1;
pub const PROTOCOL_PLUGIN_API_MINOR: u16 = 0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtocolPluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_major: u16,
    pub api_minor: u16,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginPacket {
    pub timestamp: String,
    pub service_uuid: String,
    pub characteristic_uuid: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginField {
    pub name: String,
    pub value: Option<f64>,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginFrame {
    pub protocol: String,
    pub frame_type: Option<String>,
    pub payload: Vec<u8>,
    pub fields: Vec<PluginField>,
    pub warnings: Vec<String>,
}

/// Stable host-side abstraction for future protocol engines.
///
/// v0.7 intentionally does not load arbitrary native/WASM/Lua code yet. The
/// serializable packet/frame types define the host boundary so a later sandboxed
/// runtime can be added without coupling plugins to egui or btleplug internals.
pub trait ProtocolPlugin: Send {
    fn manifest(&self) -> &ProtocolPluginManifest;
    fn reset(&mut self);
    fn push(&mut self, packet: &PluginPacket) -> Vec<PluginFrame>;
}

pub fn host_api_label() -> String {
    format!("Protocol Plugin API {PROTOCOL_PLUGIN_API_MAJOR}.{PROTOCOL_PLUGIN_API_MINOR}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_boundary_round_trips_json() {
        let packet = PluginPacket {
            timestamp: "2026-09-21 12:00:00.000".to_owned(),
            service_uuid: "service".to_owned(),
            characteristic_uuid: "char".to_owned(),
            payload: vec![1, 2, 3],
        };
        let json = serde_json::to_string(&packet).unwrap();
        let decoded: PluginPacket = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, packet);
    }
}
