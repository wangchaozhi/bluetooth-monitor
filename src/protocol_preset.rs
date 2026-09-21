use crate::{plotting::PlotValueType, protocol::{CrcMode, Endian, FieldDefinition, FrameMode, ProtocolConfig}};
use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProtocolPreset {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub config: ProtocolConfig,
    #[serde(default)]
    pub saved_at: String,
}

#[derive(Debug, Clone)]
pub struct StoredProtocolPreset {
    pub preset: ProtocolPreset,
    pub path: Option<PathBuf>,
    pub built_in: bool,
}

pub fn load_all() -> Vec<StoredProtocolPreset> {
    let mut result = built_in_presets()
        .into_iter()
        .map(|preset| StoredProtocolPreset {
            preset,
            path: None,
            built_in: true,
        })
        .collect::<Vec<_>>();
    if let Ok(custom) = load_custom() {
        result.extend(custom);
    }
    result
}

pub fn save_custom(mut preset: ProtocolPreset) -> Result<PathBuf> {
    let directory = default_dir()?;
    fs::create_dir_all(&directory)
        .with_context(|| format!("无法创建协议预设目录 {}", directory.display()))?;
    preset.saved_at = Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    let path = directory.join(format!("{}.json", sanitize_file_name(&preset.name)));
    let json = serde_json::to_vec_pretty(&preset).context("序列化协议预设失败")?;
    fs::write(&path, json).with_context(|| format!("无法写入 {}", path.display()))?;
    Ok(path)
}

fn load_custom() -> Result<Vec<StoredProtocolPreset>> {
    let directory = default_dir()?;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(&directory)
        .with_context(|| format!("无法读取协议预设目录 {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path).with_context(|| format!("无法读取 {}", path.display()))?;
        let preset = serde_json::from_slice::<ProtocolPreset>(&bytes)
            .with_context(|| format!("无法解析 {}", path.display()))?;
        result.push(StoredProtocolPreset { preset, path: Some(path), built_in: false });
    }
    result.sort_by(|left, right| left.preset.name.cmp(&right.preset.name));
    Ok(result)
}

fn default_dir() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("无法读取当前工作目录")?
        .join("protocol-presets"))
}

fn built_in_presets() -> Vec<ProtocolPreset> {
    vec![
        preset("Raw BLE packet", "每个 Notification 作为一帧", FrameMode::BlePacket, CrcMode::None),
        preset(
            "CRLF text lines",
            "以 0D 0A 分隔文本/ASCII 数据",
            FrameMode::Delimiter { delimiter: vec![0x0D, 0x0A], include: false },
            CrcMode::None,
        ),
        preset(
            "Fixed 20 bytes",
            "典型固定长度传感器帧",
            FrameMode::FixedLength { length: 20 },
            CrcMode::None,
        ),
        preset(
            "u8 length prefix",
            "首字节是总帧长度",
            FrameMode::LengthField { offset: 0, width: 1, endian: Endian::Little, adjustment: 0 },
            CrcMode::None,
        ),
        ProtocolPreset {
            name: "Modbus RTU in BLE packet".to_owned(),
            description: "一个 BLE Notification 对应一个 Modbus RTU 帧，尾部 CRC16/MODBUS LE".to_owned(),
            config: ProtocolConfig {
                enabled: true,
                frame_mode: FrameMode::BlePacket,
                crc: CrcMode::Crc16ModbusLeTail,
                fields: vec![
                    FieldDefinition {
                        name: "slave".to_owned(),
                        offset: 0,
                        value_type: PlotValueType::U8,
                        scale: 1.0,
                        bias: 0.0,
                    },
                    FieldDefinition {
                        name: "function".to_owned(),
                        offset: 1,
                        value_type: PlotValueType::U8,
                        scale: 1.0,
                        bias: 0.0,
                    },
                ],
            },
            saved_at: String::new(),
        },
    ]
}

fn preset(name: &str, description: &str, frame_mode: FrameMode, crc: CrcMode) -> ProtocolPreset {
    ProtocolPreset {
        name: name.to_owned(),
        description: description.to_owned(),
        config: ProtocolConfig {
            enabled: true,
            frame_mode,
            crc,
            fields: vec![FieldDefinition::default()],
        },
        saved_at: String::new(),
    }
}

fn sanitize_file_name(input: &str) -> String {
    let sanitized = input
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') { ch } else { '_' })
        .collect::<String>();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() { "preset".to_owned() } else { trimmed.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_enabled_and_named() {
        let presets = built_in_presets();
        assert!(presets.len() >= 5);
        assert!(presets.iter().all(|value| value.config.enabled));
        assert!(presets.iter().any(|value| value.name.contains("Modbus")));
    }
}
