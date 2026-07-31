use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use fs2::FileExt;
use rom_pipeline_core::{BatchPolicy, PipelineError, Result, StateStore, StopToken};

use crate::Nintendo3dsAdapter;
use crate::command;
use crate::format::inspect_cci;
use crate::process::{copy_with_stop, normalize_flags, validate_cia};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MigrationSummary {
    pub converted: usize,
    pub already_complete: usize,
    pub failed: usize,
}

/// Converts existing CCI files in the final 3DS library to CIA files while
/// preserving every CCI.
///
/// # Errors
///
/// Returns an error for lock contention, invalid configuration, or failures
/// outside an individual title conversion.
pub fn migrate_cci_library(
    adapter: &Nintendo3dsAdapter,
    limit: BatchPolicy,
) -> Result<MigrationSummary> {
    let profile = adapter.profile();
    let settings = profile
        .nintendo_3ds
        .as_ref()
        .ok_or_else(|| PipelineError::InvalidConfig("missing Nintendo 3DS settings".to_owned()))?;
    let library = profile.library_dir.as_ref().ok_or_else(|| {
        PipelineError::InvalidConfig("3DS library_dir is required for migration".to_owned())
    })?;
    if !library.is_dir() {
        return Err(PipelineError::MissingPath(library.clone()));
    }
    for tool in [&settings.python, &settings.converter, &settings.ctrtool] {
        if !tool.is_file() {
            return Err(PipelineError::MissingPath(tool.clone()));
        }
    }
    let state = StateStore::new(&profile.state_dir, &profile.log_dir);
    state.prepare()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.lock_path())
        .map_err(|error| PipelineError::io("open 3DS migration lock", error))?;
    FileExt::try_lock_exclusive(&lock)
        .map_err(|error| PipelineError::io("another 3DS action holds the profile lock", error))?;
    let stop = StopToken::new(state.stop_path(), Arc::new(AtomicBool::new(false)));
    stop.clear()?;
    state.clear_current_failures()?;

    let mut sources = cci_files(library)?;
    sources.sort();
    let mut summary = MigrationSummary::default();
    for source in sources {
        if stop.is_requested() || summary.converted >= limit.limit() {
            break;
        }
        let name = source
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                PipelineError::Message(format!("invalid CCI name: {}", source.display()))
            })?;
        let output = library.join(format!("{name}.cia"));
        let id = migration_id(name);
        let log = profile
            .log_dir
            .join("groups")
            .join(format!("migration-{id}.log"));
        let inspection = match inspect_cci(&source) {
            Ok(inspection) => inspection,
            Err(error) => {
                summary.failed += 1;
                state.record_failure(&id, &format!("CCI migration source failed: {error}"))?;
                continue;
            }
        };
        if output.is_file() {
            match validate_cia(adapter, &output, &inspection.title_id, &log) {
                Ok(()) => {
                    summary.already_complete += 1;
                    continue;
                }
                Err(error) => {
                    summary.failed += 1;
                    state.record_failure(&id, &format!("existing CIA failed: {error}"))?;
                    continue;
                }
            }
        }
        state.write_current(&format!("migration={id} step=create-cia output={name}.cia"))?;
        match migrate_one(adapter, &source, &output, &inspection.title_id, &log, &stop) {
            Ok(()) => {
                summary.converted += 1;
                state.log(&format!(
                    "MIGRATED preserved CCI and published CIA: {} -> {}.cia",
                    source.file_name().unwrap_or_default().to_string_lossy(),
                    name
                ))?;
            }
            Err(PipelineError::Interrupted) => break,
            Err(error) => {
                summary.failed += 1;
                state.record_failure(&id, &format!("CCI migration failed: {error}"))?;
            }
        }
    }
    state.write_current(&format!(
        "migration batch complete converted={} existing={} failed={}",
        summary.converted, summary.already_complete, summary.failed
    ))?;
    Ok(summary)
}

fn migrate_one(
    adapter: &Nintendo3dsAdapter,
    source: &Path,
    output: &Path,
    title_id: &str,
    log: &Path,
    stop: &StopToken,
) -> Result<()> {
    let profile = adapter.profile();
    let settings = profile.nintendo_3ds.as_ref().expect("validated settings");
    let work_root = profile.work_dir.join("migration");
    let work = work_root.join(migration_id(title_id));
    if work.exists() {
        fs::remove_dir_all(&work)
            .map_err(|error| PipelineError::io(format!("remove {}", work.display()), error))?;
    }
    let converted = work.join("converted");
    fs::create_dir_all(&converted)
        .map_err(|error| PipelineError::io(format!("create {}", converted.display()), error))?;
    let cartridge = work.join("input.cci");
    copy_with_stop(source, &cartridge, stop)?;
    let inspection = inspect_cci(&cartridge)?;
    normalize_flags(&cartridge, &inspection)?;
    command::run_logged(
        &settings.python,
        &[
            settings.converter.as_os_str().to_owned(),
            "--overwrite".into(),
            format!("--output={}", converted.display()).into(),
            cartridge.as_os_str().to_owned(),
        ],
        log,
    )?;
    let cia = converted.join("input.cia");
    validate_cia(adapter, &cia, title_id, log)?;
    let partial = output.with_extension("cia.partial");
    remove_if_exists(&partial)?;
    copy_with_stop(&cia, &partial, stop)?;
    validate_cia(adapter, &partial, title_id, log)?;
    fs::rename(&partial, output)
        .map_err(|error| PipelineError::io(format!("publish {}", output.display()), error))?;
    let _ = fs::remove_dir_all(work);
    Ok(())
}

fn cci_files(root: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(root)
        .map_err(|error| PipelineError::io(format!("read {}", root.display()), error))?;
    Ok(entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("cci") || extension.eq_ignore_ascii_case("3ds")
                })
        })
        .collect())
}

fn migration_id(value: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(value.as_bytes());
    digest[..8].iter().fold(String::new(), |mut output, byte| {
        write!(output, "{byte:02X}").expect("writing to a String cannot fail");
        output
    })
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
