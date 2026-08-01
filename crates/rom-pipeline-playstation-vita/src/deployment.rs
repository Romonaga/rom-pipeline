use std::fs;
use std::path::Path;

use rom_pipeline_core::{
    CompletionRecord, Job, JobOutcome, PipelineError, Result, StateStore, StopToken,
    modified_seconds, sha256_file,
};

use crate::VitaAdapter;
use crate::archive::{self, ArchiveLayout};
use crate::{command, manifest};

pub fn deploy(
    adapter: &VitaAdapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
) -> Result<JobOutcome> {
    let artifact = job
        .sources
        .first()
        .ok_or_else(|| PipelineError::Message("Vita job has no ZIP source".to_owned()))?;
    let source = adapter.profile().source_dir.join(&artifact.name);
    let metadata = fs::metadata(&source)
        .map_err(|error| PipelineError::io(format!("stat {}", source.display()), error))?;
    if !metadata.is_file() || metadata.len() != artifact.expected_size {
        return Err(PipelineError::Message(format!(
            "Vita ZIP size mismatch: {}",
            source.display()
        )));
    }
    let log = adapter
        .profile()
        .log_dir
        .join("groups")
        .join(format!("{}.log", job.id));
    state.write_current(&format!(
        "group={} step=inspect-vita-zip output={}",
        job.id, job.output_name
    ))?;
    state.log(&format!(
        "BEGIN Vita deployment group={} title_id={}",
        job.id, artifact.title_id
    ))?;
    let layout = archive::inspect(
        &adapter.settings()?.seven_zip,
        &source,
        &artifact.title_id,
        &log,
    )?;
    state.write_current(&format!(
        "group={} step=test-vita-zip output={}",
        job.id, job.output_name
    ))?;
    archive::test(&adapter.settings()?.seven_zip, &source, &log)?;
    check_stop(stop)?;
    adapter.ensure_device_space(layout.unpacked_bytes)?;

    let device = adapter.device_root()?;
    if final_roots_exist(device, &layout) {
        state.write_current(&format!(
            "group={} step=adopt-existing-vita-title output={}",
            job.id, job.output_name
        ))?;
        manifest::verify_files(device, &layout.entries, true, stop)?;
        return complete(adapter, job, state, stop, &source, &layout);
    }
    ensure_final_roots_absent(device, &layout)?;
    let staging_root = device.join(".rom-pipeline-staging");
    let staging = staging_root.join(&job.id);
    reset_staging(&staging, &staging_root)?;
    fs::create_dir_all(&staging)
        .map_err(|error| PipelineError::io(format!("create {}", staging.display()), error))?;
    state.write_current(&format!(
        "group={} step=extract-to-sd2vita output={}",
        job.id, job.output_name
    ))?;
    let args = archive::extraction_args(&layout, &source, &staging);
    if let Err(error) = command::run_logged(&adapter.settings()?.seven_zip, &args, &log) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if stop.is_requested() {
        let _ = fs::remove_dir_all(&staging);
        return Ok(JobOutcome::Interrupted);
    }
    state.write_current(&format!(
        "group={} step=verify-sd2vita-copy output={}",
        job.id, job.output_name
    ))?;
    manifest::verify_files(&staging, &layout.entries, true, stop)?;
    publish_roots(device, &staging, &layout)?;
    let _ = fs::remove_dir_all(&staging);
    complete(adapter, job, state, stop, &source, &layout)
}

pub fn installed(
    adapter: &VitaAdapter,
    job: &Job,
    state: &StateStore,
    reverify: bool,
    stop: &StopToken,
) -> Result<bool> {
    let Some(record) = state.read_completion(&job.id)? else {
        return Ok(false);
    };
    if record.component_fingerprint.as_deref() != Some(&job.component_fingerprint()) {
        return Ok(false);
    }
    let device = match adapter.device_root() {
        Ok(device) if device.is_dir() => device,
        _ => return Ok(false),
    };
    let manifest_path = manifest::path(state.root(), &job.id);
    if !manifest_path.is_file() || sha256_file(&manifest_path)? != record.sha256 {
        return Ok(false);
    }
    let entries = manifest::read(state.root(), &job.id)?;
    if !reverify {
        let app = device.join("app").join(&record.output_name);
        if !app.is_dir() {
            return Ok(false);
        }
        let has_patch = entries.iter().any(|entry| entry.path.starts_with("patch"));
        return Ok(!has_patch || device.join("patch").join(&record.output_name).is_dir());
    }
    match manifest::verify_files(device, &entries, reverify, stop) {
        Ok(()) => Ok(true),
        Err(PipelineError::MissingPath(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

fn complete(
    adapter: &VitaAdapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
    source: &Path,
    layout: &ArchiveLayout,
) -> Result<JobOutcome> {
    let device = adapter.device_root()?;
    manifest::verify_files(device, &layout.entries, true, stop)?;
    let manifest_hash = manifest::write(state.root(), &job.id, layout)?;
    state.write_completion(
        &job.id,
        &CompletionRecord {
            sha256: manifest_hash,
            size: layout.unpacked_bytes,
            modified_seconds: modified_seconds(source)?,
            output_name: layout.title_id.clone(),
            component_fingerprint: Some(job.component_fingerprint()),
        },
    )?;
    state.log(&format!(
        "COMPLETE Vita deployment group={} title_id={} bytes={} patch={}",
        job.id, layout.title_id, layout.unpacked_bytes, layout.has_patch
    ))?;
    Ok(JobOutcome::Completed)
}

fn publish_roots(device: &Path, staging: &Path, layout: &ArchiveLayout) -> Result<()> {
    for root in ["patch", "app"] {
        if root == "patch" && !layout.has_patch {
            continue;
        }
        let parent = device.join(root);
        fs::create_dir_all(&parent)
            .map_err(|error| PipelineError::io(format!("create {}", parent.display()), error))?;
        let source = staging.join(root).join(&layout.title_id);
        let destination = parent.join(&layout.title_id);
        fs::rename(&source, &destination).map_err(|error| {
            PipelineError::io(
                format!("publish Vita title {}", destination.display()),
                error,
            )
        })?;
    }
    Ok(())
}

fn final_roots_exist(device: &Path, layout: &ArchiveLayout) -> bool {
    device.join("app").join(&layout.title_id).is_dir()
        && (!layout.has_patch || device.join("patch").join(&layout.title_id).is_dir())
}

fn ensure_final_roots_absent(device: &Path, layout: &ArchiveLayout) -> Result<()> {
    for root in ["app", "patch"] {
        if root == "patch" && !layout.has_patch {
            continue;
        }
        let path = device.join(root).join(&layout.title_id);
        if path.exists() {
            return Err(PipelineError::Message(format!(
                "incomplete Vita deployment already exists; refusing to overwrite {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn reset_staging(path: &Path, root: &Path) -> Result<()> {
    if path.parent() != Some(root) {
        return Err(PipelineError::Message(format!(
            "unsafe Vita staging path: {}",
            path.display()
        )));
    }
    match fs::remove_dir_all(path) {
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
