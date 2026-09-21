use thiserror::Error;

#[derive(Debug, Error)]
pub enum HexError {
    #[error("HEX 数据长度必须是偶数")]
    OddLength,
    #[error("无效的 HEX 字节: {0}")]
    InvalidByte(String),
}

pub fn parse_hex(input: &str) -> Result<Vec<u8>, HexError> {
    let mut compact = input
        .replace("0x", "")
        .replace("0X", "")
        .chars()
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, ',' | ':' | '-' | '_'))
        .collect::<String>();

    compact.make_ascii_uppercase();

    if compact.len() % 2 != 0 {
        return Err(HexError::OddLength);
    }

    let mut output = Vec::with_capacity(compact.len() / 2);
    let bytes = compact.as_bytes();

    for pair in bytes.chunks_exact(2) {
        let token = std::str::from_utf8(pair).expect("HEX 输入应为 ASCII");
        let value = u8::from_str_radix(token, 16)
            .map_err(|_| HexError::InvalidByte(token.to_owned()))?;
        output.push(value);
    }

    Ok(output)
}

pub fn format_hex(data: &[u8]) -> String {
    data.iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn format_ascii(data: &[u8]) -> String {
    data.iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spaced_hex() {
        assert_eq!(parse_hex("01 02 AA ff").unwrap(), vec![0x01, 0x02, 0xAA, 0xFF]);
    }

    #[test]
    fn parses_compact_hex() {
        assert_eq!(parse_hex("0102Aaff").unwrap(), vec![0x01, 0x02, 0xAA, 0xFF]);
    }

    #[test]
    fn parses_prefixed_hex() {
        assert_eq!(parse_hex("0x01,0x02,0xA0").unwrap(), vec![0x01, 0x02, 0xA0]);
    }
}
