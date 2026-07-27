use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use rom_pipeline_core::{
    CompletionRecord, PipelineAdapter, PipelineError, ProfileConfig, Result, StateStore,
    SystemKind, completion_output_valid,
};
use rom_pipeline_nintendo_3ds::Nintendo3dsAdapter;
use rom_pipeline_psp::PspAdapter;
use rom_pipeline_wiiu::WiiUAdapter;
use serde::Serialize;

use crate::controller::service_state;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublicationProgress {
    pub published: usize,
    pub ready: usize,
    pub remaining: usize,
    pub total: usize,
    pub partial_files: usize,
    pub phase: String,
    pub current_game: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PruneProgress {
    pub removed: usize,
    pub remaining: usize,
    pub total: usize,
    pub phase: String,
    pub current_game: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileStatus {
    pub profile: String,
    pub service: String,
    pub activity: String,
    pub current: String,
    pub batch_limit: usize,
    pub completed_groups: usize,
    pub total_groups: usize,
    pub output_files: usize,
    pub source_archives_moved_to_done: usize,
    pub current_run_failures: usize,
    pub active_worker: Option<String>,
    pub publication: Option<PublicationProgress>,
    pub prune: Option<PruneProgress>,
    pub log: String,
}

/// Builds a live status snapshot for a configured profile.
///
/// # Errors
///
/// Returns an error when state, inventory, or service information cannot be
/// read.
pub fn profile_status(profile: &ProfileConfig) -> Result<ProfileStatus> {
    let service = service_state(&profile.id)?;
    let state = StateStore::new(&profile.state_dir, &profile.log_dir);
    let current = state
        .read_current()?
        .unwrap_or_else(|| "not started".to_owned());
    let activity = if service == "active" && current.starts_with("group=") {
        "processing"
    } else if service == "active" && current.starts_with("waiting") {
        "waiting"
    } else if service == "active" {
        "running"
    } else {
        "stopped"
    }
    .to_owned();
    let (total_groups, completed_groups) = match profile.system {
        SystemKind::WiiU => {
            let adapter = WiiUAdapter::new(profile.clone())?;
            completion_counts(&adapter, profile, &state)?
        }
        SystemKind::Nintendo3ds => {
            let adapter = Nintendo3dsAdapter::new(profile.clone())?;
            completion_counts(&adapter, profile, &state)?
        }
        SystemKind::PlayStationPortable => {
            let adapter = PspAdapter::new(profile.clone())?;
            completion_counts(&adapter, profile, &state)?
        }
        SystemKind::PlayStation2 => (0, 0),
    };
    let batch_limit =
        read_number(profile.state_dir.join("batch.limit")).unwrap_or(profile.batch_limit);
    let mut output_files = count_extension(&profile.output_dir, &profile.output_format)?;
    if let Some(library) = &profile.library_dir {
        if library != &profile.output_dir {
            output_files += count_extension(library, &profile.output_format)?;
        }
    }
    let publication =
        publication_progress(profile, &state, completed_groups, total_groups, &current)?;
    let prune = prune_progress(profile, &state, &current)?;
    Ok(ProfileStatus {
        profile: profile.id.clone(),
        service,
        activity,
        current,
        batch_limit,
        completed_groups,
        total_groups,
        output_files,
        source_archives_moved_to_done: count_regular_files(&profile.done_dir)?,
        current_run_failures: count_lines(&state.current_failures_path())?,
        active_worker: active_worker(&profile.id),
        publication,
        prune,
        log: state.pipeline_log_path().display().to_string(),
    })
}

fn publication_progress(
    profile: &ProfileConfig,
    state: &StateStore,
    ready: usize,
    total: usize,
    current: &str,
) -> Result<Option<PublicationProgress>> {
    if !matches!(profile.system, SystemKind::PlayStationPortable) {
        return Ok(None);
    }
    let Some(library) = profile.library_dir.as_deref() else {
        return Ok(None);
    };
    let records = state.completion_records()?;
    let published = count_published_outputs(library, records.values())?;
    let remaining = total.saturating_sub(published);
    let partial_files = count_suffix(library, ".chd.partial")?;
    let (phase, current_game) = publication_phase(current, published, total);
    Ok(Some(PublicationProgress {
        published,
        ready,
        remaining,
        total,
        partial_files,
        phase,
        current_game,
    }))
}

fn count_published_outputs<'a>(
    library: &Path,
    records: impl Iterator<Item = &'a CompletionRecord>,
) -> Result<usize> {
    let mut outputs = BTreeSet::new();
    for record in records {
        let path = library.join(&record.output_name);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() && metadata.len() == record.size => {
                outputs.insert(record.output_name.clone());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(PipelineError::io(format!("stat {}", path.display()), error));
            }
        }
    }
    Ok(outputs.len())
}

