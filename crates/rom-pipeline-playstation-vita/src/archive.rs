use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use rom_pipeline_core::{PipelineError, Result};

use crate::command;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveEntry {
    pub path: PathBuf,
    pub size: u64,
    pub crc32: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveLayout {
    pub title_id: String,
    pub entries: Vec<ArchiveEntry>,
    pub has_patch: bool,
    pub unpacked_bytes: u64,
}

pub fn inspect(
    seven_zip: &Path,
    archive: &Path,
    expected_title_id: &str,
    log: &Path,
) -> Result<ArchiveLayout> {
    let listing = command::capture(
        seven_zip,
        &["l".into(), "-slt".into(), archive.as_os_str().to_owned()],
        log,
    )?;
    parse_listing(&listing, expected_title_id)
}

pub fn test(seven_zip: &Path, archive: &Path, log: &Path) -> Result<()> {
    command::run_logged(
        seven_zip,
        &["t".into(), archive.as_os_str().to_owned()],
        log,
    )
}

fn parse_listing(listing: &str, expected_title_id: &str) -> Result<ArchiveLayout> {
    let entries_text = listing
        .split_once("----------\n")
        .map_or(listing, |(_, value)| value);
    let mut files = Vec::new();
    for block in entries_text.split("\n\n") {
        let mut path = None;
        let mut size = None;
        let mut crc = None;
        let mut folder = false;
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("Path = ") {
                path = Some(PathBuf::from(value));
            } else if let Some(value) = line.strip_prefix("Size = ") {
                size = value.parse::<u64>().ok();
            } else if let Some(value) = line.strip_prefix("CRC = ") {
                if !value.is_empty() {
                    crc = u32::from_str_radix(value, 16).ok();
                }
            } else if line == "Folder = +" {
                folder = true;
            }
        }
        if folder || path.is_none() {
            continue;
        }
        let path = path.unwrap_or_default();
        validate_relative_path(&path)?;
        files.push(ArchiveEntry {
            path,
            size: size.ok_or_else(|| {
                PipelineError::Message("Vita ZIP entry is missing its size".to_owned())
            })?,
            crc32: crc.ok_or_else(|| {
                PipelineError::Message("Vita ZIP entry is missing its CRC".to_owned())
            })?,
        });
    }
    validate_layout(files, expected_title_id)
}

fn validate_layout(files: Vec<ArchiveEntry>, expected_title_id: &str) -> Result<ArchiveLayout> {
    if files.is_empty() {
        return Err(PipelineError::Message(
            "Vita ZIP contains no files".to_owned(),
        ));
    }
    let mut roots = BTreeSet::new();
    for entry in &files {
        let mut components = entry.path.components();
        let root = components
            .next()
            .and_then(|value| value.as_os_str().to_str())
            .ok_or_else(|| PipelineError::Message("invalid Vita ZIP path".to_owned()))?;
        let title_id = components
            .next()
            .and_then(|value| value.as_os_str().to_str())
            .ok_or_else(|| {
                PipelineError::Message(format!(
                    "Vita ZIP entry is outside a title folder: {}",
                    entry.path.display()
                ))
            })?;
        if !matches!(root, "app" | "patch") || title_id != expected_title_id {
            return Err(PipelineError::Message(format!(
                "unexpected Vita ZIP entry: {}",
                entry.path.display()
            )));
        }
        roots.insert(root.to_owned());
    }
    if !roots.contains("app") {
        return Err(PipelineError::Message(
            "Vita ZIP does not contain an app title".to_owned(),
        ));
    }
    for required in [
        format!("app/{expected_title_id}/eboot.bin"),
        format!("app/{expected_title_id}/sce_sys/param.sfo"),
        format!("app/{expected_title_id}/sce_sys/package/work.bin"),
    ] {
        if !files.iter().any(|entry| entry.path == Path::new(&required)) {
            return Err(PipelineError::Message(format!(
                "Vita ZIP is missing {required}"
            )));
        }
    }
    let unpacked_bytes = files.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(entry.size)
            .ok_or_else(|| PipelineError::Message("Vita ZIP unpacked size overflowed".to_owned()))
    })?;
    Ok(ArchiveLayout {
        title_id: expected_title_id.to_owned(),
        has_patch: roots.contains("patch"),
        entries: files,
        unpacked_bytes,
    })
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().as_encoded_bytes().contains(&b'\\')
        || path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|byte| matches!(byte, b'\t' | b'\n' | b'\r'))
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PipelineError::Message(format!(
            "unsafe Vita ZIP entry: {}",
            path.display()
        )));
    }
    Ok(())
}

pub fn extraction_args(
    layout: &ArchiveLayout,
    archive: &Path,
    destination: &Path,
) -> Vec<OsString> {
    let mut args = vec![
        "x".into(),
        "-y".into(),
        format!("-o{}", destination.display()).into(),
        archive.as_os_str().to_owned(),
        format!("app/{}/*", layout.title_id).into(),
    ];
    if layout.has_patch {
        args.push(format!("patch/{}/*", layout.title_id).into());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::parse_listing;

    const VALID: &str = r"header
----------
Path = app/PCSE00001/eboot.bin
Folder = -
Size = 3
CRC = 352441C2

Path = app/PCSE00001/sce_sys/param.sfo
Folder = -
Size = 2
CRC = 79DCDD47

Path = app/PCSE00001/sce_sys/package/work.bin
Folder = -
Size = 1
CRC = D3D99E8B
";

    #[test]
    fn validates_native_nonpdrm_layout() {
        let layout = parse_listing(VALID, "PCSE00001").expect("valid layout");
        assert_eq!(layout.entries.len(), 3);
        assert_eq!(layout.unpacked_bytes, 6);
        assert!(!layout.has_patch);
    }

    #[test]
    fn rejects_path_traversal() {
        let invalid = VALID.replace("app/PCSE00001/eboot.bin", "app/PCSE00001/../../escape.bin");
        assert!(parse_listing(&invalid, "PCSE00001").is_err());
    }
}
