use std::ffi::OsStr;
use std::path::Path;

use rom_pipeline_core::{PipelineError, Result};

use crate::command;
use crate::sfo::parse_strings;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PspIdentity {
    pub disc_id: String,
    pub title: String,
}

/// Validates a PSP ISO and returns its embedded identity.
///
/// # Errors
///
/// Returns an error when 7-Zip cannot read the image, required PSP files are
/// missing, PARAM.SFO is malformed, or the two embedded disc IDs disagree.
pub fn inspect_iso(path: &Path) -> Result<PspIdentity> {
    let listing = command::output(
        "7z",
        [OsStr::new("l"), OsStr::new("-slt"), path.as_os_str()],
    )?;
    let listing = String::from_utf8(listing.stdout).map_err(|_| {
        PipelineError::Message(format!("7-Zip listing is not UTF-8: {}", path.display()))
    })?;
    for required in [
        "UMD_DATA.BIN",
        "PSP_GAME/PARAM.SFO",
        "PSP_GAME/SYSDIR/EBOOT.BIN",
    ] {
        let expected = format!("Path = {required}");
        if !listing.lines().any(|line| line == expected) {
            return Err(PipelineError::Message(format!(
                "PSP ISO is missing {required}: {}",
                path.display()
            )));
        }
    }

    let values = parse_strings(&extract(path, "PSP_GAME/PARAM.SFO")?)?;
    let disc_id = values
        .get("DISC_ID")
        .filter(|value| valid_disc_id(value))
        .ok_or_else(|| {
            PipelineError::Message(format!(
                "PARAM.SFO has no valid DISC_ID: {}",
                path.display()
            ))
        })?
        .to_owned();
    let title = values
        .get("TITLE")
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| disc_id.clone());
    let umd_id = normalize_umd_id(&extract(path, "UMD_DATA.BIN")?)?;
    if umd_id != disc_id {
        return Err(PipelineError::Message(format!(
            "PSP identity mismatch in {}: PARAM.SFO={disc_id}, UMD_DATA.BIN={umd_id}",
            path.display()
        )));
    }
    Ok(PspIdentity { disc_id, title })
}

fn extract(path: &Path, member: &str) -> Result<Vec<u8>> {
    Ok(command::output(
        "7z",
        [
            OsStr::new("x"),
            OsStr::new("-so"),
            path.as_os_str(),
            OsStr::new(member),
        ],
    )?
    .stdout)
}

fn valid_disc_id(value: &str) -> bool {
    value.len() == 9
        && value[..4].bytes().all(|byte| byte.is_ascii_uppercase())
        && value[4..].bytes().all(|byte| byte.is_ascii_digit())
}

fn normalize_umd_id(bytes: &[u8]) -> Result<String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == b'|' || *byte == 0 || *byte == b'\r' || *byte == b'\n')
        .unwrap_or(bytes.len());
    let value: String = bytes[..end]
        .iter()
        .filter(|byte| **byte != b'-')
        .map(|byte| char::from(*byte))
        .collect();
    if valid_disc_id(&value) {
        Ok(value)
    } else {
        Err(PipelineError::Message(format!(
            "UMD_DATA.BIN has an invalid disc ID: {value}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_umd_id;

    #[test]
    fn normalizes_hyphenated_umd_id() {
        assert_eq!(
            normalize_umd_id(b"ULUS-10055|E").expect("normalize"),
            "ULUS10055"
        );
    }

    #[test]
    fn rejects_invalid_umd_id() {
        assert!(normalize_umd_id(b"NOT-A-PSP-ID").is_err());
    }
}
