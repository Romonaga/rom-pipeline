use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use rom_pipeline_core::{PipelineError, Result};
use sha2::{Digest, Sha256};

const MEDIA_UNIT: u64 = 0x200;
const NCSD_MAGIC_OFFSET: u64 = 0x100;
const PARTITION_TABLE_OFFSET: u64 = 0x120;
const NCCH_MAGIC_OFFSET: u64 = 0x100;
const TITLE_ID_OFFSET: u64 = 0x118;
const EXT_HEADER_HASH_OFFSET: u64 = 0x160;
const EXT_HEADER_SIZE_OFFSET: u64 = 0x180;
const FLAGS_OFFSET: u64 = 0x188;
const EXT_HEADER_OFFSET: u64 = 0x200;
const EXEFS_OFFSET_FIELD: u64 = 0x1a0;
const EXEFS_HASH_SIZE_FIELD: u64 = 0x1a8;
const ROMFS_OFFSET_FIELD: u64 = 0x1b0;
const ROMFS_HASH_SIZE_FIELD: u64 = 0x1b8;
const EXEFS_HASH_OFFSET: u64 = 0x1c0;
const ROMFS_HASH_OFFSET: u64 = 0x1e0;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CciIdentity {
    pub title_id: String,
    pub partition_offset: u64,
    pub partition_size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CciInspection {
    pub title_id: String,
    pub partition_offset: u64,
    pub flags: [u8; 8],
    pub verified_regions: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CiaInspection {
    pub title_id: String,
    pub content_count: usize,
    pub content_bytes: u64,
}

impl CciInspection {
    #[must_use]
    pub const fn is_marked_decrypted(&self) -> bool {
        self.flags[7] & 0x04 != 0
    }

    #[must_use]
    pub const fn normalized_flags(&self) -> [u8; 8] {
        let mut flags = self.flags;
        flags[3] = 0;
        flags[7] = (flags[7] & !0x21) | 0x04;
        flags
    }

    #[must_use]
    pub const fn flag_offsets(&self) -> [u64; 2] {
        [
            self.partition_offset + FLAGS_OFFSET + 3,
            self.partition_offset + FLAGS_OFFSET + 7,
        ]
    }
}

/// Reads the stable identity of the main NCCH without requiring decryption.
///
/// # Errors
///
/// Returns an error when the NCSD/NCCH structure is malformed or out of range.
pub fn identify_cci(path: &Path) -> Result<CciIdentity> {
    let mut file = File::open(path)
        .map_err(|error| PipelineError::io(format!("open {}", path.display()), error))?;
    let file_size = file
        .metadata()
        .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
        .len();
    identify(&mut file, file_size)
}

/// Inspects a CCI/3DS image and proves that its main NCCH payload is decrypted.
///
/// # Errors
///
/// Returns an error for malformed containers, out-of-range sections, or a
/// decrypted-section hash mismatch.
pub fn inspect_cci(path: &Path) -> Result<CciInspection> {
    let mut file = File::open(path)
        .map_err(|error| PipelineError::io(format!("open {}", path.display()), error))?;
    let file_size = file
        .metadata()
        .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
        .len();
    let identity = identify(&mut file, file_size)?;
    let partition_offset = identity.partition_offset;
    let partition_size = identity.partition_size;
    let title_id = identity.title_id;
    let flags = read_array::<8>(&mut file, partition_offset + FLAGS_OFFSET)?;

    let mut verified_regions = 0;
    let ext_header_size = u64::from(read_u32(
        &mut file,
        partition_offset + EXT_HEADER_SIZE_OFFSET,
    )?);
    if ext_header_size > 0 {
        verify_region(
            &mut file,
            partition_offset + EXT_HEADER_HASH_OFFSET,
            partition_offset + EXT_HEADER_OFFSET,
            ext_header_size,
            partition_offset + partition_size,
            "extended header",
        )?;
        verified_regions += 1;
    }

    let exefs_hash_units = u64::from(read_u32(
        &mut file,
        partition_offset + EXEFS_HASH_SIZE_FIELD,
    )?);
    if exefs_hash_units == 0 {
        return Err(PipelineError::Message(
            "main NCCH has no verifiable ExeFS hash region".to_owned(),
        ));
    }
    let exefs_offset_units = u64::from(read_u32(&mut file, partition_offset + EXEFS_OFFSET_FIELD)?);
    verify_region(
        &mut file,
        partition_offset + EXEFS_HASH_OFFSET,
        partition_offset + exefs_offset_units * MEDIA_UNIT,
        exefs_hash_units * MEDIA_UNIT,
        partition_offset + partition_size,
        "ExeFS",
    )?;
    verified_regions += 1;

    let romfs_hash_units = u64::from(read_u32(
        &mut file,
        partition_offset + ROMFS_HASH_SIZE_FIELD,
    )?);
    if romfs_hash_units > 0 {
        let romfs_offset_units =
            u64::from(read_u32(&mut file, partition_offset + ROMFS_OFFSET_FIELD)?);
        verify_region(
            &mut file,
            partition_offset + ROMFS_HASH_OFFSET,
            partition_offset + romfs_offset_units * MEDIA_UNIT,
            romfs_hash_units * MEDIA_UNIT,
            partition_offset + partition_size,
            "RomFS",
        )?;
        verified_regions += 1;
    }

    Ok(CciInspection {
        title_id,
        partition_offset,
        flags,
        verified_regions,
    })
}

/// Validates a CIA container, all TMD-recorded content hashes, and the main
/// NCCH payload hashes.
///
/// Fake ticket and TMD signatures used by standard homebrew-installable CIAs
/// are deliberately not treated as integrity failures.
///
/// # Errors
///
/// Returns an error for malformed section ranges, content hash mismatches,
/// inconsistent title IDs, or invalid main NCCH content.
pub fn inspect_cia(path: &Path) -> Result<CiaInspection> {
    let mut file = File::open(path)
        .map_err(|error| PipelineError::io(format!("open {}", path.display()), error))?;
    let file_size = file
        .metadata()
        .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
        .len();
    let header_size = u64::from(read_u32(&mut file, 0)?);
    if header_size < 0x20 || header_size > file_size {
        return Err(PipelineError::Message("invalid CIA header size".to_owned()));
    }
    let cert_size = u64::from(read_u32(&mut file, 0x08)?);
    let ticket_size = u64::from(read_u32(&mut file, 0x0c)?);
    let tmd_size = u64::from(read_u32(&mut file, 0x10)?);
    let meta_size = u64::from(read_u32(&mut file, 0x14)?);
    let recorded_content_size = read_u64(&mut file, 0x18)?;
    let cert_offset = align_64(header_size)?;
    let ticket_offset = align_64(checked_end(cert_offset, cert_size, "CIA certificate")?)?;
    let tmd_offset = align_64(checked_end(ticket_offset, ticket_size, "CIA ticket")?)?;
    let content_offset = align_64(checked_end(tmd_offset, tmd_size, "CIA TMD")?)?;
    ensure_range(cert_offset, cert_size, file_size, "CIA certificate")?;
    ensure_range(ticket_offset, ticket_size, file_size, "CIA ticket")?;
    ensure_range(tmd_offset, tmd_size, file_size, "CIA TMD")?;

    if tmd_size < 0xb04 {
        return Err(PipelineError::Message("CIA TMD is too small".to_owned()));
    }
    let title_id_bytes = read_array::<8>(&mut file, tmd_offset + 0x18c)?;
    let title_id = format!("{:016X}", u64::from_be_bytes(title_id_bytes));
    let content_count = usize::from(read_u16_be(&mut file, tmd_offset + 0x1de)?);
    if content_count == 0 {
        return Err(PipelineError::Message("CIA contains no content".to_owned()));
    }
    let records_size = (content_count as u64)
        .checked_mul(0x30)
        .ok_or_else(|| PipelineError::Message("CIA content records overflow".to_owned()))?;
    ensure_range(
        tmd_offset + 0xb04,
        records_size,
        tmd_offset + tmd_size,
        "CIA content records",
    )?;

    let mut cursor = content_offset;
    let mut total = 0_u64;
    let mut main_content = None;
    for index in 0..content_count {
        let record = tmd_offset + 0xb04 + index as u64 * 0x30;
        let content_index = read_u16_be(&mut file, record + 4)?;
        let size = read_u64_be(&mut file, record + 8)?;
        let expected_hash = read_array::<32>(&mut file, record + 0x10)?;
        ensure_range(cursor, size, file_size, "CIA content")?;
        if sha256_region(&mut file, cursor, size)? != expected_hash {
            return Err(PipelineError::Message(format!(
                "CIA content hash mismatch for index {content_index}"
            )));
        }
        if content_index == 0 {
            main_content = Some((cursor, size));
        }
        cursor = checked_end(cursor, size, "CIA content")?;
        total = total
            .checked_add(size)
            .ok_or_else(|| PipelineError::Message("CIA content size overflows".to_owned()))?;
    }
    if total != recorded_content_size {
        return Err(PipelineError::Message(format!(
            "CIA content size mismatch: header={recorded_content_size} records={total}"
        )));
    }
    let (main_offset, main_size) = main_content
        .ok_or_else(|| PipelineError::Message("CIA has no main content index 0".to_owned()))?;
    let main = inspect_ncch(&mut file, main_offset, main_size)?;
    if main.title_id != title_id {
        return Err(PipelineError::Message(format!(
            "CIA title ID differs between TMD ({title_id}) and NCCH ({})",
            main.title_id
        )));
    }
    let meta_offset = align_64(cursor)?;
    ensure_range(meta_offset, meta_size, file_size, "CIA metadata")?;
    Ok(CiaInspection {
        title_id,
        content_count,
        content_bytes: total,
    })
}

fn inspect_ncch(file: &mut File, offset: u64, size: u64) -> Result<CciInspection> {
    let end = checked_end(offset, size, "NCCH content")?;
    expect_magic(file, offset + NCCH_MAGIC_OFFSET, *b"NCCH", "NCCH")?;
    let declared_size = u64::from(read_u32(file, offset + 0x104)?) * MEDIA_UNIT;
    if declared_size == 0 || declared_size > size {
        return Err(PipelineError::Message(
            "invalid CIA NCCH content size".to_owned(),
        ));
    }
    let title_id = format!(
        "{:016X}",
        u64::from_le_bytes(read_array::<8>(file, offset + TITLE_ID_OFFSET)?)
    );
    let flags = read_array::<8>(file, offset + FLAGS_OFFSET)?;
    let mut verified_regions = 0;
    let ext_header_size = u64::from(read_u32(file, offset + EXT_HEADER_SIZE_OFFSET)?);
    if ext_header_size > 0 {
        verify_region(
            file,
            offset + EXT_HEADER_HASH_OFFSET,
            offset + EXT_HEADER_OFFSET,
            ext_header_size,
            end,
            "extended header",
        )?;
        verified_regions += 1;
    }
    let exefs_hash_units = u64::from(read_u32(file, offset + EXEFS_HASH_SIZE_FIELD)?);
    if exefs_hash_units == 0 {
        return Err(PipelineError::Message(
            "CIA main NCCH has no verifiable ExeFS hash region".to_owned(),
        ));
    }
    let exefs_offset = u64::from(read_u32(file, offset + EXEFS_OFFSET_FIELD)?) * MEDIA_UNIT;
    verify_region(
        file,
        offset + EXEFS_HASH_OFFSET,
        offset + exefs_offset,
        exefs_hash_units * MEDIA_UNIT,
        end,
        "ExeFS",
    )?;
    verified_regions += 1;
    let romfs_hash_units = u64::from(read_u32(file, offset + ROMFS_HASH_SIZE_FIELD)?);
    if romfs_hash_units > 0 {
        let romfs_offset = u64::from(read_u32(file, offset + ROMFS_OFFSET_FIELD)?) * MEDIA_UNIT;
        verify_region(
            file,
            offset + ROMFS_HASH_OFFSET,
            offset + romfs_offset,
            romfs_hash_units * MEDIA_UNIT,
            end,
            "RomFS",
        )?;
        verified_regions += 1;
    }
    Ok(CciInspection {
        title_id,
        partition_offset: offset,
        flags,
        verified_regions,
    })
}

fn sha256_region(file: &mut File, offset: u64, size: u64) -> Result<[u8; 32]> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| PipelineError::io(format!("seek to 0x{offset:X}"), error))?;
    let mut remaining = size;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    while remaining > 0 {
        let wanted =
            usize::try_from(remaining.min(buffer.len() as u64)).expect("bounded by buffer length");
        file.read_exact(&mut buffer[..wanted])
            .map_err(|error| PipelineError::io("read CIA content", error))?;
        digest.update(&buffer[..wanted]);
        remaining -= wanted as u64;
    }
    Ok(digest.finalize().into())
}

