use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use rom_pipeline_core::{
    Job, JobOutcome, PipelineError, Result, StateStore, StopToken, sha256_file,
};

use crate::adapter::{GameCubeAdapter, completion_record};
use crate::command;

struct JobPaths {
    source: PathBuf,
    work: PathBuf,
    output: PathBuf,
    partial: PathBuf,
    group_log: PathBuf,
}

pub fn process_job(
    adapter: &GameCubeAdapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
) -> Result<JobOutcome> {
    let artifact = job
        .sources
        .first()
        .ok_or_else(|| PipelineError::Message("GameCube job has no source".to_owned()))?;
    let source = adapter.locate(&artifact.name)?.ok_or_else(|| {
        PipelineError::MissingPath(adapter.profile().source_dir.join(&artifact.name))
    })?;
    let source_size = fs::metadata(&source)
        .map_err(|error| PipelineError::io(format!("stat {}", source.display()), error))?
        .len();
    if source_size != artifact.expected_size {
        return Err(PipelineError::Message(format!(
            "GameCube source size mismatch for {}: expected={} actual={source_size}",
            artifact.name, artifact.expected_size
        )));
    }

    prepare_directories(adapter)?;
    let output = adapter.profile().output_dir.join(&job.output_name);
    let work_root = adapter.profile().work_dir.join("groups");
    let paths = JobPaths {
        source,
        work: work_root.join(&job.id),
        partial: output.with_extension("rvz.partial"),
        output,
        group_log: adapter
            .profile()
            .log_dir
            .join("groups")
            .join(format!("{}.log", job.id)),
    };
    reset_work(&paths.work, &work_root)?;
    fs::write(
        &paths.group_log,
        format!("source={}\n", paths.source.display()),
    )
    .map_err(|error| PipelineError::io(format!("write {}", paths.group_log.display()), error))?;

    execute_conversion(
        adapter,
        job,
        state,
        stop,
        &artifact.name,
        source_size,
        &paths,
    )
}

fn execute_conversion(
    adapter: &GameCubeAdapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
    source_name: &str,
    source_size: u64,
    paths: &JobPaths,
) -> Result<JobOutcome> {
    state.write_current(&format!(
        "group={} step=verify-source output={}",
        job.id, job.output_name
    ))?;
    state.log(&format!(
        "BEGIN GameCube group={} output={}",
        job.id, job.output_name
    ))?;
    verify_image(adapter, &paths.source, &paths.group_log)?;
    state.log(&format!("VALIDATED GameCube source name={source_name}"))?;
    check_stop(stop)?;

    if paths.output.is_file() {
        state.write_current(&format!(
            "group={} step=resume-verify-rvz output={}",
            job.id, job.output_name
        ))?;
        verify_rvz(adapter, &paths.output, &paths.group_log)?;
        if adapter.settings()?.verify_round_trip {
            state.write_current(&format!(
                "group={} step=resume-roundtrip-rvz output={}",
                job.id, job.output_name
            ))?;
            verify_round_trip(
                adapter,
                &paths.source,
                &paths.output,
                &paths.work,
                &paths.group_log,
            )?;
        }
        state.log(&format!(
            "RESUMED verified GameCube RVZ output={}",
            job.output_name
        ))?;
        return complete_output(adapter, job, state, paths);
    }

    remove_if_exists(&paths.partial)?;
    state.write_current(&format!(
        "group={} step=create-rvz output={}",
        job.id, job.output_name
    ))?;
    if let Err(error) = create_rvz(adapter, &paths.source, &paths.partial, &paths.group_log) {
        let _ = fs::remove_file(&paths.partial);
        return Err(error);
    }
    check_stop_or_remove(stop, &paths.partial)?;

    state.write_current(&format!(
        "group={} step=verify-rvz output={}",
        job.id, job.output_name
    ))?;
    verify_rvz(adapter, &paths.partial, &paths.group_log)?;
    if adapter.settings()?.verify_round_trip {
        state.write_current(&format!(
            "group={} step=roundtrip-rvz output={}",
            job.id, job.output_name
        ))?;
        verify_round_trip(
            adapter,
            &paths.source,
            &paths.partial,
            &paths.work,
            &paths.group_log,
        )?;
    }
    check_stop_or_remove(stop, &paths.partial)?;

    let output_size = fs::metadata(&paths.partial)
        .map_err(|error| PipelineError::io(format!("stat {}", paths.partial.display()), error))?
        .len();
    fs::rename(&paths.partial, &paths.output)
        .map_err(|error| PipelineError::io(format!("publish {}", paths.output.display()), error))?;
    state.log(&format!(
        "COMPRESSED GameCube savings={} source_bytes={} output_bytes={} output={}",
        savings_percent(source_size, output_size),
        source_size,
        output_size,
        job.output_name
    ))?;
    complete_output(adapter, job, state, paths)
}

