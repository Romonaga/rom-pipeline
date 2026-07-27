use rom_pipeline_core::{PipelineError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleMetadata {
    pub title_id: String,
    pub version: u64,
}

/// Extracts the title ID and version from decrypted `meta/meta.xml`.
///
/// # Errors
///
/// Returns an error when either tag is missing or malformed.
pub fn parse_meta_xml(xml: &str) -> Result<TitleMetadata> {
    let title_id = value(xml, "title_id")
        .ok_or_else(|| PipelineError::Message("meta.xml has no title_id".to_owned()))?
        .to_ascii_uppercase();
    if title_id.len() != 16 || !title_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PipelineError::Message(format!(
            "invalid title_id in meta.xml: {title_id}"
        )));
    }
    let version_text = value(xml, "title_version")
        .ok_or_else(|| PipelineError::Message("meta.xml has no title_version".to_owned()))?;
    let version = version_text.parse::<u64>().map_err(|_| {
        PipelineError::Message(format!("invalid title_version in meta.xml: {version_text}"))
    })?;
    Ok(TitleMetadata { title_id, version })
}

/// Reads the title ID stored in a Wii U TMD.
///
/// # Errors
///
/// Returns an error when the TMD is too short or its title ID is malformed.
pub fn parse_tmd_title_id(tmd: &[u8]) -> Result<String> {
    const TITLE_ID_OFFSET: usize = 0x18c;
    const TITLE_ID_LENGTH: usize = 8;
    let bytes = tmd
        .get(TITLE_ID_OFFSET..TITLE_ID_OFFSET + TITLE_ID_LENGTH)
        .ok_or_else(|| PipelineError::Message("title.tmd is too short".to_owned()))?;
    let mut title_id = String::with_capacity(16);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(title_id, "{byte:02X}").expect("writing to a String cannot fail");
    }
    Ok(title_id)
}

fn value<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open_start = xml.find(&format!("<{tag}"))?;
    let value_start = xml[open_start..].find('>')? + open_start + 1;
    let value_end = xml[value_start..].find(&format!("</{tag}>"))? + value_start;
    Some(xml[value_start..value_end].trim())
}

#[cfg(test)]
mod tests {
    use super::{TitleMetadata, parse_meta_xml, parse_tmd_title_id};

    #[test]
    fn parses_title_metadata() {
        let xml = "<menu><title_id type=\"hexBinary\">0005000010101800</title_id><title_version>16</title_version></menu>";
        assert_eq!(
            parse_meta_xml(xml).expect("metadata"),
            TitleMetadata {
                title_id: "0005000010101800".to_owned(),
                version: 16
            }
        );
    }

    #[test]
    fn parses_tmd_title_id() {
        let mut tmd = vec![0_u8; 0x194];
        tmd[0x18c..0x194].copy_from_slice(&[0x00, 0x05, 0x00, 0x0e, 0x10, 0x10, 0x1d, 0x00]);
        assert_eq!(
            parse_tmd_title_id(&tmd).expect("title ID"),
            "0005000E10101D00"
        );
    }
}
