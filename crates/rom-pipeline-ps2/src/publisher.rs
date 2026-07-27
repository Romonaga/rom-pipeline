use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use fs2::FileExt;
use rom_pipeline_core::{
    BatchPolicy, CompletionRecord, Job, PipelineAdapter, PipelineError, Result, StateStore,
    StopToken, sha256_file,
};

use crate::Ps2Adapter;
use crate::command;

const COPY_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LibrarySummary {
    pub completed_jobs: usize,
    pub failed_jobs: usize,
    pub files_removed: usize,
    pub bytes_reclaimed: u64,
}

enum Change {
    None,
    Applied {
        files_removed: usize,
        bytes_reclaimed: u64,
    },
}

/// Publishes verified PS2 outputs from fast staging to the final library.
///
/// # Errors
///
/// Returns an error for lock contention, invalid completion records, failed
/// integrity checks, interrupted copies, or filesystem failures.
pub fn publish_library(adapter: &Ps2Adapter, limit: BatchPolicy) -> Result<LibrarySummary> {
    run_library_action(adapter, limit, "publish", publish_one)
}

/// Removes original PS2 disc images only after the full set is complete and
/// every output exists in the final library.
///
/// # Errors
///
/// Returns an error when any job is incomplete or unpublished, when validation
/// fails, or when a filesystem operation fails.
pub fn prune_sources(adapter: &Ps2Adapter, limit: BatchPolicy) -> Result<LibrarySummary> {
    run_library_action(adapter, limit, "prune", prune_one)
}

fn run_library_action(
    adapter: &Ps2Adapter,
    limit: BatchPolicy,
    action: &str,
    operation: fn(&Ps2Adapter, &Job, &CompletionRecord, &StateStore, &StopToken) -> Result<Change>,
) -> Result<LibrarySummary> {
    adapter.preflight()?;
    let state = StateStore::new(&adapter.profile().state_dir, &adapter.profile().log_dir);
    state.prepare()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.lock_path())
        .map_err(|error| PipelineError::io("open PS2 profile lock", error))?;
    FileExt::try_lock_exclusive(&lock)
        .map_err(|error| PipelineError::io("another PS2 action holds the profile lock", error))?;
    let stop = StopToken::new(state.stop_path(), Arc::new(AtomicBool::new(false)));
    stop.clear()?;
    state.clear_current_failures()?;
    fs::write(
        adapter.profile().state_dir.join("batch.limit"),
        format!("{}\n", limit.limit()),
    )
    .map_err(|error| PipelineError::io("write PS2 library action batch limit", error))?;

    let jobs = adapter.inventory(None)?;
    if action == "prune" {
        ensure_all_jobs_are_published(adapter, &jobs, &state)?;
    }
    let mut summary = LibrarySummary::default();
    for job in &jobs {
        if stop.is_requested() || summary.completed_jobs >= limit.limit() {
            break;
        }
        let Some(record) = state.read_completion(&job.id)? else {
            continue;
        };
        if record.component_fingerprint.as_deref() != Some(&job.component_fingerprint()) {
            summary.failed_jobs += 1;
            state.record_failure(&job.id, "completion component fingerprint changed")?;
            continue;
        }
        match operation(adapter, job, &record, &state, &stop) {
            Ok(Change::None) => {}
            Ok(Change::Applied {
                files_removed,
                bytes_reclaimed,
            }) => {
                summary.completed_jobs += 1;
                summary.files_removed += files_removed;
                summary.bytes_reclaimed += bytes_reclaimed;
            }
            Err(PipelineError::Interrupted) => break,
            Err(error) => {
                summary.failed_jobs += 1;
                state.record_failure(&job.id, &format!("{action} failed: {error}"))?;
            }
        }
    }
    let stopped = stop.is_requested();
    state.write_current(&format!(
        "{action} {} completed={} failed={} files_removed={} bytes_reclaimed={}",
        if stopped {
            "stopped cleanly"
        } else {
            "batch complete"
        },
        summary.completed_jobs,
        summary.failed_jobs,
        summary.files_removed,
        summary.bytes_reclaimed
    ))?;
    state.log(&format!(
        "{} PS2 {} completed={} failed={} files_removed={} bytes_reclaimed={}",
        action.to_ascii_uppercase(),
        if stopped { "STOPPED" } else { "COMPLETE" },
        summary.completed_jobs,
        summary.failed_jobs,
        summary.files_removed,
        summary.bytes_reclaimed
    ))?;
    Ok(summary)
}

fn ensure_all_jobs_are_published(
    adapter: &Ps2Adapter,
    jobs: &[Job],
    state: &StateStore,
) -> Result<()> {
    for job in jobs {
        let record = state.read_completion(&job.id)?.ok_or_else(|| {
            PipelineError::Message(format!(
                "prune requires all PS2 jobs to be complete; missing marker for {}",
                job.id
            ))
        })?;
        if record.component_fingerprint.as_deref() != Some(&job.component_fingerprint()) {
            return Err(PipelineError::Message(format!(
                "prune requires a current completion fingerprint for {}",
                job.id
            )));
        }
        let library = library_path(adapter, &record)?;
        if !library.is_file() || file_size(&library)? != record.size {
            return Err(PipelineError::Message(format!(
                "prune requires every PS2 output in the final library; missing {}",
                library.display()
            )));
        }
    }
    Ok(())
}

