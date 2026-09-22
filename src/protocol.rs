use crate::plotting::PlotValueType;
use crc::{CRC_8_SMBUS, CRC_16_IBM_SDLC, CRC_16_MODBUS, CRC_16_XMODEM, Crc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const MAX_BUFFER_BYTES: usize = 1024 * 1024;
const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Endian {
    #[default]
    Little,
    Big,
}

impl Endian {
    pub const ALL: [Self; 2] = [Self::Little, Self::Big];

    pub fn label(self) -> &'static str {
        match self {
            Self::Little => "LE",
            Self::Big => "BE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FrameMode {
    #[default]
    BlePacket,
    FixedLength {
        length: usize,
    },
    Delimiter {
        delimiter: Vec<u8>,
        include: bool,
    },
    LengthField {
        offset: usize,
        width: usize,
        endian: Endian,
        adjustment: i32,
    },
}

impl FrameMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::BlePacket => "BLE packet",
            Self::FixedLength { .. } => "Fixed length",
            Self::Delimiter { .. } => "Delimiter",
            Self::LengthField { .. } => "Length field",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CrcMode {
    #[default]
    None,
    Crc8SmbusTail,
    Crc16ModbusLeTail,
    Crc16ModbusBeTail,
    Crc16XmodemBeTail,
    Crc16IbmSdlcLeTail,
}

impl CrcMode {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Crc8SmbusTail,
        Self::Crc16ModbusLeTail,
        Self::Crc16ModbusBeTail,
        Self::Crc16XmodemBeTail,
        Self::Crc16IbmSdlcLeTail,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Crc8SmbusTail => "CRC-8/SMBUS tail",
            Self::Crc16ModbusLeTail => "CRC-16/MODBUS LE tail",
            Self::Crc16ModbusBeTail => "CRC-16/MODBUS BE tail",
            Self::Crc16XmodemBeTail => "CRC-16/XMODEM BE tail",
            Self::Crc16IbmSdlcLeTail => "CRC-16/IBM-SDLC LE tail",
        }
    }

    pub fn width(self) -> usize {
        match self {
            Self::None => 0,
            Self::Crc8SmbusTail => 1,
            _ => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrcStatus {
    Disabled,
    Valid,
    Invalid,
    TooShort,
}

impl CrcStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Disabled => "-",
            Self::Valid => "OK",
            Self::Invalid => "BAD",
            Self::TooShort => "SHORT",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FieldDefinition {
    pub name: String,
    pub offset: usize,
    pub value_type: PlotValueType,
    pub scale: f64,
    pub bias: f64,
}

impl Default for FieldDefinition {
    fn default() -> Self {
        Self {
            name: "value".to_owned(),
            offset: 0,
            value_type: PlotValueType::U8,
            scale: 1.0,
            bias: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolConfig {
    pub enabled: bool,
    pub frame_mode: FrameMode,
    pub crc: CrcMode,
    pub fields: Vec<FieldDefinition>,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            frame_mode: FrameMode::BlePacket,
            crc: CrcMode::None,
            fields: vec![FieldDefinition::default()],
        }
    }
}

#[derive(Debug, Clone)]
pub struct DecodedField {
    pub name: String,
    pub value: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub data: Vec<u8>,
    pub crc: CrcStatus,
    pub fields: Vec<DecodedField>,
}

pub trait ProtocolDecoder {
    fn configure(&mut self, config: &ProtocolConfig);
    fn reset(&mut self);
    fn push(&mut self, data: &[u8]) -> Vec<DecodedFrame>;
    fn buffered_bytes(&self) -> usize;
}

#[derive(Default)]
pub struct StreamProtocolDecoder {
    config: ProtocolConfig,
    buffer: VecDeque<u8>,
}

impl ProtocolDecoder for StreamProtocolDecoder {
    fn configure(&mut self, config: &ProtocolConfig) {
        if &self.config != config {
            self.config = config.clone();
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.buffer.clear();
    }

    fn push(&mut self, data: &[u8]) -> Vec<DecodedFrame> {
        if !self.config.enabled || data.is_empty() {
            return Vec::new();
        }

        if matches!(&self.config.frame_mode, FrameMode::BlePacket) {
            return vec![decode_frame(data.to_vec(), &self.config)];
        }

        let frame_mode = self.config.frame_mode.clone();
        self.buffer.extend(data.iter().copied());
        if self.buffer.len() > MAX_BUFFER_BYTES {
            let discard = self.buffer.len() - MAX_BUFFER_BYTES;
            self.buffer.drain(..discard);
        }

        let mut frames = Vec::new();
        loop {
            if self.buffer.is_empty() {
                break;
            }
            let next = match &frame_mode {
                FrameMode::BlePacket => unreachable!(),
                FrameMode::FixedLength { length } => self.take_fixed(*length),
                FrameMode::Delimiter { delimiter, include } => {
                    self.take_delimited(delimiter, *include)
                }
                FrameMode::LengthField {
                    offset,
                    width,
                    endian,
                    adjustment,
                } => self.take_length_field(*offset, *width, *endian, *adjustment),
            };

            match next {
                FrameTake::Frame(frame) => frames.push(decode_frame(frame, &self.config)),
                FrameTake::NeedMore => break,
                FrameTake::DiscardOne => {
                    self.buffer.pop_front();
                }
            }
        }
        frames
    }

    fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }
}

enum FrameTake {
    Frame(Vec<u8>),
    NeedMore,
    DiscardOne,
}

impl StreamProtocolDecoder {
    fn take_fixed(&mut self, length: usize) -> FrameTake {
        if length == 0 || length > MAX_FRAME_BYTES {
            return FrameTake::DiscardOne;
        }
        if self.buffer.len() < length {
            return FrameTake::NeedMore;
        }
        FrameTake::Frame(self.buffer.drain(..length).collect())
    }

    fn take_delimited(&mut self, delimiter: &[u8], include: bool) -> FrameTake {
        if delimiter.is_empty() || delimiter.len() > 256 {
            return FrameTake::DiscardOne;
        }
        let haystack = self.buffer.make_contiguous();
        let Some(index) = find_subslice(haystack, delimiter) else {
            return if self.buffer.len() > MAX_FRAME_BYTES {
                FrameTake::DiscardOne
            } else {
                FrameTake::NeedMore
            };
        };
        let consume = index + delimiter.len();
        let output_len = if include { consume } else { index };
        if consume > MAX_FRAME_BYTES {
            return FrameTake::DiscardOne;
        }
        let mut consumed: Vec<u8> = self.buffer.drain(..consume).collect();
        consumed.truncate(output_len);
        FrameTake::Frame(consumed)
    }

    fn take_length_field(
        &mut self,
        offset: usize,
        width: usize,
        endian: Endian,
        adjustment: i32,
    ) -> FrameTake {
        if !matches!(width, 1 | 2 | 4) || offset > 4096 {
            return FrameTake::DiscardOne;
        }
        let header_end = match offset.checked_add(width) {
            Some(value) => value,
            None => return FrameTake::DiscardOne,
        };
        if self.buffer.len() < header_end {
            return FrameTake::NeedMore;
        }
        let contiguous = self.buffer.make_contiguous();
        let raw_len = read_unsigned(&contiguous[offset..header_end], endian);
        let total = raw_len as i64 + adjustment as i64;
        if total < header_end as i64 || total <= 0 {
            return FrameTake::DiscardOne;
        }
        let Ok(total) = usize::try_from(total) else {
            return FrameTake::DiscardOne;
        };
        if total > MAX_FRAME_BYTES {
            return FrameTake::DiscardOne;
        }
        if self.buffer.len() < total {
            return FrameTake::NeedMore;
        }
        FrameTake::Frame(self.buffer.drain(..total).collect())
    }
}

fn decode_frame(data: Vec<u8>, config: &ProtocolConfig) -> DecodedFrame {
    let fields = config
        .fields
        .iter()
        .map(|field| DecodedField {
            name: field.name.clone(),
            value: crate::plotting::decode_value(&data, field.offset, field.value_type)
                .map(|value| value * field.scale + field.bias),
        })
        .collect();

    DecodedFrame {
        crc: validate_crc(&data, config.crc),
        data,
        fields,
    }
}

pub fn validate_crc(frame: &[u8], mode: CrcMode) -> CrcStatus {
    let width = mode.width();
    if width == 0 {
        return CrcStatus::Disabled;
    }
    if frame.len() <= width {
        return CrcStatus::TooShort;
    }
    let body_len = frame.len() - width;
    let body = &frame[..body_len];
    let tail = &frame[body_len..];

    let valid = match mode {
        CrcMode::None => true,
        CrcMode::Crc8SmbusTail => {
            let crc = Crc::<u8>::new(&CRC_8_SMBUS).checksum(body);
            tail[0] == crc
        }
        CrcMode::Crc16ModbusLeTail => {
            let crc = Crc::<u16>::new(&CRC_16_MODBUS).checksum(body);
            tail == crc.to_le_bytes().as_slice()
        }
        CrcMode::Crc16ModbusBeTail => {
            let crc = Crc::<u16>::new(&CRC_16_MODBUS).checksum(body);
            tail == crc.to_be_bytes().as_slice()
        }
        CrcMode::Crc16XmodemBeTail => {
            let crc = Crc::<u16>::new(&CRC_16_XMODEM).checksum(body);
            tail == crc.to_be_bytes().as_slice()
        }
        CrcMode::Crc16IbmSdlcLeTail => {
            let crc = Crc::<u16>::new(&CRC_16_IBM_SDLC).checksum(body);
            tail == crc.to_le_bytes().as_slice()
        }
    };

    if valid {
        CrcStatus::Valid
    } else {
        CrcStatus::Invalid
    }
}

fn read_unsigned(bytes: &[u8], endian: Endian) -> u64 {
    match (bytes.len(), endian) {
        (1, _) => bytes[0] as u64,
        (2, Endian::Little) => u16::from_le_bytes([bytes[0], bytes[1]]) as u64,
        (2, Endian::Big) => u16::from_be_bytes([bytes[0], bytes[1]]) as u64,
        (4, Endian::Little) => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64,
        (4, Endian::Big) => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64,
        _ => 0,
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_length_reassembles_stream() {
        let mut decoder = StreamProtocolDecoder::default();
        decoder.configure(&ProtocolConfig {
            enabled: true,
            frame_mode: FrameMode::FixedLength { length: 4 },
            ..ProtocolConfig::default()
        });
        assert!(decoder.push(&[1, 2]).is_empty());
        let frames = decoder.push(&[3, 4, 5, 6, 7, 8]);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, vec![1, 2, 3, 4]);
        assert_eq!(frames[1].data, vec![5, 6, 7, 8]);
    }

    #[test]
    fn delimiter_reassembles_stream() {
        let mut decoder = StreamProtocolDecoder::default();
        decoder.configure(&ProtocolConfig {
            enabled: true,
            frame_mode: FrameMode::Delimiter {
                delimiter: vec![0x0D, 0x0A],
                include: false,
            },
            ..ProtocolConfig::default()
        });
        let frames = decoder.push(b"ABC\r\nDEF\r\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, b"ABC".to_vec());
        assert_eq!(frames[1].data, b"DEF".to_vec());
    }

    #[test]
    fn length_field_reassembles_stream() {
        let mut decoder = StreamProtocolDecoder::default();
        decoder.configure(&ProtocolConfig {
            enabled: true,
            frame_mode: FrameMode::LengthField {
                offset: 0,
                width: 1,
                endian: Endian::Little,
                adjustment: 0,
            },
            ..ProtocolConfig::default()
        });
        let frames = decoder.push(&[3, 0xAA, 0xBB, 4, 1, 2, 3]);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, vec![3, 0xAA, 0xBB]);
        assert_eq!(frames[1].data, vec![4, 1, 2, 3]);
    }

    #[test]
    fn delimiter_can_span_notifications() {
        let mut decoder = StreamProtocolDecoder::default();
        decoder.configure(&ProtocolConfig {
            enabled: true,
            frame_mode: FrameMode::Delimiter {
                delimiter: vec![0x0D, 0x0A],
                include: false,
            },
            ..ProtocolConfig::default()
        });
        assert!(decoder.push(b"ABC\r").is_empty());
        let frames = decoder.push(b"\nDEF\r\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, b"ABC".to_vec());
        assert_eq!(frames[1].data, b"DEF".to_vec());
    }

    #[test]
    fn decoded_fields_apply_scale_and_bias() {
        let config = ProtocolConfig {
            enabled: true,
            fields: vec![FieldDefinition {
                name: "temperature".to_owned(),
                offset: 0,
                value_type: PlotValueType::U16Le,
                scale: 0.1,
                bias: -40.0,
            }],
            ..ProtocolConfig::default()
        };
        let frame = decode_frame(vec![0xF4, 0x01], &config);
        assert_eq!(frame.fields[0].name, "temperature");
        let value = frame.fields[0].value.unwrap();
        assert!((value - 10.0).abs() < 1e-9);
    }

    #[test]
    fn modbus_crc_is_validated() {
        let mut frame = b"123456789".to_vec();
        let crc = Crc::<u16>::new(&CRC_16_MODBUS).checksum(&frame);
        frame.extend_from_slice(&crc.to_le_bytes());
        assert_eq!(
            validate_crc(&frame, CrcMode::Crc16ModbusLeTail),
            CrcStatus::Valid
        );
    }
}
