use crate::{
    ble::model::CharacteristicKey,
    plotting::PlotChannelConfig,
    protocol::ProtocolConfig,
};
use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceProfile {
    pub name: String,
    pub device_id: Option<String>,
    pub address: Option<String>,
    pub characteristic: Option<CharacteristicKey>,
    pub auto_reconnect: bool,
    pub write_with_response: bool,
    pub tx_hex: String,
    #[serde(default)]
    pub protocol_source: Option<CharacteristicKey>,
    #[serde(default)]
    pub protocol: ProtocolConfig,
    #[serde(default)]
    pub plot_channels: Vec<PlotChannelConfig>,
    #[serde(default = "default_periodic_interval_ms")]
    pub periodic_interval_ms: u64,
    #[serde(default)]
    pub tx_history: Vec<String>,
    #[serde(default)]
    pub scan_name_filter: String,
    #[serde(default)]
    pub scan_service_filter: String,
    #[serde(default = "default_scan_min_rssi")]
    pub scan_min_rssi: i16,
    pub saved_at: String,
}

fn default_periodic_interval_ms() -> u64 {
    1_000
}

fn default_scan_min_rssi() -> i16 {
    -127
}

#[derive(Debug, Clone)]
pub struct StoredProfile {
    pub path: PathBuf,
    pub profile: DeviceProfile,
}

pub fn default_profile_dir() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("无法读取当前工作目录")?
        .join("profiles"))
}

pub fn load_profiles() -> Result<Vec<StoredProfile>> {
    let directory = default_profile_dir()?;
    load_profiles_from(directory)
}

pub fn load_profiles_from(directory: impl AsRef<Path>) -> Result<Vec<StoredProfile>> {
    let directory = directory.as_ref();
    if !directory.exists() {
        return Ok(Vec::new());
    }

    let mut profiles = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("无法读取 Profile 目录 {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("无法读取 {}", path.display()))?;
        let profile = serde_json::from_slice::<DeviceProfile>(&bytes)
            .with_context(|| format!("无法解析 {}", path.display()))?;
        profiles.push(StoredProfile { path, profile });
    }

    profiles.sort_by(|left, right| left.profile.name.cmp(&right.profile.name));
    Ok(profiles)
}

pub fn save_profile(mut profile: DeviceProfile) -> Result<PathBuf> {
    let directory = default_profile_dir()?;
    fs::create_dir_all(&directory)
        .with_context(|| format!("无法创建 Profile 目录 {}", directory.display()))?;

    profile.saved_at = Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    let file_name = format!("{}.json", sanitize_file_name(&profile.name));
    let path = directory.join(file_name);
    let json = serde_json::to_vec_pretty(&profile).context("序列化 Profile 失败")?;
    fs::write(&path, json).with_context(|| format!("无法写入 {}", path.display()))?;
    Ok(path)
}

fn sanitize_file_name(input: &str) -> String {
    let sanitized = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();

    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "profile".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_profile_json_gets_v06_defaults() {
        let json = r#"{
            "name": "legacy",
            "device_id": null,
            "address": null,
            "characteristic": null,
            "auto_reconnect": true,
            "write_with_response": true,
            "tx_hex": "01 02",
            "saved_at": "2026-01-01 00:00:00"
        }"#;
        let profile: DeviceProfile = serde_json::from_str(json).unwrap();
        assert!(profile.protocol_source.is_none());
        assert!(!profile.protocol.enabled);
        assert!(profile.plot_channels.is_empty());
        assert_eq!(profile.periodic_interval_ms, 1_000);
        assert!(profile.tx_history.is_empty());
        assert!(profile.scan_name_filter.is_empty());
        assert!(profile.scan_service_filter.is_empty());
        assert_eq!(profile.scan_min_rssi, -127);
    }

    #[test]
    fn profile_filename_is_safe() {
        assert_eq!(sanitize_file_name("ESP32 Sensor #1"), "ESP32_Sensor__1");
        assert_eq!(sanitize_file_name("../x"), ".._x");
    }
}
