use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use rom_pipeline_core::{
    Job, JobOutcome, PipelineError, Result, StateStore, StopToken, sha256_file,
};

use crate::adapter::{Nintendo3dsAdapter, completion_record};
use crate::command;
use crate::format::{CciInspection, inspect_cci, inspect_cia};

const COPY_BUFFER_SIZE: usize = 1024 * 1024;

struct JobPaths {
    source: PathBuf,
    work: PathBuf,
    extracted: PathBuf,
    converted: PathBuf,
    output: PathBuf,
    partial: PathBuf,
    log: PathBuf,
}

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
    let actual_size = fs::metadata(&source)
        .map_err(|error| PipelineError::io(format!("stat {}", source.display()), error))?
        .len();
    if actual_size != artifact.expected_size {
        return Err(PipelineError::Message(format!(
            "3DS ZIP size mismatch for {}: expected={} actual={actual_size}",
            artifact.name, artifact.expected_size
        )));
    }

    let work_root = adapter.profile().work_dir.join("groups");
    let work = work_root.join(&job.id);
    reset_work(&work, &work_root)?;
    let output = adapter.output_path(job);
    let paths = JobPaths {
        source,
        extracted: work.join("cartridge"),
        converted: work.join("converted"),
        partial: output.with_extension("cia.partial"),
        output,
        log: adapter
            .profile()
            .log_dir
            .join("groups")
            .join(format!("{}.log", job.id)),
        work,
    };
    fs::create_dir_all(&paths.extracted).map_err(|error| {
        PipelineError::io(format!("create {}", paths.extracted.display()), error)
    })?;
    fs::create_dir_all(&paths.converted).map_err(|error| {
        PipelineError::io(format!("create {}", paths.converted.display()), error)
    })?;
    fs::write(&paths.log, format!("source={}\n", paths.source.display()))
        .map_err(|error| PipelineError::io(format!("write {}", paths.log.display()), error))?;
    execute(adapter, job, state, stop, &paths)
}

fn execute(
    adapter: &Nintendo3dsAdapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
    paths: &JobPaths,
) -> Result<JobOutcome> {
    let settings = adapter.settings()?;
    state.write_current(&format!(
        "group={} step=verify-zip output={}",
        job.id, job.output_name
    ))?;
    state.log(&format!(
        "BEGIN 3DS ZIP group={} output={}",
        job.id, job.output_name
    ))?;
    command::run_logged(
        &settings.seven_zip,
        &["t".into(), paths.source.as_os_str().to_owned()],
        &paths.log,
    )?;
    check_stop(stop)?;

    state.write_current(&format!(
        "group={} step=extract-cartridge output={}",
        job.id, job.output_name
    ))?;
    command::run_logged(
        &settings.seven_zip,
        &[
            "e".into(),
            "-y".into(),
            format!("-o{}", paths.extracted.display()).into(),
            paths.source.as_os_str().to_owned(),
            "*.3ds".into(),
            "*.cci".into(),
        ],
        &paths.log,
    )?;
    let cartridge = exactly_one_extension(&paths.extracted, &["3ds", "cci"])?;
    state.write_current(&format!(
        "group={} step=validate-cartridge output={}",
        job.id, job.output_name
    ))?;
    let inspection = inspect_cci(&cartridge)?;
    normalize_flags(&cartridge, &inspection)?;
    let normalized = inspect_cci(&cartridge)?;
    if !normalized.is_marked_decrypted() || normalized.title_id != inspection.title_id {
        return Err(PipelineError::Message(
            "normalized 3DS cartridge identity or crypto flags are invalid".to_owned(),
        ));
    }
    check_stop(stop)?;

    state.write_current(&format!(
        "group={} step=create-cia output={}",
        job.id, job.output_name
    ))?;
    command::run_logged(
        &settings.python,
        &[
            settings.converter.as_os_str().to_owned(),
            "--overwrite".into(),
            format!("--output={}", paths.converted.display()).into(),
            cartridge.as_os_str().to_owned(),
        ],
        &paths.log,
    )?;
    let cia = exactly_one_extension(&paths.converted, &["cia"])?;
    validate_cia(adapter, &cia, &inspection.title_id, &paths.log)?;
    check_stop(stop)?;

    fs::create_dir_all(&adapter.profile().output_dir).map_err(|error| {
        PipelineError::io(
            format!("create {}", adapter.profile().output_dir.display()),
            error,
        )
    })?;
    remove_if_exists(&paths.partial)?;
    state.write_current(&format!(
        "group={} step=stage-cia output={}",
        job.id, job.output_name
    ))?;
    copy_with_stop(&cia, &paths.partial, stop)?;
    validate_cia(adapter, &paths.partial, &inspection.title_id, &paths.log)?;
    check_stop_or_remove(stop, &paths.partial)?;
    fs::rename(&paths.partial, &paths.output)
        .map_err(|error| PipelineError::io(format!("publish {}", paths.output.display()), error))?;
    let hash = sha256_file(&paths.output)?;
    adapter.move_source_to_done(job, state)?;
    state.write_completion(
        &job.id,
        &completion_record(job, &paths.output, hash.clone())?,
    )?;
    state.log(&format!(
        "COMPLETE 3DS CIA group={} title_id={} sha256={} output={}",
        job.id, inspection.title_id, hash, job.output_name
    ))?;
    let _ = fs::remove_dir_all(&paths.work);
    Ok(JobOutcome::Completed)
}