fn align_64(value: u64) -> Result<u64> {
    value
        .checked_add(0x3f)
        .map(|rounded| rounded & !0x3f)
        .ok_or_else(|| PipelineError::Message("CIA section alignment overflows".to_owned()))
}

fn checked_end(offset: u64, size: u64, name: &str) -> Result<u64> {
    offset
        .checked_add(size)
        .ok_or_else(|| PipelineError::Message(format!("{name} range overflows")))
}

fn read_u16_be(file: &mut File, offset: u64) -> Result<u16> {
    Ok(u16::from_be_bytes(read_array(file, offset)?))
}

fn read_u64(file: &mut File, offset: u64) -> Result<u64> {
    Ok(u64::from_le_bytes(read_array(file, offset)?))
}

fn read_u64_be(file: &mut File, offset: u64) -> Result<u64> {
    Ok(u64::from_be_bytes(read_array(file, offset)?))
}

fn identify(file: &mut File, file_size: u64) -> Result<CciIdentity> {
    expect_magic(file, NCSD_MAGIC_OFFSET, *b"NCSD", "NCSD")?;

    let partition_offset_units = read_u32(file, PARTITION_TABLE_OFFSET)?;
    let partition_size_units = read_u32(file, PARTITION_TABLE_OFFSET + 4)?;
    if partition_offset_units == 0 || partition_size_units == 0 {
        return Err(PipelineError::Message(
            "CCI has no main NCCH partition".to_owned(),
        ));
    }
    let partition_offset = u64::from(partition_offset_units) * MEDIA_UNIT;
    let partition_size = u64::from(partition_size_units) * MEDIA_UNIT;
    ensure_range(partition_offset, partition_size, file_size, "main NCCH")?;
    expect_magic(file, partition_offset + NCCH_MAGIC_OFFSET, *b"NCCH", "NCCH")?;

    let title_id_bytes = read_array::<8>(file, partition_offset + TITLE_ID_OFFSET)?;
    let title_id = format!("{:016X}", u64::from_le_bytes(title_id_bytes));
    Ok(CciIdentity {
        title_id,
        partition_offset,
        partition_size,
    })
}

