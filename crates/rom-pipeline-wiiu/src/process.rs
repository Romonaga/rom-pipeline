use std::fs::{self, File};
use std::path::PathBuf;

use rom_pipeline_core::{
    Job, JobOutcome, PipelineError, Result, StateStore, StopToken, sha256_file,
};

use crate::adapter::{WiiUAdapter, completion_record};
use crate::archive::extract_and_decrypt;
use crate::filesystem::clear_owned_directory;
use crate::packaging::{pack_wua, sidecar_path, validate_and_finalize};

pub(crate) struct JobWorkspace {
    pub groups_root: PathBuf,
    pub group_work: PathBuf,
    pub pack_root: PathBuf,
    pub validation: PathBuf,
    pub group_log: PathBuf,
}

impl JobWorkspace {
    fn prepare(adapter: &WiiUAdapter, job: &Job) -> Result<Self> {
        let profile = adapter.profile();
        let groups_root = profile.work_dir.join("groups");
        let group_work = groups_root.join(&job.id);
        let pack_root = group_work.join("pack");
        let validation = group_work.join("validation");
        let group_log = profile
            .log_dir
            .join("groups")
            .join(format!("{}.log", job.id));
        for path in [
            groups_root.as_path(),
            profile.log_dir.join("groups").as_path(),
            profile.output_dir.as_path(),
        ] {
            fs::create_dir_all(path)
                .map_err(|error| PipelineError::io(format!("create {}", path.display()), error))?;
        }
        clear_owned_directory(&group_work, &groups_root)?;
        fs::create_dir_all(&pack_root)
            .map_err(|error| PipelineError::io(format!("create {}", pack_root.display()), error))?;
        File::create(&group_log)
            .map_err(|error| PipelineError::io(format!("create {}", group_log.display()), error))?;
        Ok(Self {
            groups_root,
            group_work,
            pack_root,
            validation,
            group_log,
        })
    }
}

pub fn process_job(
    adapter: &WiiUAdapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
) -> Result<JobOutcome> {
    if adopt_validated_output(adapter, job, state)? {
        return Ok(JobOutcome::Completed);
    }
    let output = adapter.output_path(job);
    if output.exists() {
        return Err(PipelineError::Message(format!(
            "unverified output already exists: {}",
            output.display()
        )));
    }

    let work = JobWorkspace::prepare(adapter, job)?;
    state.write_current(&format!(
        "group={} step=extract-decrypt output={}",
        job.id, job.output_name
    ))?;
    state.log(&format!(
        "BEGIN group={} output={}",
        job.id, job.output_name
    ))?;
    extract_and_decrypt(adapter, job, state, stop, &work)?;
    let partial = pack_wua(adapter, job, state, stop, &work, &output)?;
    validate_and_finalize(adapter, job, state, stop, &work, &partial, &output)?;
    Ok(JobOutcome::Completed)
}

fn adopt_validated_output(adapter: &WiiUAdapter, job: &Job, state: &StateStore) -> Result<bool> {
    let output = adapter.output_path(job);
    let sidecar = sidecar_path(&output)?;
    if !output.is_file() || !sidecar.is_file() {
        return Ok(false);
    }
    let sidecar_text = fs::read_to_string(&sidecar)
        .map_err(|error| PipelineError::io(format!("read {}", sidecar.display()), error))?;
    let expected = sidecar_text
        .split_whitespace()
        .next()
        .ok_or_else(|| PipelineError::Message("empty SHA-256 sidecar".to_owned()))?;
    let actual = sha256_file(&output)?;
    if actual != expected {
        return Ok(false);
    }
    adapter.move_sources_to_done(job, state)?;
    state.write_completion(&job.id, &completion_record(job, &output, actual)?)?;
    state.log(&format!(
        "RESUMED verified output and finalized source moves: {}",
        job.output_name
    ))?;
    Ok(true)
}
