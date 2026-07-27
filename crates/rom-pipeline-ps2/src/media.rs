use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use rom_pipeline_core::{PipelineError, Result};

const LOGICAL_SECTOR: u64 = 2048;
const RAW_SECTOR: usize = 2352;
const PVD_SECTOR: u64 = 16;
const PVD_MAGIC: &[u8; 5] = b"CD001";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscFormat {
    DvdLogical,
    CdMode2Raw,
}

impl DiscFormat {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DvdLogical => "dvd-logical-2048",
            Self::CdMode2Raw => "cd-mode2-2352",
        }
    }
}

/// Identifies a PS2 logical DVD image or a proven single-track raw Mode 2 CD.
///
/// # Errors
///
/// Returns an error when the image is malformed, uses an ambiguous sector
/// layout, or contains sectors that cannot safely be represented by the
/// generated single-track cue.
pub fn inspect_disc(path: &Path) -> Result<DiscFormat> {
    let mut file = File::open(path)
        .map_err(|error| PipelineError::io(format!("open {}", path.display()), error))?;
    let size = file
        .metadata()
        .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
        .len();

    if size % RAW_SECTOR as u64 == 0 && has_magic(&mut file, PVD_SECTOR * RAW_SECTOR as u64 + 25)? {
        validate_mode2_sectors(&mut file, size, path)?;
        return Ok(DiscFormat::CdMode2Raw);
    }
    if size % LOGICAL_SECTOR == 0 && has_magic(&mut file, PVD_SECTOR * LOGICAL_SECTOR + 1)? {
        return Ok(DiscFormat::DvdLogical);
    }
    Err(PipelineError::Message(format!(
        "unsupported or ambiguous PS2 disc layout: {}",
        path.display()
    )))
}

fn has_magic(file: &mut File, offset: u64) -> Result<bool> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| PipelineError::io(format!("seek to 0x{offset:X}"), error))?;
    let mut magic = [0_u8; 5];
    file.read_exact(&mut magic)
        .map_err(|error| PipelineError::io(format!("read at 0x{offset:X}"), error))?;
    Ok(&magic == PVD_MAGIC)
}

fn validate_mode2_sectors(file: &mut File, size: u64, path: &Path) -> Result<()> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| PipelineError::io(format!("seek {}", path.display()), error))?;
    let mut sector = [0_u8; RAW_SECTOR];
    let mut index = 0_u64;
    while index * (RAW_SECTOR as u64) < size {
        file.read_exact(&mut sector)
            .map_err(|error| PipelineError::io(format!("read {}", path.display()), error))?;
        let valid_sync = sector[0] == 0
            && sector[1..11].iter().all(|byte| *byte == 0xff)
            && sector[11] == 0
            && sector[15] == 2;
        if !valid_sync {
            return Err(PipelineError::Message(format!(
                "raw CD contains a non-Mode-2 sector at index {index}; a trusted cue is required"
            )));
        }
        index += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Seek, SeekFrom, Write};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{DiscFormat, RAW_SECTOR, inspect_disc};

    #[test]
    fn identifies_logical_image() {
        let path = fixture_path("logical");
        let mut bytes = vec![0_u8; 20 * 2048];
        bytes[16 * 2048 + 1..16 * 2048 + 6].copy_from_slice(b"CD001");
        fs::write(&path, bytes).expect("write fixture");
        assert_eq!(
            inspect_disc(&path).expect("inspect"),
            DiscFormat::DvdLogical
        );
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn identifies_single_track_mode2_image() {
        let path = fixture_path("mode2");
        let mut file = fs::File::create(&path).expect("create fixture");
        for index in 0..20 {
            let mut sector = [0_u8; RAW_SECTOR];
            sector[1..11].fill(0xff);
            sector[15] = 2;
            if index == 16 {
                sector[25..30].copy_from_slice(b"CD001");
            }
            file.write_all(&sector).expect("write sector");
        }
        assert_eq!(
            inspect_disc(&path).expect("inspect"),
            DiscFormat::CdMode2Raw
        );
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn rejects_audio_sector_in_generated_single_track_layout() {
        let path = fixture_path("audio");
        let mut file = fs::File::create(&path).expect("create fixture");
        for index in 0..20 {
            let mut sector = [0_u8; RAW_SECTOR];
            sector[1..11].fill(0xff);
            sector[15] = 2;
            if index == 16 {
                sector[25..30].copy_from_slice(b"CD001");
            }
            file.write_all(&sector).expect("write sector");
        }
        file.seek(SeekFrom::Start(18 * RAW_SECTOR as u64))
            .expect("seek");
        file.write_all(&[0_u8; RAW_SECTOR]).expect("write audio");
        assert!(inspect_disc(&path).is_err());
        fs::remove_file(path).expect("remove fixture");
    }

    fn fixture_path(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rom-pipeline-ps2-media-{label}-{}-{nonce}",
            std::process::id()
        ))
    }
}