fn verify_region(
    file: &mut File,
    expected_hash_offset: u64,
    data_offset: u64,
    size: u64,
    partition_end: u64,
    name: &str,
) -> Result<()> {
    ensure_range(data_offset, size, partition_end, name)?;
    let expected = read_array::<32>(file, expected_hash_offset)?;
    file.seek(SeekFrom::Start(data_offset))
        .map_err(|error| PipelineError::io(format!("seek to {name}"), error))?;
    let mut remaining = size;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    while remaining > 0 {
        let wanted = usize::try_from(remaining.min(buffer.len() as u64))
            .expect("bounded by the fixed buffer length");
        file.read_exact(&mut buffer[..wanted])
            .map_err(|error| PipelineError::io(format!("read {name}"), error))?;
        digest.update(&buffer[..wanted]);
        remaining -= wanted as u64;
    }
    let actual: [u8; 32] = digest.finalize().into();
    if actual != expected {
        return Err(PipelineError::Message(format!(
            "main NCCH {name} hash mismatch; content is encrypted or corrupt"
        )));
    }
    Ok(())
}

fn expect_magic(file: &mut File, offset: u64, expected: [u8; 4], name: &str) -> Result<()> {
    let actual = read_array::<4>(file, offset)?;
    if actual != expected {
        return Err(PipelineError::Message(format!(
            "invalid {name} magic at 0x{offset:X}"
        )));
    }
    Ok(())
}

