use crate::codec::{format_ascii, format_hex};
use anyhow::{Context, Result, anyhow};
use chrono::Local;
use serde::Serialize;
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

const RAW_MAGIC: &[u8; 5] = b"BMON\x01";
const MAX_STRING_BYTES: usize = 64 * 1024;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct CapturePaths {
    pub csv: PathBuf,
    pub raw: PathBuf,
}

#[derive(Debug)]
pub struct CaptureRecord<'a> {
    pub timestamp: &'a str,
    pub direction: &'a str,
    pub service_uuid: &'a str,
    pub characteristic_uuid: &'a str,
    pub data: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayRecord {
    pub timestamp: String,
    pub direction: String,
    pub service_uuid: String,
    pub characteristic_uuid: String,
    pub data: Vec<u8>,
}

#[derive(Serialize)]
struct CsvRecord<'a> {
    timestamp: &'a str,
    direction: &'a str,
    service_uuid: &'a str,
    characteristic_uuid: &'a str,
    length: usize,
    hex: String,
    ascii: String,
}

pub struct CaptureSession {
    csv: csv::Writer<BufWriter<File>>,
    raw: BufWriter<File>,
    paths: CapturePaths,
    records: u64,
}

impl CaptureSession {
    pub fn start_default() -> Result<Self> {
        let directory = std::env::current_dir()
            .context("无法读取当前工作目录")?
            .join("captures");
        Self::start_in(directory)
    }

    pub fn start_in(directory: impl AsRef<Path>) -> Result<Self> {
        let directory = directory.as_ref();
        fs::create_dir_all(directory)
            .with_context(|| format!("无法创建捕获目录 {}", directory.display()))?;

        let stem = Local::now().format("bluetooth_%Y%m%d_%H%M%S_%3f").to_string();
        let csv_path = directory.join(format!("{stem}.csv"));
        let raw_path = directory.join(format!("{stem}.bmon"));

        let csv_file = File::create(&csv_path)
            .with_context(|| format!("无法创建 {}", csv_path.display()))?;
        let raw_file = File::create(&raw_path)
            .with_context(|| format!("无法创建 {}", raw_path.display()))?;

        let mut raw = BufWriter::new(raw_file);
        raw.write_all(RAW_MAGIC).context("写入 BMON 文件头失败")?;

        Ok(Self {
            csv: csv::WriterBuilder::new()
                .has_headers(true)
                .from_writer(BufWriter::new(csv_file)),
            raw,
            paths: CapturePaths {
                csv: csv_path,
                raw: raw_path,
            },
            records: 0,
        })
    }

    pub fn paths(&self) -> &CapturePaths {
        &self.paths
    }

    pub fn records(&self) -> u64 {
        self.records
    }

    pub fn write(&mut self, record: CaptureRecord<'_>) -> Result<()> {
        self.csv
            .serialize(CsvRecord {
                timestamp: record.timestamp,
                direction: record.direction,
                service_uuid: record.service_uuid,
                characteristic_uuid: record.characteristic_uuid,
                length: record.data.len(),
                hex: format_hex(record.data),
                ascii: format_ascii(record.data),
            })
            .context("写入 CSV 捕获失败")?;

        write_raw_string(&mut self.raw, record.timestamp)?;
        self.raw
            .write_all(&[direction_code(record.direction)])
            .context("写入 BMON direction 失败")?;
        write_raw_string(&mut self.raw, record.service_uuid)?;
        write_raw_string(&mut self.raw, record.characteristic_uuid)?;
        self.raw
            .write_all(&(record.data.len() as u32).to_le_bytes())
            .context("写入 BMON payload 长度失败")?;
        self.raw
            .write_all(record.data)
            .context("写入 BMON payload 失败")?;

        self.records += 1;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        self.csv.flush().context("刷新 CSV 捕获失败")?;
        self.raw.flush().context("刷新 BMON 捕获失败")?;
        Ok(())
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

pub fn read_bmon(path: impl AsRef<Path>) -> Result<Vec<ReplayRecord>> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 5];
    reader
        .read_exact(&mut magic)
        .context("读取 BMON 文件头失败")?;
    if &magic != RAW_MAGIC {
        return Err(anyhow!("不是受支持的 BMON v1 文件"));
    }

