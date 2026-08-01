use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crc32fast::Hasher;
use rom_pipeline_core::{PipelineError, Result, StopToken};

use crate::archive::{ArchiveEntry, ArchiveLayout};

pub fn path(root: &Path, job_id: &str) -> PathBuf {
    root.join("deployments").join(format!("{job_id}.tsv"))
}

pub fn write(root: &Path, job_id: &str, layout: &ArchiveLayout) -> Result<String> {
    let mut entries = layout.entries.clone();
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let mut text = String::new();
    for entry in entries {
        use std::fmt::Write as _;
        writeln!(
            text,
            "{:08X}\t{}\t{}",
            entry.crc32,
            entry.size,
            entry.path.display()
        )
        .expect("writing to a String cannot fail");
    }
    let destination = path(root, job_id);
    let parent = destination
        .parent()
        .ok_or_else(|| PipelineError::Message("deployment manifest has no parent".to_owned()))?;
    fs::create_dir_all(parent)
        .map_err(|error| PipelineError::io(format!("create {}", parent.display()), error))?;
    let temporary = destination.with_extension("tsv.new");
    fs::write(&temporary, &text)
        .map_err(|error| PipelineError::io(format!("write {}", temporary.display()), error))?;
    fs::rename(&temporary, &destination)
        .map_err(|error| PipelineError::io(format!("publish {}", destination.display()), error))?;
    rom_pipeline_core::sha256_file(&destination)
}

pub fn read(root: &Path, job_id: &str) -> Result<Vec<ArchiveEntry>> {
    let manifest = path(root, job_id);
    let text = fs::read_to_string(&manifest)
        .map_err(|error| PipelineError::io(format!("read {}", manifest.display()), error))?;
    text.lines()
        .map(|line| {
            let mut fields = line.splitn(3, '\t');
            let crc = fields
                .next()
                .and_then(|value| u32::from_str_radix(value, 16).ok())
                .ok_or_else(|| PipelineError::Message("invalid Vita manifest CRC".to_owned()))?;
            let size = fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| PipelineError::Message("invalid Vita manifest size".to_owned()))?;
            let path =
                PathBuf::from(fields.next().ok_or_else(|| {
                    PipelineError::Message("invalid Vita manifest path".to_owned())
                })?);
            Ok(ArchiveEntry {
                path,
                size,
                crc32: crc,
            })
        })
        .collect()
}

pub fn verify_files(
    root: &Path,
    entries: &[ArchiveEntry],
    verify_crc: bool,
    stop: &StopToken,
) -> Result<()> {
    for entry in entries {
        if stop.is_requested() {
            return Err(PipelineError::Interrupted);
        }
        let file = root.join(&entry.path);
        let metadata = match fs::metadata(&file) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(PipelineError::MissingPath(file));
            }
            Err(error) => {
                return Err(PipelineError::io(format!("stat {}", file.display()), error));
            }
        };
        if !metadata.is_file() || metadata.len() != entry.size {
            return Err(PipelineError::Message(format!(
                "deployed Vita file mismatch: {}",
                file.display()
            )));
        }
        if verify_crc && crc32_file(&file, stop)? != entry.crc32 {
            return Err(PipelineError::Message(format!(
                "deployed Vita CRC mismatch: {}",
                file.display()
            )));
        }
    }
    Ok(())
}

fn crc32_file(path: &Path, stop: &StopToken) -> Result<u32> {
    let mut file = fs::File::open(path)
        .map_err(|error| PipelineError::io(format!("open {}", path.display()), error))?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        if stop.is_requested() {
            return Err(PipelineError::Interrupted);
        }
        let count = file
            .read(&mut buffer)
            .map_err(|error| PipelineError::io(format!("read {}", path.display()), error))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize())
}
