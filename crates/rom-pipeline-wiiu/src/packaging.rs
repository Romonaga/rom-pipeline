use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use rom_pipeline_core::{Job, PipelineError, Result, StateStore, StopToken, sha256_file};

use crate::adapter::{WiiUAdapter, completion_record};
use crate::command::{check_stop, run_logged};
use crate::filesystem::clear_owned_directory;
use crate::process::JobWorkspace;

pub fn pack_wua(
    adapter: &WiiUAdapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
    work: &JobWorkspace,
    output: &Path,
) -> Result<PathBuf> {
    check_stop(stop)?;
    state.write_current(&format!(
        "group={} step=pack output={}",
        job.id, job.output_name
    ))?;
    state.log(&format!("PACK WUA: {}", job.output_name))?;
    let partial = partial_path(output)?;
    remove_if_exists(&partial)?;
    let zarchive = zarchive(adapter)?;
    run_logged(
        zarchive,
        [work.pack_root.as_os_str(), partial.as_os_str()],
        &work.group_log,
    )?;
    if fs::metadata(&partial)
        .map_err(|error| PipelineError::io(format!("stat {}", partial.display()), error))?
        .len()
        == 0
    {
        return Err(PipelineError::Message(format!(
            "packed WUA is empty: {}",
            partial.display()
        )));
    }
    Ok(partial)
}

pub fn validate_and_finalize(
    adapter: &WiiUAdapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
    work: &JobWorkspace,
    partial: &Path,
    output: &Path,
) -> Result<()> {
    check_stop(stop)?;
    state.write_current(&format!(
        "group={} step=validate output={}",
        job.id, job.output_name
    ))?;
    state.log(&format!(
        "VALIDATE WUA by full extraction and comparison: {}",
        job.output_name
    ))?;
    fs::create_dir_all(&work.validation).map_err(|error| {
        PipelineError::io(format!("create {}", work.validation.display()), error)
    })?;
    run_logged(
        zarchive(adapter)?,
        [partial.as_os_str(), work.validation.as_os_str()],
        &work.group_log,
    )?;
    run_logged(
        "diff",
        [
            OsStr::new("-qr"),
            OsStr::new("--no-dereference"),
            work.pack_root.as_os_str(),
            work.validation.as_os_str(),
        ],
        &work.group_log,
    )?;
    publish(adapter, job, state, work, partial, output)
}

fn publish(
    adapter: &WiiUAdapter,
    job: &Job,
    state: &StateStore,
    work: &JobWorkspace,
    partial: &Path,
    output: &Path,
) -> Result<()> {
    fs::rename(partial, output).map_err(|error| {
        PipelineError::io(
            format!("publish {} as {}", partial.display(), output.display()),
            error,
        )
    })?;
    let hash = sha256_file(output)?;
    write_sidecar(output, &hash)?;
    adapter.move_sources_to_done(job, state)?;
    state.write_completion(&job.id, &completion_record(job, output, hash.clone())?)?;
    state.log(&format!(
        "COMPLETE group={} sha256={} output={}",
        job.id, hash, job.output_name
    ))?;
    clear_owned_directory(&work.group_work, &work.groups_root)
}

fn zarchive(adapter: &WiiUAdapter) -> Result<&Path> {
    adapter
        .profile()
        .wiiu
        .as_ref()
        .map(|settings| settings.zarchive.as_path())
        .ok_or_else(|| PipelineError::InvalidConfig("missing Wii U settings".to_owned()))
}

fn partial_path(output: &Path) -> Result<PathBuf> {
    let name = output
        .file_name()
        .ok_or_else(|| PipelineError::Message("output has no filename".to_owned()))?
        .to_string_lossy();
    Ok(output.with_file_name(format!(".{name}.partial")))
}

pub fn sidecar_path(output: &Path) -> Result<PathBuf> {
    let name = output
        .file_name()
        .ok_or_else(|| PipelineError::Message("output has no filename".to_owned()))?
        .to_string_lossy();
    Ok(output.with_file_name(format!("{name}.sha256")))
}

fn write_sidecar(output: &Path, hash: &str) -> Result<()> {
    let sidecar = sidecar_path(output)?;
    let temporary = sidecar.with_extension("sha256.new");
    let output_name = output
        .file_name()
        .ok_or_else(|| PipelineError::Message("output has no filename".to_owned()))?
        .to_string_lossy();
    let mut file = File::create(&temporary)
        .map_err(|error| PipelineError::io(format!("create {}", temporary.display()), error))?;
    writeln!(file, "{hash}  {output_name}")
        .map_err(|error| PipelineError::io(format!("write {}", temporary.display()), error))?;
    fs::rename(&temporary, &sidecar)
        .map_err(|error| PipelineError::io(format!("publish {}", sidecar.display()), error))
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