    let mut records = Vec::new();
    loop {
        let timestamp = match read_raw_string_or_eof(&mut reader)? {
            Some(value) => value,
            None => break,
        };

        let mut direction = [0u8; 1];
        reader
            .read_exact(&mut direction)
            .context("BMON direction 字段不完整")?;
        let service_uuid = read_raw_string(&mut reader)?;
        let characteristic_uuid = read_raw_string(&mut reader)?;
        let payload_len = read_u32(&mut reader)? as usize;
        if payload_len > MAX_PAYLOAD_BYTES {
            return Err(anyhow!("BMON payload 过大: {payload_len} bytes"));
        }
        let mut data = vec![0u8; payload_len];
        reader
            .read_exact(&mut data)
            .context("BMON payload 不完整")?;

        records.push(ReplayRecord {
            timestamp,
            direction: direction_label(direction[0]).to_owned(),
            service_uuid,
            characteristic_uuid,
            data,
        });
    }

    Ok(records)
}

fn write_raw_string(writer: &mut impl Write, value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    let length = u16::try_from(bytes.len()).context("BMON 字符串字段过长")?;
    writer
        .write_all(&length.to_le_bytes())
        .context("写入 BMON 字符串长度失败")?;
    writer
        .write_all(bytes)
        .context("写入 BMON 字符串失败")?;
    Ok(())
}

fn read_raw_string_or_eof(reader: &mut impl Read) -> Result<Option<String>> {
    let mut length = [0u8; 2];
    let first = reader.read(&mut length[..1]).context("读取 BMON 字段失败")?;
    if first == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut length[1..])
        .context("BMON 字符串长度不完整")?;
    read_string_body(reader, u16::from_le_bytes(length) as usize).map(Some)
}

fn read_raw_string(reader: &mut impl Read) -> Result<String> {
    let mut length = [0u8; 2];
    reader
        .read_exact(&mut length)
        .context("BMON 字符串长度不完整")?;
    read_string_body(reader, u16::from_le_bytes(length) as usize)
}

fn read_string_body(reader: &mut impl Read, length: usize) -> Result<String> {
    if length > MAX_STRING_BYTES {
        return Err(anyhow!("BMON 字符串字段过大: {length} bytes"));
    }
    let mut bytes = vec![0u8; length];
    reader
        .read_exact(&mut bytes)
        .context("BMON 字符串字段不完整")?;
    String::from_utf8(bytes).context("BMON 字符串不是 UTF-8")
}

fn read_u32(reader: &mut impl Read) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes)
        .context("BMON u32 字段不完整")?;
    Ok(u32::from_le_bytes(bytes))
}

fn direction_code(direction: &str) -> u8 {
    match direction {
        "RX" => 1,
        "RD" => 2,
        "TX" => 3,
        _ => 0,
    }
}

fn direction_label(code: u8) -> &'static str {
    match code {
        1 => "RX",
        2 => "RD",
        3 => "TX",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn raw_direction_codes_are_stable() {
        assert_eq!(direction_code("RX"), 1);
        assert_eq!(direction_code("RD"), 2);
        assert_eq!(direction_code("TX"), 3);
        assert_eq!(direction_code("?"), 0);
        assert_eq!(direction_label(1), "RX");
        assert_eq!(direction_label(2), "RD");
        assert_eq!(direction_label(3), "TX");
    }

    #[test]
    fn bmon_round_trip() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bmon-test-{unique}"));
        let mut session = CaptureSession::start_in(&dir).unwrap();
        session
            .write(CaptureRecord {
                timestamp: "2026-09-21 10:00:00.000",
                direction: "RX",
                service_uuid: "service",
                characteristic_uuid: "char",
                data: &[0x01, 0xA5, 0xFF],
            })
            .unwrap();
        let raw = session.paths().raw.clone();
        session.flush().unwrap();
        drop(session);

        let records = read_bmon(&raw).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].direction, "RX");
        assert_eq!(records[0].data, vec![0x01, 0xA5, 0xFF]);

        let _ = fs::remove_dir_all(dir);
    }
}
