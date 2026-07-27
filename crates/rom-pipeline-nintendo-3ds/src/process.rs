use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use rom_pipeline_core::{
    Job, JobOutcome, PipelineError, Result, StateStore, StopToken, sha256_file,
};

use crate::adapter::{Nintendo3dsAdapter, completion_record};
use crate::format::{CciInspection, inspect_cci};

const COPY_BUFFER_SIZE: usize = 1024 * 1024;

pub fn process_job(
    adapter: &Nintendo3dsAdapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
) -> Result<JobOutcome> {
    let artifact = job
        .sources
        .first()
        .ok_or_else(|| PipelineError::Message("3DS job has no source".to_owned()))?;
    let source = adapter.locate(&artifact.name)?.ok_or_else(|| {
        PipelineError::MissingPath(adapter.profile().source_dir.join(&artifact.name))
    })?;
    let output = adapter.output_path(job);

    if output.exists() {
        return adopt_existing_output(adapter, job, state, &source, &output);
    }

    fs::create_dir_all(&adapter.profile().output_dir).map_err(|error| {
        PipelineError::io(
            format!("create {}", adapter.profile().output_dir.display()),
            error,
        )
    })?;
    fs::create_dir_all(adapter.profile().log_dir.join("groups")).map_err(|error| {
        PipelineError::io(
            format!(
                "create {}",
                adapter.profile().log_dir.join("groups").display()
            ),
            error,
        )
    })?;
    let group_log = adapter
        .profile()
        .log_dir
        .join("groups")
        .join(format!("{}.log", job.id));
    fs::write(&group_log, format!("source={}\n", source.display()))
        .map_err(|error| PipelineError::io(format!("write {}", group_log.display()), error))?;

    state.write_current(&format!(
        "group={} step=validate-source output={}",
        job.id, job.output_name
    ))?;
    state.log(&format!(
        "BEGIN group={} output={}",
        job.id, job.output_name
    ))?;
    let inspection = inspect_cci(&source)?;
    state.log(&format!(
        "VALIDATED decrypted main NCCH title_id={} regions={}",
        inspection.title_id, inspection.verified_regions
    ))?;
    check_stop(stop)?;

    let partial = output.with_extension("cci.partial");
    state.write_current(&format!(
        "group={} step=copy-normalize output={}",
        job.id, job.output_name
    ))?;
    copy_with_stop(&source, &partial, stop)?;
    normalize_flags(&partial, &inspection)?;
    check_stop_or_remove(stop, &partial)?;

    state.write_current(&format!(
        "group={} step=validate-output output={}",
        job.id, job.output_name
    ))?;
    validate_normalized_copy(&source, &partial, &inspection)?;
    check_stop_or_remove(stop, &partial)?;
    fs::rename(&partial, &output)
        .map_err(|error| PipelineError::io(format!("publish {}", output.display()), error))?;

    let hash = sha256_file(&output)?;
    adapter.move_source_to_done(job, state)?;
    state.write_completion(&job.id, &completion_record(job, &output, hash.clone())?)?;
    state.log(&format!(
        "COMPLETE group={} sha256={} output={}",
        job.id, hash, job.output_name
    ))?;
    Ok(JobOutcome::Completed)
}

fn adopt_existing_output(
    adapter: &Nintendo3dsAdapter,
    job: &Job,
    state: &StateStore,
    source: &Path,
    output: &Path,
) -> Result<JobOutcome> {
    let inspection = inspect_cci(source)?;
    validate_normalized_copy(source, output, &inspection)?;
    let hash = sha256_file(output)?;
    adapter.move_source_to_done(job, state)?;
    state.write_completion(&job.id, &completion_record(job, output, hash)?)?;
    state.log(&format!(
        "RESUMED validated output and finalized source move: {}",
        job.output_name
    ))?;
    Ok(JobOutcome::Completed)
}

fn copy_with_stop(source: &Path, destination: &Path, stop: &StopToken) -> Result<()> {
    let source_file = File::open(source)
        .map_err(|error| PipelineError::io(format!("open {}", source.display()), error))?;
    let destination_file = File::create(destination)
        .map_err(|error| PipelineError::io(format!("create {}", destination.display()), error))?;
    let mut reader = BufReader::with_capacity(COPY_BUFFER_SIZE, source_file);
    let mut writer = BufWriter::with_capacity(COPY_BUFFER_SIZE, destination_file);
    let mut buffer = vec![0_u8; COPY_BUFFER_SIZE];
    loop {
        if stop.is_requested() {
            drop(writer);
            let _ = fs::remove_file(destination);
            return Err(PipelineError::Interrupted);
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| PipelineError::io(format!("read {}", source.display()), error))?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read]).map_err(|error| {
            PipelineError::io(format!("write {}", destination.display()), error)
        })?;
    }
    writer
        .flush()
        .map_err(|error| PipelineError::io(format!("flush {}", destination.display()), error))
}

