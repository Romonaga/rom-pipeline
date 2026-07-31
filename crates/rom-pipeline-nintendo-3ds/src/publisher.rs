use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use fs2::FileExt;
use rom_pipeline_core::{
    BatchPolicy, CompletionRecord, Job, PipelineAdapter, PipelineError, Result, StateStore,
    StopToken, sha256_file,
};

use crate::Nintendo3dsAdapter;
use crate::process::{copy_with_stop, validate_cia_file};

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

/// Publishes verified 3DS CIAs from `FastDrive` staging to the final library.
///
/// # Errors
///
/// Returns an error for lock contention, invalid state, failed validation, or
/// filesystem failures.
pub fn publish_library(adapter: &Nintendo3dsAdapter, limit: BatchPolicy) -> Result<LibrarySummary> {
    run_action(adapter, limit, "publish", publish_one)
}

/// Permanently removes source ZIPs only after every manifest job is complete
/// and published.
///
/// # Errors
///
/// Returns an error if the set is incomplete, validation fails, or a
/// filesystem operation fails.
pub fn prune_sources(adapter: &Nintendo3dsAdapter, limit: BatchPolicy) -> Result<LibrarySummary> {
    run_action(adapter, limit, "prune", prune_one)
}

type Operation =
    fn(&Nintendo3dsAdapter, &Job, &CompletionRecord, &StateStore, &StopToken) -> Result<Change>;

fn run_action(
    adapter: &Nintendo3dsAdapter,
    limit: BatchPolicy,
    action: &str,
    operation: Operation,
) -> Result<LibrarySummary> {
    adapter.preflight()?;
    let state = StateStore::new(&adapter.profile().state_dir, &adapter.profile().log_dir);
    state.prepare()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.lock_path())
        .map_err(|error| PipelineError::io("open 3DS profile lock", error))?;
    FileExt::try_lock_exclusive(&lock)
        .map_err(|error| PipelineError::io("another 3DS action holds the profile lock", error))?;
    let stop = StopToken::new(state.stop_path(), Arc::new(AtomicBool::new(false)));
    stop.clear()?;
    state.clear_current_failures()?;
    fs::write(
        adapter.profile().state_dir.join("batch.limit"),
        format!("{}\n", limit.limit()),
    )
    .map_err(|error| PipelineError::io("write 3DS library batch limit", error))?;
    let jobs = adapter.inventory(None)?;
    if action == "prune" {
        ensure_all_published(adapter, &jobs, &state)?;
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
    state.write_current(&format!(
        "{action} batch complete completed={} failed={} files_removed={} bytes_reclaimed={}",
        summary.completed_jobs, summary.failed_jobs, summary.files_removed, summary.bytes_reclaimed
    ))?;
    Ok(summary)
}

fn ensure_all_published(
    adapter: &Nintendo3dsAdapter,
    jobs: &[Job],
    state: &StateStore,
) -> Result<()> {
    for job in jobs {
        let record = state.read_completion(&job.id)?.ok_or_else(|| {
            PipelineError::Message(format!(
                "prune requires all 3DS jobs complete; missing {}",
                job.id
            ))
        })?;
        let library = library_path(adapter, &record)?;
        if record.component_fingerprint.as_deref() != Some(&job.component_fingerprint())
            || !library.is_file()
            || file_size(&library)? != record.size
        {
            return Err(PipelineError::Message(format!(
                "prune requires every verified 3DS CIA in the final library; missing {}",
                library.display()
            )));
        }
    }
    Ok(())
}

fn publish_one(
    adapter: &Nintendo3dsAdapter,
    job: &Job,
    record: &CompletionRecord,
    state: &StateStore,
    stop: &StopToken,
) -> Result<Change> {
    let staging = adapter.profile().output_dir.join(&record.output_name);
    let library = library_path(adapter, record)?;
    let log = group_log(adapter, job);
    if library.is_file() {
        verify_record(adapter, &library, record, &log)?;
        if staging.is_file() {
            let bytes = file_size(&staging)?;
            fs::remove_file(&staging).map_err(|error| {
                PipelineError::io(format!("remove {}", staging.display()), error)
            })?;
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
    verify_record(adapter, &staging, record, &log)?;
    let partial = append_partial(&library);
    remove_if_exists(&partial)?;
    state.write_current(&format!(
        "group={} step=publish-copy output={}",
        job.id, record.output_name
    ))?;
    copy_with_stop(&staging, &partial, stop)?;
    state.write_current(&format!(
        "group={} step=publish-verify output={}",
        job.id, record.output_name
    ))?;
    verify_record(adapter, &partial, record, &log)?;
    fs::rename(&partial, &library)
        .map_err(|error| PipelineError::io(format!("publish {}", library.display()), error))?;
    let bytes = file_size(&staging)?;
    fs::remove_file(&staging)
        .map_err(|error| PipelineError::io(format!("remove {}", staging.display()), error))?;
    state.log(&format!(
        "PUBLISH verified 3DS CIA and removed staging copy: {}",
        record.output_name
    ))?;
    Ok(Change::Applied {
        files_removed: 1,
        bytes_reclaimed: bytes,
    })
}

fn prune_one(
    adapter: &Nintendo3dsAdapter,
    job: &Job,
    record: &CompletionRecord,
    state: &StateStore,
    _stop: &StopToken,
) -> Result<Change> {
    let library = library_path(adapter, record)?;
    state.write_current(&format!(
        "group={} step=prune-verify-library output={}",
        job.id, record.output_name
    ))?;
    verify_record(adapter, &library, record, &group_log(adapter, job))?;
    let mut files_removed = 0;
    let mut bytes_reclaimed = 0;
    for artifact in &job.sources {
        for path in [
            adapter.profile().source_dir.join(&artifact.name),
            adapter.profile().done_dir.join(&artifact.name),
        ] {
            if path.is_file() {
                bytes_reclaimed += file_size(&path)?;
                fs::remove_file(&path).map_err(|error| {
                    PipelineError::io(format!("remove {}", path.display()), error)
                })?;
                files_removed += 1;
                state.log(&format!(
                    "PRUNED verified 3DS ZIP source: {}",
                    artifact.name
                ))?;
            }
        }
    }
    if files_removed == 0 {
        Ok(Change::None)
    } else {
        Ok(Change::Applied {
            files_removed,
            bytes_reclaimed,
        })
    }
}

fn verify_record(
    adapter: &Nintendo3dsAdapter,
    path: &Path,
    record: &CompletionRecord,
    log: &Path,
) -> Result<()> {
    if file_size(path)? != record.size || sha256_file(path)? != record.sha256 {
        return Err(PipelineError::Message(format!(
            "recorded 3DS CIA mismatch: {}",
            path.display()
        )));
    }
    let _ = validate_cia_file(adapter, path, log)?;
    Ok(())
}

fn library_path(adapter: &Nintendo3dsAdapter, record: &CompletionRecord) -> Result<PathBuf> {
    adapter
        .profile()
        .library_dir
        .as_ref()
        .map(|root| root.join(&record.output_name))
        .ok_or_else(|| PipelineError::InvalidConfig("3DS library_dir is required".to_owned()))
}

fn group_log(adapter: &Nintendo3dsAdapter, job: &Job) -> PathBuf {
    adapter
        .profile()
        .log_dir
        .join("groups")
        .join(format!("{}.log", job.id))
}

fn append_partial(path: &Path) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
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
