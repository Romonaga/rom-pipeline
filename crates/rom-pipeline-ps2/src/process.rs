use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use rom_pipeline_core::{
    Job, JobOutcome, PipelineError, Result, StateStore, StopToken, sha256_file,
};

use crate::adapter::{Ps2Adapter, completion_record};
use crate::command;
use crate::media::{DiscFormat, inspect_disc};

const COPY_BUFFER_SIZE: usize = 1024 * 1024;

struct JobPaths {
    source: PathBuf,
    work: PathBuf,
    chd: PathBuf,
    chd_partial: PathBuf,
    preserved: PathBuf,
    group_log: PathBuf,
}

pub fn process_job(
    adapter: &Ps2Adapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
) -> Result<JobOutcome> {
    let artifact = job
        .sources
        .first()
        .ok_or_else(|| PipelineError::Message("PS2 job has no source".to_owned()))?;
    let source = adapter.locate(&artifact.name)?.ok_or_else(|| {
        PipelineError::MissingPath(adapter.profile().source_dir.join(&artifact.name))
    })?;
    let metadata = fs::metadata(&source)
        .map_err(|error| PipelineError::io(format!("stat {}", source.display()), error))?;
    if metadata.len() != artifact.expected_size {
        return Err(PipelineError::Message(format!(
            "PS2 source size mismatch for {}: expected={} actual={}",
            artifact.name,
            artifact.expected_size,
            metadata.len()
        )));
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
        "group={} step=inspect-disc output={}",
        job.id, job.output_name
    ))?;
    state.log(&format!(
        "BEGIN PS2 group={} output={}",
        job.id, job.output_name
    ))?;
    let format = inspect_disc(&source)?;
    state.log(&format!(
        "VALIDATED PS2 source layout={} name={}",
        format.as_str(),
        artifact.name
    ))?;
    check_stop(stop)?;

    let work = adapter.profile().work_dir.join("groups").join(&job.id);
    let chd = adapter.profile().output_dir.join(&job.output_name);
    let paths = JobPaths {
        source,
        work,
        chd_partial: chd.with_extension("chd.partial"),
        chd,
        preserved: adapter.profile().output_dir.join(&artifact.name),
        group_log,
    };
    reset_work(&paths.work, &adapter.profile().work_dir.join("groups"))?;
    remove_if_exists(&paths.chd_partial)?;
    if let Some(outcome) = resume_existing(adapter, job, state, format, metadata.len(), &paths)? {
        return Ok(outcome);
    }
    let (output, output_name) =
        create_verified_output(adapter, job, state, stop, format, metadata.len(), &paths)?;
    complete_output(adapter, job, state, &output, &output_name, &paths.work)
}

fn resume_existing(
    adapter: &Ps2Adapter,
    job: &Job,
    state: &StateStore,
    format: DiscFormat,
    source_size: u64,
    paths: &JobPaths,
) -> Result<Option<JobOutcome>> {
    let artifact = &job.sources[0];
    if paths.preserved.is_file() {
        state.write_current(&format!(
            "group={} step=resume-verify-preserved output={}",
            job.id, artifact.name
        ))?;
        if fs::metadata(&paths.preserved)
            .map_err(|error| {
                PipelineError::io(format!("stat {}", paths.preserved.display()), error)
            })?
            .len()
            != source_size
            || sha256_file(&paths.preserved)? != sha256_file(&paths.source)?
        {
            return Err(PipelineError::Message(format!(
                "existing preserved PS2 output does not match its source: {}",
                paths.preserved.display()
            )));
        }
        state.log(&format!(
            "RESUMED verified PS2 preserved output={}",
            artifact.name
        ))?;
        return complete_output(
            adapter,
            job,
            state,
            &paths.preserved,
            &artifact.name,
            &paths.work,
        )
        .map(Some);
    }

    if paths.chd.is_file() {
        state.write_current(&format!(
            "group={} step=resume-verify-chd output={}",
            job.id, job.output_name
        ))?;
        verify_chd(adapter, &paths.chd, &paths.group_log)?;
        if adapter.settings()?.verify_round_trip {
            verify_round_trip(
                adapter,
                format,
                &paths.source,
                &paths.chd,
                &paths.work,
                &paths.group_log,
            )?;
        }
        let chd_size = fs::metadata(&paths.chd)
            .map_err(|error| PipelineError::io(format!("stat {}", paths.chd.display()), error))?
            .len();
        let savings = savings_percent(source_size, chd_size);
        let settings = adapter.settings()?;
        if !(settings.preserve_when_compression_is_not_worthwhile
            && savings < u64::from(settings.minimum_savings_percent))
        {
            state.log(&format!(
                "RESUMED verified PS2 CHD savings={} output={}",
                savings, job.output_name
            ))?;
            return complete_output(
                adapter,
                job,
                state,
                &paths.chd,
                &job.output_name,
                &paths.work,
            )
            .map(Some);
        }
        fs::remove_file(&paths.chd)
            .map_err(|error| PipelineError::io(format!("remove {}", paths.chd.display()), error))?;
        reset_work(&paths.work, &adapter.profile().work_dir.join("groups"))?;
    }
    Ok(None)
}