pub(crate) fn validate_cia(
    adapter: &Nintendo3dsAdapter,
    cia: &Path,
    expected_title_id: &str,
    log: &Path,
) -> Result<()> {
    let inspection = validate_cia_file(adapter, cia, log)?;
    if inspection.title_id != expected_title_id {
        return Err(PipelineError::Message(format!(
            "CIA title ID mismatch: expected={expected_title_id} actual={}",
            inspection.title_id
        )));
    }
    Ok(())
}

pub(crate) fn validate_cia_file(
    adapter: &Nintendo3dsAdapter,
    cia: &Path,
    log: &Path,
) -> Result<crate::format::CiaInspection> {
    let inspection = inspect_cia(cia)?;
    command::run_logged(
        &adapter.settings()?.ctrtool,
        &["-y".into(), "-v".into(), cia.as_os_str().to_owned()],
        log,
    )?;
    Ok(inspection)
}

fn exactly_one_extension(root: &Path, extensions: &[&str]) -> Result<PathBuf> {
    let mut matches = Vec::new();
    for entry in fs::read_dir(root)
        .map_err(|error| PipelineError::io(format!("read {}", root.display()), error))?
    {
        let entry =
            entry.map_err(|error| PipelineError::io(format!("read {}", root.display()), error))?;
        let path = entry.path();
        if entry
            .file_type()
            .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
            .is_file()
            && path.extension().is_some_and(|value| {
                extensions
                    .iter()
                    .any(|extension| value.eq_ignore_ascii_case(extension))
            })
        {
            matches.push(path);
        }
    }
    if matches.len() == 1 {
        Ok(matches.remove(0))
    } else {
        Err(PipelineError::Message(format!(
            "expected exactly one {} file in {}; found {}",
            extensions.join("/"),
            root.display(),
            matches.len()
        )))
    }
}

pub(crate) fn normalize_flags(path: &Path, inspection: &CciInspection) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| PipelineError::io(format!("open {}", path.display()), error))?;
    file.seek(SeekFrom::Start(inspection.partition_offset + 0x188))
        .map_err(|error| PipelineError::io(format!("seek {}", path.display()), error))?;
    file.write_all(&inspection.normalized_flags())
        .map_err(|error| PipelineError::io(format!("normalize {}", path.display()), error))?;
    file.sync_all()
        .map_err(|error| PipelineError::io(format!("sync {}", path.display()), error))
}

pub(crate) fn copy_with_stop(source: &Path, destination: &Path, stop: &StopToken) -> Result<()> {
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