fn prepare_directories(adapter: &GameCubeAdapter) -> Result<()> {
    for path in [
        adapter.profile().output_dir.as_path(),
        adapter.profile().work_dir.join("groups").as_path(),
        adapter.profile().log_dir.join("groups").as_path(),
    ] {
        fs::create_dir_all(path)
            .map_err(|error| PipelineError::io(format!("create {}", path.display()), error))?;
    }
    Ok(())
}

fn complete_output(
    adapter: &GameCubeAdapter,
    job: &Job,
    state: &StateStore,
    paths: &JobPaths,
) -> Result<JobOutcome> {
    let hash = sha256_file(&paths.output)?;
    adapter.move_sources_to_done(job, state)?;
    state.write_completion(
        &job.id,
        &completion_record(job, &paths.output, hash.clone())?,
    )?;
    state.log(&format!(
        "COMPLETE GameCube group={} sha256={} output={}",
        job.id, hash, job.output_name
    ))?;
    let _ = fs::remove_dir_all(&paths.work);
    Ok(JobOutcome::Completed)
}

fn create_rvz(adapter: &GameCubeAdapter, source: &Path, output: &Path, log: &Path) -> Result<()> {
    let settings = adapter.settings()?;
    command::run_logged(
        &settings.dolphin_tool,
        [
            "convert".into(),
            "-i".into(),
            source.as_os_str().to_owned(),
            "-o".into(),
            output.as_os_str().to_owned(),
            "-f".into(),
            "rvz".into(),
            "-b".into(),
            settings.block_size.to_string().into(),
            "-c".into(),
            OsString::from(&settings.compression),
            "-l".into(),
            settings.compression_level.to_string().into(),
        ],
        log,
    )
}

pub(crate) fn verify_rvz(adapter: &GameCubeAdapter, image: &Path, log: &Path) -> Result<()> {
    verify_image(adapter, image, log)
}

fn verify_image(adapter: &GameCubeAdapter, image: &Path, log: &Path) -> Result<()> {
    command::run_logged(
        &adapter.settings()?.dolphin_tool,
        ["verify".into(), "-i".into(), image.as_os_str().to_owned()],
        log,
    )
}

fn verify_round_trip(
    adapter: &GameCubeAdapter,
    source: &Path,
    rvz: &Path,
    work: &Path,
    log: &Path,
) -> Result<()> {
    let roundtrip = work.join("roundtrip.iso");
    remove_if_exists(&roundtrip)?;
    command::run_logged(
        &adapter.settings()?.dolphin_tool,
        [
            "convert".into(),
            "-i".into(),
            rvz.as_os_str().to_owned(),
            "-o".into(),
            roundtrip.as_os_str().to_owned(),
            "-f".into(),
            "iso".into(),
        ],
        log,
    )?;
    if sha256_file(source)? != sha256_file(&roundtrip)? {
        return Err(PipelineError::Message(format!(
            "GameCube RVZ round-trip hash mismatch: {}",
            source.display()
        )));
    }
    Ok(())
}

fn savings_percent(source: u64, output: u64) -> u64 {
    if source == 0 || output >= source {
        0
    } else {
        (source - output).saturating_mul(100) / source
    }
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
