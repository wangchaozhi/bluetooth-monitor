use anyhow::{Context, Result};
use serde::Serialize;
use std::{collections::{BTreeMap, BTreeSet}, fs::File, io::BufWriter, path::Path};

#[derive(Debug, Clone, Serialize)]
pub struct ProtocolExportFrame {
    pub sequence: u64,
    pub timestamp: String,
    pub crc: String,
    pub hex: String,
    pub fields: BTreeMap<String, Option<f64>>,
}

pub fn export_json(path: impl AsRef<Path>, frames: &[ProtocolExportFrame]) -> Result<()> {
    let path = path.as_ref();
    let file = File::create(path).with_context(|| format!("无法创建 {}", path.display()))?;
    serde_json::to_writer_pretty(BufWriter::new(file), frames)
        .with_context(|| format!("无法写入 {}", path.display()))?;
    Ok(())
}

pub fn export_csv(path: impl AsRef<Path>, frames: &[ProtocolExportFrame]) -> Result<()> {
    let path = path.as_ref();
    let mut field_names = BTreeSet::new();
    for frame in frames {
        field_names.extend(frame.fields.keys().cloned());
    }
    let field_names = field_names.into_iter().collect::<Vec<_>>();

    let file = File::create(path).with_context(|| format!("无法创建 {}", path.display()))?;
    let mut writer = csv::Writer::from_writer(BufWriter::new(file));
    let mut header = vec![
        "sequence".to_owned(),
        "timestamp".to_owned(),
        "crc".to_owned(),
        "hex".to_owned(),
    ];
    header.extend(field_names.iter().cloned());
    writer.write_record(&header).context("写入协议 CSV 表头失败")?;

    for frame in frames {
        let mut row = vec![
            frame.sequence.to_string(),
            frame.timestamp.clone(),
            frame.crc.clone(),
            frame.hex.clone(),
        ];
        for field in &field_names {
            row.push(
                frame
                    .fields
                    .get(field)
                    .and_then(|value| *value)
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            );
        }
        writer.write_record(&row).context("写入协议 CSV 记录失败")?;
    }
    writer.flush().context("刷新协议 CSV 失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_serialization_contains_fields() {
        let frame = ProtocolExportFrame {
            sequence: 1,
            timestamp: "t".to_owned(),
            crc: "OK".to_owned(),
            hex: "01 02".to_owned(),
            fields: BTreeMap::from([("temperature".to_owned(), Some(12.5))]),
        };
        let text = serde_json::to_string(&frame).unwrap();
        assert!(text.contains("temperature"));
    }
}
