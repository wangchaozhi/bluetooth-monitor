use crate::ble::model::CharacteristicKey;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlotValueType {
    U8,
    I8,
    U16Le,
    U16Be,
    I16Le,
    I16Be,
    U32Le,
    U32Be,
    I32Le,
    I32Be,
    F32Le,
    F32Be,
}

impl Default for PlotValueType {
    fn default() -> Self {
        Self::U8
    }
}

impl PlotValueType {
    pub const ALL: [Self; 12] = [
        Self::U8,
        Self::I8,
        Self::U16Le,
        Self::U16Be,
        Self::I16Le,
        Self::I16Be,
        Self::U32Le,
        Self::U32Be,
        Self::I32Le,
        Self::I32Be,
        Self::F32Le,
        Self::F32Be,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::I8 => "i8",
            Self::U16Le => "u16 LE",
            Self::U16Be => "u16 BE",
            Self::I16Le => "i16 LE",
            Self::I16Be => "i16 BE",
            Self::U32Le => "u32 LE",
            Self::U32Be => "u32 BE",
            Self::I32Le => "i32 LE",
            Self::I32Be => "i32 BE",
            Self::F32Le => "f32 LE",
            Self::F32Be => "f32 BE",
        }
    }

    pub fn width(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16Le | Self::U16Be | Self::I16Le | Self::I16Be => 2,
            Self::U32Le
            | Self::U32Be
            | Self::I32Le
            | Self::I32Be
            | Self::F32Le
            | Self::F32Be => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlotChannelConfig {
    pub name: String,
    pub enabled: bool,
    pub source: Option<CharacteristicKey>,
    pub offset: usize,
    pub value_type: PlotValueType,
    pub scale: f64,
    pub bias: f64,
}

impl Default for PlotChannelConfig {
    fn default() -> Self {
        Self {
            name: "CH1".to_owned(),
            enabled: false,
            source: None,
            offset: 0,
            value_type: PlotValueType::U8,
            scale: 1.0,
            bias: 0.0,
        }
    }
}

pub struct PlotChannel {
    pub config: PlotChannelConfig,
    pub samples: VecDeque<[f64; 2]>,
    counter: u64,
}

impl PlotChannel {
    pub fn new(config: PlotChannelConfig) -> Self {
        Self {
            config,
            samples: VecDeque::new(),
            counter: 0,
        }
    }

    pub fn ingest(&mut self, source: &CharacteristicKey, data: &[u8], max_points: usize) {
        if !self.config.enabled || self.config.source.as_ref() != Some(source) {
            return;
        }
        let Some(value) = decode_value(data, self.config.offset, self.config.value_type) else {
            return;
        };
        let value = value * self.config.scale + self.config.bias;
        if !value.is_finite() {
            return;
        }
        self.counter = self.counter.saturating_add(1);
        self.samples.push_back([self.counter as f64, value]);
        while self.samples.len() > max_points {
            self.samples.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.counter = 0;
    }
}

pub fn default_channels() -> Vec<PlotChannelConfig> {
    (1..=4)
        .map(|index| PlotChannelConfig {
            name: format!("CH{index}"),
            ..PlotChannelConfig::default()
        })
        .collect()
}

pub fn decode_value(data: &[u8], offset: usize, value_type: PlotValueType) -> Option<f64> {
    let width = value_type.width();
    let bytes = data.get(offset..offset.checked_add(width)?)?;

    let value = match value_type {
        PlotValueType::U8 => bytes[0] as f64,
        PlotValueType::I8 => (bytes[0] as i8) as f64,
        PlotValueType::U16Le => u16::from_le_bytes(bytes.try_into().ok()?) as f64,
        PlotValueType::U16Be => u16::from_be_bytes(bytes.try_into().ok()?) as f64,
        PlotValueType::I16Le => i16::from_le_bytes(bytes.try_into().ok()?) as f64,
        PlotValueType::I16Be => i16::from_be_bytes(bytes.try_into().ok()?) as f64,
        PlotValueType::U32Le => u32::from_le_bytes(bytes.try_into().ok()?) as f64,
        PlotValueType::U32Be => u32::from_be_bytes(bytes.try_into().ok()?) as f64,
        PlotValueType::I32Le => i32::from_le_bytes(bytes.try_into().ok()?) as f64,
        PlotValueType::I32Be => i32::from_be_bytes(bytes.try_into().ok()?) as f64,
        PlotValueType::F32Le => f32::from_le_bytes(bytes.try_into().ok()?) as f64,
        PlotValueType::F32Be => f32::from_be_bytes(bytes.try_into().ok()?) as f64,
    };

    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_integer_endianness() {
        assert_eq!(decode_value(&[0x34, 0x12], 0, PlotValueType::U16Le), Some(0x1234 as f64));
        assert_eq!(decode_value(&[0x12, 0x34], 0, PlotValueType::U16Be), Some(0x1234 as f64));
    }

    #[test]
    fn rejects_short_payload() {
        assert_eq!(decode_value(&[1], 0, PlotValueType::U32Le), None);
    }
}