fn publication_phase(current: &str, published: usize, total: usize) -> (String, Option<String>) {
    let game = current
        .split_once(" output=")
        .filter(|_| current.contains("step=publish-"))
        .map(|(_, output)| output.to_owned());
    let phase = if current.contains("step=publish-copy") {
        "Copying to final library"
    } else if current.contains("step=publish-verify-staging") {
        "Verifying staged CHD"
    } else if current.contains("step=publish-verify") {
        "Verifying final copy"
    } else if current.contains("step=publish-check-existing") {
        "Checking existing publication"
    } else if current.starts_with("publish stopped cleanly") {
        "Publishing stopped cleanly"
    } else if current.starts_with("publish batch complete") {
        if published == total && total > 0 {
            "Publishing complete"
        } else {
            "Publish batch complete"
        }
    } else if published == total && total > 0 {
        "Publishing complete"
    } else {
        "Ready to publish"
    };
    (phase.to_owned(), game)
}

fn prune_progress(
    profile: &ProfileConfig,
    state: &StateStore,
    current: &str,
) -> Result<Option<PruneProgress>> {
    if !matches!(profile.system, SystemKind::PlayStationPortable) || profile.library_dir.is_none() {
        return Ok(None);
    }
    let log = match fs::read_to_string(state.pipeline_log_path()) {
        Ok(log) => log,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(PipelineError::io("read pipeline log", error)),
    };
    let removed = pruned_source_names(&log).len();
    let mut remaining = count_extension(&profile.source_dir, &profile.source_format)?;
    if profile.done_dir != profile.source_dir {
        remaining += count_extension(&profile.done_dir, &profile.source_format)?;
    }
    let total = removed.saturating_add(remaining);
    let (phase, current_game) = prune_phase(current, removed, remaining);
    Ok(Some(PruneProgress {
        removed,
        remaining,
        total,
        phase,
        current_game,
    }))
}

fn pruned_source_names(log: &str) -> BTreeSet<&str> {
    const EVENT: &str = " PRUNED verified PSP source: ";
    log.lines()
        .filter_map(|line| line.split_once(EVENT).map(|(_, name)| name))
        .collect()
}

fn prune_phase(current: &str, removed: usize, remaining: usize) -> (String, Option<String>) {
    let game = current
        .split_once(" output=")
        .filter(|_| current.contains("step=prune-"))
        .map(|(_, output)| output.to_owned());
    let phase = if current.contains("step=prune-verify-library") {
        "Verifying before source deletion"
    } else if current.starts_with("prune stopped cleanly") {
        "Pruning stopped cleanly"
    } else if current.starts_with("prune batch complete") {
        if remaining == 0 && removed > 0 {
            "Pruning complete"
        } else {
            "Prune batch complete"
        }
    } else if remaining == 0 && removed > 0 {
        "Pruning complete"
    } else if removed > 0 {
        "Ready to continue pruning"
    } else {
        "Not started"
    };
    (phase.to_owned(), game)
}

