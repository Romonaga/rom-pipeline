use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use rom_pipeline_core::{
    Job, JobOutcome, PipelineError, Result, StateStore, StopToken, sha256_file,
};

use crate::adapter::{PspAdapter, completion_record};
use crate::command;
use crate::format::inspect_iso;

pub fn process_job(
    adapter: &PspAdapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
) -> Result<JobOutcome> {
    if stop.is_requested() {
        return Ok(JobOutcome::Interrupted);
    }
    let artifact = job
        .sources
        .first()
        .ok_or_else(|| PipelineError::Message("PSP job has no source".to_owned()))?;
    let source = adapter.locate(&artifact.name)?.ok_or_else(|| {
        PipelineError::MissingPath(adapter.profile().source_dir.join(&artifact.name))
    })?;
    let output = adapter.output_path(job);
    let work = JobWorkspace::prepare(adapter, job)?;

    if output.exists() {
        return adopt_existing_output(adapter, job, state, &source, &output, &work);
    }

    state.write_current(&format!(
        "group={} step=validate-source output={}",
        job.id, job.output_name
    ))?;
    state.log(&format!(
        "BEGIN PSP group={} disc_id={} output={} copies={}",
        job.id,
        artifact.title_id,
        job.output_name,
        job.sources.len()
    ))?;
    let identity = inspect_iso(&source)?;
    if identity.disc_id != artifact.title_id {
        return Err(PipelineError::Message(format!(
            "PSP identity changed after inventory: {}",
            source.display()
        )));
    }
    let source_hash = validate_exact_sources(adapter, job, &source)?;

    let partial = output.with_extension("chd.partial");
    remove_if_exists(&partial)?;
    state.write_current(&format!(
        "group={} step=create-chd output={}",
        job.id, job.output_name
    ))?;
    let creation = create_chd(adapter, &source, &partial, &work.group_log);
    if let Err(error) = creation {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }

    state.write_current(&format!(
        "group={} step=verify-chd output={}",
        job.id, job.output_name
    ))?;
    let validation = validate_chd(adapter, &partial, &source_hash, &work);
    if let Err(error) = validation {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }

    fs::rename(&partial, &output)
        .map_err(|error| PipelineError::io(format!("publish {}", output.display()), error))?;
    let output_hash = sha256_file(&output)?;
    adapter.move_sources_to_done(job, state)?;
    state.write_completion(
        &job.id,
        &completion_record(job, &output, output_hash.clone())?,
    )?;
    state.log(&format!(
        "COMPLETE PSP group={} sha256={} output={}",
        job.id, output_hash, job.output_name
    ))?;
    Ok(JobOutcome::Completed)
}

struct JobWorkspace {
    roundtrip: PathBuf,
    group_log: PathBuf,
}

impl JobWorkspace {
    fn prepare(adapter: &PspAdapter, job: &Job) -> Result<Self> {
        let group_work = adapter.profile().work_dir.join("groups").join(&job.id);
        let group_log = adapter
            .profile()
            .log_dir
            .join("groups")
            .join(format!("{}.log", job.id));
        for path in [
            group_work.as_path(),
            adapter.profile().log_dir.join("groups").as_path(),
            adapter.profile().output_dir.as_path(),
        ] {
            fs::create_dir_all(path)
                .map_err(|error| PipelineError::io(format!("create {}", path.display()), error))?;
        }
        File::create(&group_log)
            .map_err(|error| PipelineError::io(format!("create {}", group_log.display()), error))?;
        Ok(Self {
            roundtrip: group_work.join("roundtrip.iso"),
            group_log,
        })
    }
}

fn create_chd(adapter: &PspAdapter, source: &Path, partial: &Path, log: &Path) -> Result<()> {
    let settings = adapter.settings()?;
    command::run_logged(
        &settings.chdman,
        [
            OsString::from("createdvd"),
            OsString::from("-hs"),
            OsString::from(settings.hunk_size.to_string()),
            OsString::from("-i"),
            source.as_os_str().to_owned(),
            OsString::from("-o"),
            partial.as_os_str().to_owned(),
            OsString::from("-c"),
            OsString::from(&settings.codec),
        ],
        log,
    )
}

fn validate_chd(
    adapter: &PspAdapter,
    chd: &Path,
    source_hash: &str,
    work: &JobWorkspace,
) -> Result<()> {
    let settings = adapter.settings()?;
    command::run_logged(
        &settings.chdman,
        [
            OsString::from("verify"),
            OsString::from("-i"),
            chd.as_os_str().to_owned(),
        ],
        &work.group_log,
    )?;
    if !settings.verify_round_trip {
        return Ok(());
    }
    remove_if_exists(&work.roundtrip)?;
    let extracted = command::run_logged(
        &settings.chdman,
        [
            OsString::from("extractdvd"),
            OsString::from("-i"),
            chd.as_os_str().to_owned(),
            OsString::from("-o"),
            work.roundtrip.as_os_str().to_owned(),
        ],
        &work.group_log,
    );
    if let Err(error) = extracted {
        let _ = fs::remove_file(&work.roundtrip);
        return Err(error);
    }
    let roundtrip_hash = sha256_file(&work.roundtrip);
    let cleanup = fs::remove_file(&work.roundtrip)
        .map_err(|error| PipelineError::io(format!("remove {}", work.roundtrip.display()), error));
    let roundtrip_hash = roundtrip_hash?;
    cleanup?;
    if roundtrip_hash != source_hash {
        return Err(PipelineError::Message(format!(
            "PSP CHD round-trip hash mismatch: {}",
            chd.display()
        )));
    }
    Ok(())
}

fn validate_exact_sources(adapter: &PspAdapter, job: &Job, canonical: &Path) -> Result<String> {
    let expected = sha256_file(canonical)?;
    for artifact in job.sources.iter().skip(1) {
        let path = adapter.locate(&artifact.name)?.ok_or_else(|| {
            PipelineError::MissingPath(adapter.profile().source_dir.join(&artifact.name))
        })?;
        if sha256_file(&path)? != expected {
            return Err(PipelineError::Message(format!(
                "PSP duplicate changed after inventory: {}",
                path.display()
            )));
        }
    }
    Ok(expected)
}

fn adopt_existing_output(
    adapter: &PspAdapter,
    job: &Job,
    state: &StateStore,
    source: &Path,
    output: &Path,
    work: &JobWorkspace,
) -> Result<JobOutcome> {
    let source_hash = validate_exact_sources(adapter, job, source)?;
    validate_chd(adapter, output, &source_hash, work)?;
    let output_hash = sha256_file(output)?;
    adapter.move_sources_to_done(job, state)?;
    state.write_completion(
        job.id.as_str(),
        &completion_record(job, output, output_hash)?,
    )?;
    state.log(&format!(
        "RESUMED verified PSP output and finalized source moves: {}",
        job.output_name
    ))?;
    Ok(JobOutcome::Completed)
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

#[cfg(test)]
mod tests {
    use super::remove_if_exists;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn removing_a_missing_partial_is_safe() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rom-pipeline-psp-missing-{}-{nonce}",
            std::process::id()
        ));
        assert!(remove_if_exists(&path).is_ok());
        fs::write(&path, b"partial").expect("write partial");
        assert!(remove_if_exists(&path).is_ok());
        assert!(!path.exists());
    }
}