fn read_u32(file: &mut File, offset: u64) -> Result<u32> {
    Ok(u32::from_le_bytes(read_array(file, offset)?))
}

fn read_array<const N: usize>(file: &mut File, offset: u64) -> Result<[u8; N]> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| PipelineError::io(format!("seek to 0x{offset:X}"), error))?;
    let mut bytes = [0_u8; N];
    file.read_exact(&mut bytes)
        .map_err(|error| PipelineError::io(format!("read at 0x{offset:X}"), error))?;
    Ok(bytes)
}

fn ensure_range(offset: u64, size: u64, end: u64, name: &str) -> Result<()> {
    let range_end = offset
        .checked_add(size)
        .ok_or_else(|| PipelineError::Message(format!("{name} range overflows")))?;
    if range_end > end {
        return Err(PipelineError::Message(format!(
            "{name} extends beyond its containing image"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use sha2::{Digest, Sha256};

    use super::{inspect_cci, inspect_cia};

    #[test]
    fn proves_decrypted_payload_with_bad_crypto_flags() {
        let path = fixture_path("bad-flags");
        fs::write(&path, fixture(false)).expect("write fixture");
        let inspection = inspect_cci(&path).expect("inspect fixture");
        assert_eq!(inspection.title_id, "0004000000123400");
        assert!(!inspection.is_marked_decrypted());
        assert_eq!(inspection.verified_regions, 3);
        assert_eq!(inspection.normalized_flags()[3], 0);
        assert_eq!(inspection.normalized_flags()[7], 4);
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn rejects_payload_whose_decrypted_hash_does_not_match() {
        let path = fixture_path("encrypted");
        let mut bytes = fixture(false);
        bytes[0x4800] ^= 0xff;
        fs::write(&path, bytes).expect("write fixture");
        assert!(inspect_cci(&path).is_err());
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn validates_cia_content_and_nested_ncch_hashes() {
        let path = fixture_path("cia").with_extension("cia");
        fs::write(&path, cia_fixture()).expect("write CIA fixture");
        let inspection = inspect_cia(&path).expect("inspect CIA");
        assert_eq!(inspection.title_id, "0004000000123400");
        assert_eq!(inspection.content_count, 1);
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn rejects_cia_with_changed_content() {
        let path = fixture_path("bad-cia").with_extension("cia");
        let mut bytes = cia_fixture();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&path, bytes).expect("write CIA fixture");
        assert!(inspect_cia(&path).is_err());
        fs::remove_file(path).expect("remove fixture");
    }

    fn cia_fixture() -> Vec<u8> {
        const TMD_OFFSET: usize = 0x2040;
        const CONTENT_OFFSET: usize = 0x2b80;
        const PARTITION: usize = 0x4000;
        let cci = fixture(true);
        let content = &cci[PARTITION..];
        let mut cia = vec![0_u8; CONTENT_OFFSET + content.len()];
        cia[0..4].copy_from_slice(&0x2020_u32.to_le_bytes());
        cia[0x10..0x14].copy_from_slice(&0x0b34_u32.to_le_bytes());
        cia[0x18..0x20].copy_from_slice(&(content.len() as u64).to_le_bytes());
        cia[TMD_OFFSET + 0x18c..TMD_OFFSET + 0x194]
            .copy_from_slice(&0x0004_0000_0012_3400_u64.to_be_bytes());
        cia[TMD_OFFSET + 0x1de..TMD_OFFSET + 0x1e0].copy_from_slice(&1_u16.to_be_bytes());
        let record = TMD_OFFSET + 0xb04;
        cia[record + 8..record + 0x10].copy_from_slice(&(content.len() as u64).to_be_bytes());
        cia[record + 0x10..record + 0x30].copy_from_slice(&Sha256::digest(content));
        cia[CONTENT_OFFSET..].copy_from_slice(content);
        cia
    }

    fn fixture(marked_decrypted: bool) -> Vec<u8> {
        const PARTITION: usize = 0x4000;
        let mut bytes = vec![0_u8; 0x4c00];
        bytes[0x100..0x104].copy_from_slice(b"NCSD");
        bytes[0x120..0x124].copy_from_slice(&(0x20_u32).to_le_bytes());
        bytes[0x124..0x128].copy_from_slice(&(6_u32).to_le_bytes());
        bytes[PARTITION + 0x100..PARTITION + 0x104].copy_from_slice(b"NCCH");
        bytes[PARTITION + 0x104..PARTITION + 0x108].copy_from_slice(&(6_u32).to_le_bytes());
        bytes[PARTITION + 0x118..PARTITION + 0x120]
            .copy_from_slice(&0x0004_0000_0012_3400_u64.to_le_bytes());
        bytes[PARTITION + 0x180..PARTITION + 0x184].copy_from_slice(&(0x400_u32).to_le_bytes());
        bytes[PARTITION + 0x188 + 3] = 1;
        bytes[PARTITION + 0x188 + 7] = if marked_decrypted { 4 } else { 0 };
        bytes[PARTITION + 0x1a0..PARTITION + 0x1a4].copy_from_slice(&(4_u32).to_le_bytes());
        bytes[PARTITION + 0x1a8..PARTITION + 0x1ac].copy_from_slice(&(1_u32).to_le_bytes());
        bytes[PARTITION + 0x1b0..PARTITION + 0x1b4].copy_from_slice(&(5_u32).to_le_bytes());
        bytes[PARTITION + 0x1b8..PARTITION + 0x1bc].copy_from_slice(&(1_u32).to_le_bytes());
        bytes[PARTITION + 0x200..PARTITION + 0x600].fill(0x11);
        bytes[PARTITION + 0x800..PARTITION + 0xa00].fill(0x22);
        bytes[PARTITION + 0x800..PARTITION + 0x805].copy_from_slice(b".code");
        bytes[PARTITION + 0xa00..PARTITION + 0xc00].fill(0x33);
        bytes[PARTITION + 0xa00..PARTITION + 0xa04].copy_from_slice(b"IVFC");
        set_hash(&mut bytes, PARTITION + 0x160, PARTITION + 0x200, 0x400);
        set_hash(&mut bytes, PARTITION + 0x1c0, PARTITION + 0x800, 0x200);
        set_hash(&mut bytes, PARTITION + 0x1e0, PARTITION + 0xa00, 0x200);
        bytes
    }

    fn set_hash(bytes: &mut [u8], hash_offset: usize, data_offset: usize, size: usize) {
        let hash = Sha256::digest(&bytes[data_offset..data_offset + size]);
        bytes[hash_offset..hash_offset + 32].copy_from_slice(&hash);
    }

    fn fixture_path(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rom-pipeline-3ds-{label}-{}-{nonce}.cci",
            std::process::id()
        ))
    }
}