fn completion_counts(
    adapter: &impl PipelineAdapter,
    profile: &ProfileConfig,
    state: &StateStore,
) -> Result<(usize, usize)> {
    let jobs = adapter.inventory(None)?;
    let mut ids = BTreeSet::new();
    let mut completed = 0;
    for job in &jobs {
        ids.insert(job.id.clone());
        if adapter.is_complete(job, state, false)? {
            completed += 1;
        }
    }
    for (id, record) in state.completion_records()? {
        if ids.insert(id) && completion_output_valid(profile, &record, false)? {
            completed += 1;
        }
    }
    Ok((ids.len(), completed))
}

fn count_regular_files(path: &Path) -> Result<usize> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(PipelineError::io(format!("read {}", path.display()), error));
        }
    };
    Ok(entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_file()))
        .count())
}

fn count_extension(path: &Path, extension: &str) -> Result<usize> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(PipelineError::io(format!("read {}", path.display()), error));
        }
    };
    Ok(entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        })
        .count())
}

fn count_suffix(path: &Path, suffix: &str) -> Result<usize> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(PipelineError::io(format!("read {}", path.display()), error));
        }
    };
    Ok(entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry
                    .file_name()
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .ends_with(suffix)
        })
        .count())
}

fn count_lines(path: &Path) -> Result<usize> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text.lines().count()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(PipelineError::io(format!("read {}", path.display()), error)),
    }
}

fn read_number(path: impl AsRef<Path>) -> Option<usize> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn active_worker(profile_id: &str) -> Option<String> {
    let unit = crate::controller::unit_name(profile_id);
    let output = Command::new("systemctl")
        .args(["--user", "show", &unit, "-p", "MainPID", "--value"])
        .output()
        .ok()?;
    let main_pid = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()?;
    if main_pid == 0 {
        return None;
    }
    let output = Command::new("ps")
        .args([
            "--ppid",
            &main_pid.to_string(),
            "-o",
            "etime=,cmd=",
            "--no-headers",
        ])
        .output()
        .ok()?;
    let worker = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!worker.is_empty()).then_some(worker)
}

#[cfg(test)]
mod tests {
    use super::{prune_phase, pruned_source_names, publication_phase};

    #[test]
    fn publication_phase_describes_copy_and_game() {
        let (phase, game) = publication_phase(
            "group=GAME1 step=publish-copy output=Example Game.chd",
            3,
            10,
        );
        assert_eq!(phase, "Copying to final library");
        assert_eq!(game.as_deref(), Some("Example Game.chd"));
    }

    #[test]
    fn publication_phase_reports_set_completion() {
        let (phase, game) =
            publication_phase("publish batch complete completed=10 failed=0", 10, 10);
        assert_eq!(phase, "Publishing complete");
        assert_eq!(game, None);
    }

    #[test]
    fn publication_phase_describes_staging_verification() {
        let (phase, game) = publication_phase(
            "group=GAME2 step=publish-verify-staging output=Next Game.chd",
            4,
            10,
        );
        assert_eq!(phase, "Verifying staged CHD");
        assert_eq!(game.as_deref(), Some("Next Game.chd"));
    }

    #[test]
    fn prune_events_count_unique_source_files() {
        let log = "1 PRUNED verified PSP source: First.iso\n\
                   2 PRUNED verified PSP source: Second.iso\n\
                   3 PRUNED verified PSP source: First.iso\n";
        let names = pruned_source_names(log);
        assert_eq!(names.len(), 2);
        assert!(names.contains("First.iso"));
        assert!(names.contains("Second.iso"));
    }

    #[test]
    fn prune_phase_describes_verification_and_game() {
        let (phase, game) = prune_phase(
            "group=GAME3 step=prune-verify-library output=Safe Game.chd",
            4,
            121,
        );
        assert_eq!(phase, "Verifying before source deletion");
        assert_eq!(game.as_deref(), Some("Safe Game.chd"));
    }
}