fn create_verified_output(
    adapter: &Ps2Adapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
    format: DiscFormat,
    source_size: u64,
    paths: &JobPaths,
) -> Result<(PathBuf, String)> {
    let artifact = &job.sources[0];
    state.write_current(&format!(
        "group={} step=create-chd output={}",
        job.id, job.output_name
    ))?;
    create_chd(
        adapter,
        format,
        &paths.source,
        &paths.chd_partial,
        &paths.work,
        &paths.group_log,
    )?;
    check_stop_or_remove(stop, &paths.chd_partial)?;

    state.write_current(&format!(
        "group={} step=verify-chd output={}",
        job.id, job.output_name
    ))?;
    verify_chd(adapter, &paths.chd_partial, &paths.group_log)?;
    if adapter.settings()?.verify_round_trip {
        state.write_current(&format!(
            "group={} step=roundtrip-chd output={}",
            job.id, job.output_name
        ))?;
        verify_round_trip(
            adapter,
            format,
            &paths.source,
            &paths.chd_partial,
            &paths.work,
            &paths.group_log,
        )?;
    }
    check_stop_or_remove(stop, &paths.chd_partial)?;

    let chd_size = fs::metadata(&paths.chd_partial)
        .map_err(|error| PipelineError::io(format!("stat {}", paths.chd_partial.display()), error))?
        .len();
    let savings = savings_percent(source_size, chd_size);
    let settings = adapter.settings()?;
    let (output, output_name) = if settings.preserve_when_compression_is_not_worthwhile
        && savings < u64::from(settings.minimum_savings_percent)
    {
        fs::remove_file(&paths.chd_partial).map_err(|error| {
            PipelineError::io(format!("remove {}", paths.chd_partial.display()), error)
        })?;
        let output_name = artifact.name.clone();
        let output = paths.preserved.clone();
        let partial = append_partial(&output);
        remove_if_exists(&partial)?;
        state.write_current(&format!(
            "group={} step=preserve-source output={}",
            job.id, output_name
        ))?;
        copy_with_stop(&paths.source, &partial, stop)?;
        if sha256_file(&paths.source)? != sha256_file(&partial)? {
            let _ = fs::remove_file(&partial);
            return Err(PipelineError::Message(
                "preserved PS2 source copy hash mismatch".to_owned(),
            ));
        }
        fs::rename(&partial, &output)
            .map_err(|error| PipelineError::io(format!("publish {}", output.display()), error))?;
        state.log(&format!(
            "PRESERVED PS2 source format savings={} threshold={} output={}",
            savings, settings.minimum_savings_percent, output_name
        ))?;
        (output, output_name)
    } else {
        fs::rename(&paths.chd_partial, &paths.chd).map_err(|error| {
            PipelineError::io(format!("publish {}", paths.chd.display()), error)
        })?;
        state.log(&format!(
            "COMPRESSED PS2 savings={} source_bytes={} output_bytes={} output={}",
            savings, source_size, chd_size, job.output_name
        ))?;
        (paths.chd.clone(), job.output_name.clone())
    };
    Ok((output, output_name))
}

fn complete_output(
    adapter: &Ps2Adapter,
    job: &Job,
    state: &StateStore,
    output: &Path,
    output_name: &str,
    work: &Path,
) -> Result<JobOutcome> {
    let hash = sha256_file(output)?;
    adapter.move_sources_to_done(job, state)?;
    state.write_completion(
        &job.id,
        &completion_record(job, output, output_name.to_owned(), hash.clone())?,
    )?;
    state.log(&format!(
        "COMPLETE PS2 group={} sha256={} output={}",
        job.id, hash, output_name
    ))?;
    let _ = fs::remove_dir_all(work);
    Ok(JobOutcome::Completed)
}

