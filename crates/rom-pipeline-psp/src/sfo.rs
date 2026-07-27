use std::collections::BTreeMap;

use rom_pipeline_core::{PipelineError, Result};

const HEADER_SIZE: usize = 20;
const INDEX_SIZE: usize = 16;

pub fn parse_strings(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    if bytes.len() < HEADER_SIZE || &bytes[..4] != b"\0PSF" {
        return Err(PipelineError::Message(
            "PARAM.SFO has an invalid header".to_owned(),
        ));
    }
    let key_table = usize_from_u32(read_u32(bytes, 8)?)?;
    let data_table = usize_from_u32(read_u32(bytes, 12)?)?;
    let count = usize_from_u32(read_u32(bytes, 16)?)?;
    let index_end = HEADER_SIZE
        .checked_add(
            count
                .checked_mul(INDEX_SIZE)
                .ok_or_else(|| PipelineError::Message("PARAM.SFO index overflow".to_owned()))?,
        )
        .ok_or_else(|| PipelineError::Message("PARAM.SFO index overflow".to_owned()))?;
    if index_end > bytes.len() || key_table > bytes.len() || data_table > bytes.len() {
        return Err(PipelineError::Message(
            "PARAM.SFO tables are outside the file".to_owned(),
        ));
    }

    let mut strings = BTreeMap::new();
    for index in 0..count {
        let offset = HEADER_SIZE + index * INDEX_SIZE;
        let key_offset = usize::from(read_u16(bytes, offset)?);
        let format = read_u16(bytes, offset + 2)?;
        let data_length = usize_from_u32(read_u32(bytes, offset + 4)?)?;
        let data_offset = usize_from_u32(read_u32(bytes, offset + 12)?)?;
        if format != 0x0204 {
            continue;
        }
        let key_start = key_table
            .checked_add(key_offset)
            .ok_or_else(|| PipelineError::Message("PARAM.SFO key overflow".to_owned()))?;
        let data_start = data_table
            .checked_add(data_offset)
            .ok_or_else(|| PipelineError::Message("PARAM.SFO data overflow".to_owned()))?;
        let data_end = data_start
            .checked_add(data_length)
            .ok_or_else(|| PipelineError::Message("PARAM.SFO value overflow".to_owned()))?;
        if key_start >= bytes.len() || data_end > bytes.len() {
            return Err(PipelineError::Message(
                "PARAM.SFO entry is outside the file".to_owned(),
            ));
        }
        let key_end = bytes[key_start..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|length| key_start + length)
            .ok_or_else(|| PipelineError::Message("PARAM.SFO key is unterminated".to_owned()))?;
        let value_bytes = &bytes[data_start..data_end];
        let value_length = value_bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(value_bytes.len());
        let key = std::str::from_utf8(&bytes[key_start..key_end])
            .map_err(|_| PipelineError::Message("PARAM.SFO key is not UTF-8".to_owned()))?;
        let value = std::str::from_utf8(&value_bytes[..value_length])
            .map_err(|_| PipelineError::Message(format!("PARAM.SFO value is not UTF-8: {key}")))?;
        strings.insert(key.to_owned(), value.to_owned());
    }
    Ok(strings)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| PipelineError::Message("PARAM.SFO is truncated".to_owned()))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| PipelineError::Message("PARAM.SFO is truncated".to_owned()))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn usize_from_u32(value: u32) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| PipelineError::Message("PARAM.SFO offset is too large".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::parse_strings;

    #[test]
    fn parses_string_entries() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\0PSF");
        bytes.extend_from_slice(&0x0000_0101_u32.to_le_bytes());
        bytes.extend_from_slice(&36_u32.to_le_bytes());
        bytes.extend_from_slice(&50_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&0x0204_u16.to_le_bytes());
        bytes.extend_from_slice(&10_u32.to_le_bytes());
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(b"DISC_ID\0");
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(b"ULUS10055\0");
        bytes.extend_from_slice(&[0; 6]);

        let values = parse_strings(&bytes).expect("parse");
        assert_eq!(values.get("DISC_ID").map(String::as_str), Some("ULUS10055"));
    }

    #[test]
    fn rejects_invalid_magic() {
        assert!(parse_strings(b"not an sfo").is_err());
    }
}