fn publish_one(
    adapter: &Ps2Adapter,
    job: &Job,
    record: &CompletionRecord,
    state: &StateStore,
    stop: &StopToken,
) -> Result<Change> {
    let library = library_path(adapter, record)?;
    let staging = adapter.profile().output_dir.join(&record.output_name);
    let group_log = group_log(adapter, job);

    if library.is_file() {
        state.write_current(&format!(
            "group={} step=publish-check-existing output={}",
            job.id, record.output_name
        ))?;
        verify_recorded_output(adapter, &library, record, &group_log)?;
        set_modified_time(&library, record.modified_seconds)?;
        if staging.is_file() {
            let bytes = file_size(&staging)?;
            fs::remove_file(&staging).map_err(|error| {
                PipelineError::io(format!("remove {}", staging.display()), error)
            })?;
            state.log(&format!(
                "PUBLISH adopted verified PS2 library output and removed staging copy: {}",
                record.output_name
            ))?;
            return Ok(Change::Applied {
                files_removed: 1,
                bytes_reclaimed: bytes,
            });
        }
        return Ok(Change::None);
    }
    if !staging.is_file() {
        return Err(PipelineError::MissingPath(staging));
    }
    state.write_current(&format!(
        "group={} step=publish-verify-staging output={}",
        job.id, record.output_name
    ))?;
    verify_recorded_output(adapter, &staging, record, &group_log)?;
    let parent = library
        .parent()
        .ok_or_else(|| PipelineError::Message("PS2 library path has no parent".to_owned()))?;
    fs::create_dir_all(parent)
        .map_err(|error| PipelineError::io(format!("create {}", parent.display()), error))?;
    let partial = append_partial(&library);
    remove_if_exists(&partial)?;
    state.write_current(&format!(
        "group={} step=publish-copy output={}",
        job.id, record.output_name
    ))?;
    if let Err(error) = copy_with_stop(&staging, &partial, stop) {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    state.write_current(&format!(
        "group={} step=publish-verify output={}",
        job.id, record.output_name
    ))?;
    if let Err(error) = verify_recorded_output(adapter, &partial, record, &group_log) {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    set_modified_time(&partial, record.modified_seconds)?;
    fs::rename(&partial, &library)
        .map_err(|error| PipelineError::io(format!("publish {}", library.display()), error))?;
    let bytes = file_size(&staging)?;
    fs::remove_file(&staging)
        .map_err(|error| PipelineError::io(format!("remove {}", staging.display()), error))?;
    state.log(&format!(
        "PUBLISH verified PS2 library output and removed staging copy: {}",
        record.output_name
    ))?;
    Ok(Change::Applied {
        files_removed: 1,
        bytes_reclaimed: bytes,
    })
}

fn prune_one(
    adapter: &Ps2Adapter,
    job: &Job,
    record: &CompletionRecord,
    state: &StateStore,
    _stop: &StopToken,
) -> Result<Change> {
    let library = library_path(adapter, record)?;
    let group_log = group_log(adapter, job);
    state.write_current(&format!(
        "group={} step=prune-verify-library output={}",
        job.id, record.output_name
    ))?;
    verify_recorded_output(adapter, &library, record, &group_log)?;

    let mut files = 0;
    let mut bytes = 0_u64;
    for artifact in &job.sources {
        let source = adapter.profile().source_dir.join(&artifact.name);
        let done = adapter.profile().done_dir.join(&artifact.name);
        for candidate in [&source, &done] {
            if candidate == &library {
                continue;
            }
            if candidate.is_file() {
                bytes += file_size(candidate)?;
                fs::remove_file(candidate).map_err(|error| {
                    PipelineError::io(format!("remove {}", candidate.display()), error)
                })?;
                files += 1;
                state.log(&format!("PRUNED verified PS2 source: {}", artifact.name))?;
            }
        }
    }
    if files == 0 {
        Ok(Change::None)
    } else {
        Ok(Change::Applied {
            files_removed: files,
            bytes_reclaimed: bytes,
        })
    }
}

fn verify_recorded_output(
    adapter: &Ps2Adapter,
    path: &Path,
    record: &CompletionRecord,
    log: &Path,
) -> Result<()> {
    if file_size(path)? != record.size {
        return Err(PipelineError::Message(format!(
            "recorded PS2 output size mismatch: {}",
            path.display()
        )));
    }
    if sha256_file(path)? != record.sha256 {
        return Err(PipelineError::Message(format!(
            "recorded PS2 output hash mismatch: {}",
            path.display()
        )));
    }
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("chd"))
    {
        command::run_logged(
            &adapter.settings()?.chdman,
            ["verify".into(), "-i".into(), path.as_os_str().to_owned()],
            log,
        )?;
    }
    Ok(())
}

fn library_path(adapter: &Ps2Adapter, record: &CompletionRecord) -> Result<PathBuf> {
    adapter
        .profile()
        .library_dir
        .as_ref()
        .map(|root| root.join(&record.output_name))
        .ok_or_else(|| PipelineError::InvalidConfig("PS2 library_dir is required".to_owned()))
}

fn group_log(adapter: &Ps2Adapter, job: &Job) -> PathBuf {
    adapter
        .profile()
        .log_dir
        .join("groups")
        .join(format!("{}.log", job.id))
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

fn set_modified_time(path: &Path, seconds: u64) -> Result<()> {
    let status = std::process::Command::new("touch")
        .args(["-m", "-d"])
        .arg(format!("@{seconds}"))
        .arg("--")
        .arg(path)
        .status()
        .map_err(|error| PipelineError::io(format!("touch {}", path.display()), error))?;
    if status.success() {
        Ok(())
    } else {
        Err(PipelineError::CommandFailed {
            command: format!("touch {}", path.display()),
            status: status.to_string(),
        })
    }
}

fn append_partial(path: &Path) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(".partial");
    PathBuf::from(value)
}

fn file_size(path: &Path) -> Result<u64> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))
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