fn create_chd(
    adapter: &Ps2Adapter,
    format: DiscFormat,
    source: &Path,
    output: &Path,
    work: &Path,
    log: &Path,
) -> Result<()> {
    let mut arguments = Vec::<OsString>::new();
    match format {
        DiscFormat::DvdLogical => {
            arguments.extend(["createdvd".into(), "-i".into()]);
            arguments.push(source.as_os_str().to_owned());
        }
        DiscFormat::CdMode2Raw => {
            let cue = work.join("source.cue");
            let linked_source = work.join("source.bin");
            std::os::unix::fs::symlink(source, &linked_source).map_err(|error| {
                PipelineError::io(
                    format!("link {} to {}", linked_source.display(), source.display()),
                    error,
                )
            })?;
            write_single_track_cue(&cue)?;
            arguments.extend(["createcd".into(), "-i".into()]);
            arguments.push(cue.as_os_str().to_owned());
        }
    }
    arguments.push("-o".into());
    arguments.push(output.as_os_str().to_owned());
    command::run_logged(&adapter.settings()?.chdman, arguments, log)
}

fn verify_chd(adapter: &Ps2Adapter, chd: &Path, log: &Path) -> Result<()> {
    command::run_logged(
        &adapter.settings()?.chdman,
        ["verify".into(), "-i".into(), chd.as_os_str().to_owned()],
        log,
    )
}

fn verify_round_trip(
    adapter: &Ps2Adapter,
    format: DiscFormat,
    source: &Path,
    chd: &Path,
    work: &Path,
    log: &Path,
) -> Result<()> {
    let extracted = match format {
        DiscFormat::DvdLogical => {
            let output = work.join("roundtrip.iso");
            command::run_logged(
                &adapter.settings()?.chdman,
                [
                    "extractdvd".into(),
                    "-i".into(),
                    chd.as_os_str().to_owned(),
                    "-o".into(),
                    output.as_os_str().to_owned(),
                ],
                log,
            )?;
            output
        }
        DiscFormat::CdMode2Raw => {
            let cue = work.join("roundtrip.cue");
            let bin = work.join("roundtrip.bin");
            command::run_logged(
                &adapter.settings()?.chdman,
                [
                    "extractcd".into(),
                    "-i".into(),
                    chd.as_os_str().to_owned(),
                    "-o".into(),
                    cue.as_os_str().to_owned(),
                    "-ob".into(),
                    bin.as_os_str().to_owned(),
                ],
                log,
            )?;
            bin
        }
    };
    if sha256_file(source)? != sha256_file(&extracted)? {
        return Err(PipelineError::Message(format!(
            "PS2 CHD round-trip hash mismatch: {}",
            source.display()
        )));
    }
    Ok(())
}

fn write_single_track_cue(path: &Path) -> Result<()> {
    fs::write(
        path,
        "FILE \"source.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
    )
    .map_err(|error| PipelineError::io(format!("write {}", path.display()), error))
}

fn savings_percent(source: u64, output: u64) -> u64 {
    if source == 0 || output >= source {
        0
    } else {
        (source - output).saturating_mul(100) / source
    }
}

fn append_partial(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
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
        let count = reader
            .read(&mut buffer)
            .map_err(|error| PipelineError::io(format!("read {}", source.display()), error))?;
        if count == 0 {
            break;
        }
        writer.write_all(&buffer[..count]).map_err(|error| {
            PipelineError::io(format!("write {}", destination.display()), error)
        })?;
    }
    writer
        .flush()
        .map_err(|error| PipelineError::io(format!("flush {}", destination.display()), error))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|error| PipelineError::io(format!("sync {}", destination.display()), error))
}

fn reset_work(path: &Path, root: &Path) -> Result<()> {
    if path.parent() != Some(root) {
        return Err(PipelineError::Message(format!(
            "refusing to reset work outside owned root: {}",
            path.display()
        )));
    }
    if path.exists() {
        fs::remove_dir_all(path)
            .map_err(|error| PipelineError::io(format!("remove {}", path.display()), error))?;
    }
    fs::create_dir_all(path)
        .map_err(|error| PipelineError::io(format!("create {}", path.display()), error))
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PipelineError::io(
            format!("remove {}", path.display()),
            error,
        )),
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
    use super::savings_percent;

    #[test]
    fn savings_percentage_is_bounded_and_truncated() {
        assert_eq!(savings_percent(100, 80), 20);
        assert_eq!(savings_percent(100, 101), 0);
        assert_eq!(savings_percent(0, 0), 0);
    }
}