fn normalize_flags(path: &Path, inspection: &CciInspection) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| PipelineError::io(format!("open {}", path.display()), error))?;
    let flags = inspection.normalized_flags();
    file.seek(SeekFrom::Start(inspection.partition_offset + 0x188))
        .map_err(|error| PipelineError::io(format!("seek {}", path.display()), error))?;
    file.write_all(&flags)
        .map_err(|error| PipelineError::io(format!("normalize {}", path.display()), error))?;
    file.sync_all()
        .map_err(|error| PipelineError::io(format!("sync {}", path.display()), error))
}

fn validate_normalized_copy(source: &Path, output: &Path, original: &CciInspection) -> Result<()> {
    let normalized = inspect_cci(output)?;
    if normalized.title_id != original.title_id
        || normalized.partition_offset != original.partition_offset
        || normalized.verified_regions != original.verified_regions
    {
        return Err(PipelineError::Message(
            "normalized CCI structure differs from its source".to_owned(),
        ));
    }
    if !normalized.is_marked_decrypted()
        || normalized.flags[3] != 0
        || normalized.flags != original.normalized_flags()
    {
        return Err(PipelineError::Message(
            "normalized CCI crypto flags are invalid".to_owned(),
        ));
    }
    compare_except_flags(source, output, original.flag_offsets())
}

fn compare_except_flags(source: &Path, output: &Path, allowed: [u64; 2]) -> Result<()> {
    let mut left = BufReader::with_capacity(
        COPY_BUFFER_SIZE,
        File::open(source)
            .map_err(|error| PipelineError::io(format!("open {}", source.display()), error))?,
    );
    let mut right = BufReader::with_capacity(
        COPY_BUFFER_SIZE,
        File::open(output)
            .map_err(|error| PipelineError::io(format!("open {}", output.display()), error))?,
    );
    let mut left_buffer = vec![0_u8; COPY_BUFFER_SIZE];
    let mut right_buffer = vec![0_u8; COPY_BUFFER_SIZE];
    let mut position = 0_u64;
    loop {
        let left_read = left
            .read(&mut left_buffer)
            .map_err(|error| PipelineError::io(format!("read {}", source.display()), error))?;
        let right_read = right
            .read(&mut right_buffer)
            .map_err(|error| PipelineError::io(format!("read {}", output.display()), error))?;
        if left_read != right_read {
            return Err(PipelineError::Message(
                "normalized CCI size differs from its source".to_owned(),
            ));
        }
        if left_read == 0 {
            return Ok(());
        }
        for index in 0..left_read {
            let absolute = position + index as u64;
            if left_buffer[index] != right_buffer[index] && !allowed.contains(&absolute) {
                return Err(PipelineError::Message(format!(
                    "normalized CCI differs outside crypto flags at 0x{absolute:X}"
                )));
            }
        }
        position += left_read as u64;
    }
}

fn check_stop(stop: &StopToken) -> Result<()> {
    if stop.is_requested() {
        Err(PipelineError::Interrupted)
    } else {
        Ok(())
    }
}

fn check_stop_or_remove(stop: &StopToken, partial: &Path) -> Result<()> {
    if stop.is_requested() {
        let _ = fs::remove_file(partial);
        Err(PipelineError::Interrupted)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::format::CciInspection;

    use super::{compare_except_flags, normalize_flags};

    #[test]
    fn normalization_changes_only_the_two_allowed_flag_bytes() {
        let source = fixture_path("source");
        let output = fixture_path("output");
        let mut bytes = vec![0x5a_u8; 0x4200];
        bytes[0x418b] = 1;
        bytes[0x418f] = 0;
        fs::write(&source, &bytes).expect("write source");
        fs::write(&output, &bytes).expect("write output");
        let inspection = CciInspection {
            title_id: "0004000000123400".to_owned(),
            partition_offset: 0x4000,
            flags: [0x5a, 0x5a, 0x5a, 1, 0x5a, 0x5a, 0x5a, 0],
            verified_regions: 3,
        };

        normalize_flags(&output, &inspection).expect("normalize");
        compare_except_flags(&source, &output, inspection.flag_offsets()).expect("compare");
        let normalized = fs::read(&output).expect("read output");
        assert_eq!(normalized[0x418b], 0);
        assert_eq!(normalized[0x418f], 4);

        fs::remove_file(source).expect("remove source");
        fs::remove_file(output).expect("remove output");
    }

    #[test]
    fn comparison_rejects_changes_outside_crypto_flags() {
        let source = fixture_path("compare-source");
        let output = fixture_path("compare-output");
        let bytes = vec![0x5a_u8; 0x4200];
        fs::write(&source, &bytes).expect("write source");
        let mut changed = bytes;
        changed[100] ^= 0xff;
        fs::write(&output, changed).expect("write output");

        assert!(compare_except_flags(&source, &output, [0x418b, 0x418f]).is_err());
        fs::remove_file(source).expect("remove source");
        fs::remove_file(output).expect("remove output");
    }

    fn fixture_path(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rom-pipeline-3ds-process-{label}-{}-{nonce}.cci",
            std::process::id()
        ))
    }
}
