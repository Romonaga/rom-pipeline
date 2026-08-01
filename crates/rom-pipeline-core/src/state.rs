use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::{PipelineError, ProfileConfig, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionRecord {
    pub sha256: String,
    pub size: u64,
    pub modified_seconds: u64,
    pub output_name: String,
    pub component_fingerprint: Option<String>,
}

impl CompletionRecord {
    fn parse(line: &str) -> Result<Self> {
        let fields: Vec<_> = line.trim_end().split('\t').collect();
        if !(4..=5).contains(&fields.len()) {
            return Err(PipelineError::Message(
                "completion marker must contain four or five tab-separated fields".to_owned(),
            ));
        }
        Ok(Self {
            sha256: fields[0].to_owned(),
            size: fields[1]
                .parse()
                .map_err(|_| PipelineError::Message("invalid marker size".to_owned()))?,
            modified_seconds: fields[2]
                .parse()
                .map_err(|_| PipelineError::Message("invalid marker timestamp".to_owned()))?,
            output_name: fields[3].to_owned(),
            component_fingerprint: fields.get(4).map(|value| (*value).to_owned()),
        })
    }

    fn encode(&self) -> String {
        self.component_fingerprint.as_ref().map_or_else(
            || {
                format!(
                    "{}\t{}\t{}\t{}\n",
                    self.sha256, self.size, self.modified_seconds, self.output_name
                )
            },
            |fingerprint| {
                format!(
                    "{}\t{}\t{}\t{}\t{}\n",
                    self.sha256, self.size, self.modified_seconds, self.output_name, fingerprint
                )
            },
        )
    }
}

#[derive(Clone, Debug)]
pub struct StateStore {
    root: PathBuf,
    log_dir: PathBuf,
}

impl StateStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, log_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            log_dir: log_dir.into(),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    #[must_use]
    pub fn completed_dir(&self) -> PathBuf {
        self.root.join("completed")
    }

    #[must_use]
    pub fn marker_path(&self, job_id: &str) -> PathBuf {
        self.completed_dir().join(format!("{job_id}.tsv"))
    }

    #[must_use]
    pub fn current_path(&self) -> PathBuf {
        self.root.join("current")
    }

    #[must_use]
    pub fn stop_path(&self) -> PathBuf {
        self.root.join("stop.requested")
    }

    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.root.join("pipeline.lock")
    }

    #[must_use]
    pub fn current_failures_path(&self) -> PathBuf {
        self.root.join("failures.current.tsv")
    }

    #[must_use]
    pub fn failure_history_path(&self) -> PathBuf {
        self.root.join("failures.history.tsv")
    }

    #[must_use]
    pub fn pipeline_log_path(&self) -> PathBuf {
        self.log_dir.join("pipeline.log")
    }

    /// Creates state and log directories.
    ///
    /// # Errors
    ///
    /// Returns an error when directories cannot be created.
    pub fn prepare(&self) -> Result<()> {
        for path in [
            self.root.as_path(),
            self.log_dir.as_path(),
            self.completed_dir().as_path(),
            self.log_dir.join("groups").as_path(),
        ] {
            fs::create_dir_all(path)
                .map_err(|error| PipelineError::io(format!("create {}", path.display()), error))?;
        }
        Ok(())
    }

    /// Atomically updates the human-readable current activity.
    ///
    /// # Errors
    ///
    /// Returns an error when the state cannot be written.
    pub fn write_current(&self, message: &str) -> Result<()> {
        write_atomic(&self.current_path(), format!("{message}\n").as_bytes())
    }

    /// Reads current activity, if available.
    ///
    /// # Errors
    ///
    /// Returns an error when an existing state file cannot be read.
    pub fn read_current(&self) -> Result<Option<String>> {
        match fs::read_to_string(self.current_path()) {
            Ok(value) => Ok(Some(value.trim_end().to_owned())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(PipelineError::io("read current activity", error)),
        }
    }

    /// Appends a timestamped pipeline event.
    ///
    /// # Errors
    ///
    /// Returns an error when the log cannot be opened or written.
    pub fn log(&self, message: &str) -> Result<()> {
        self.prepare()?;
        let line = format!("{} {message}\n", unix_timestamp());
        print!("{line}");
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.pipeline_log_path())
            .map_err(|error| PipelineError::io("open pipeline log", error))?;
        log.write_all(line.as_bytes())
            .map_err(|error| PipelineError::io("write pipeline log", error))
    }

    /// Clears failures for a new run.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be reset.
    pub fn clear_current_failures(&self) -> Result<()> {
        write_atomic(&self.current_failures_path(), b"")
    }

    /// Records a failure in current and historical logs.
    ///
    /// # Errors
    ///
    /// Returns an error when either failure log cannot be written.
    pub fn record_failure(&self, job_id: &str, message: &str) -> Result<()> {
        let line = format!("{}\t{job_id}\t{message}\n", unix_timestamp());
        for path in [self.current_failures_path(), self.failure_history_path()] {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|error| PipelineError::io(format!("open {}", path.display()), error))?;
            file.write_all(line.as_bytes())
                .map_err(|error| PipelineError::io(format!("write {}", path.display()), error))?;
        }
        self.log(&format!("FAILED group={job_id} reason={message}"))
    }

    /// Reads a completion marker.
    ///
    /// # Errors
    ///
    /// Returns an error when an existing marker is unreadable or malformed.
    pub fn read_completion(&self, job_id: &str) -> Result<Option<CompletionRecord>> {
        let path = self.marker_path(job_id);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(PipelineError::io(format!("open {}", path.display()), error));
            }
        };
        let mut lines = BufReader::new(file).lines();
        let line = lines
            .next()
            .ok_or_else(|| PipelineError::Message("empty completion marker".to_owned()))?
            .map_err(|error| PipelineError::io("read completion marker", error))?;
        CompletionRecord::parse(&line).map(Some)
    }

    /// Writes a completion marker atomically.
    ///
    /// # Errors
    ///
    /// Returns an error when the marker cannot be published.
    pub fn write_completion(&self, job_id: &str, record: &CompletionRecord) -> Result<()> {
        fs::create_dir_all(self.completed_dir())
            .map_err(|error| PipelineError::io("create completion directory", error))?;
        write_atomic(&self.marker_path(job_id), record.encode().as_bytes())
    }

    /// Counts valid marker files without validating their outputs.
    ///
    /// # Errors
    ///
    /// Returns an error when the completion directory cannot be read.
    pub fn completion_marker_count(&self) -> Result<usize> {
        let entries = match fs::read_dir(self.completed_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(PipelineError::io("read completion directory", error)),
        };
        Ok(entries
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|value| value == "tsv"))
            .count())
    }

    /// Reads every durable completion marker keyed by job ID.
    ///
    /// # Errors
    ///
    /// Returns an error when the completion directory, a marker filename, or a
    /// marker record is unreadable.
    pub fn completion_records(&self) -> Result<BTreeMap<String, CompletionRecord>> {
        let entries = match fs::read_dir(self.completed_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => return Err(PipelineError::io("read completion directory", error)),
        };
        let mut records = BTreeMap::new();
        for entry in entries {
            let entry = entry
                .map_err(|error| PipelineError::io("read completion directory entry", error))?;
            let path = entry.path();
            if !entry
                .file_type()
                .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
                .is_file()
                || path.extension().is_none_or(|extension| extension != "tsv")
            {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    PipelineError::Message(format!(
                        "completion marker filename is not UTF-8: {}",
                        path.display()
                    ))
                })?
                .to_owned();
            let record = self.read_completion(&id)?.ok_or_else(|| {
                PipelineError::Message(format!("completion marker disappeared: {}", path.display()))
            })?;
            records.insert(id, record);
        }
        Ok(records)
    }
}

#[derive(Clone, Debug)]
pub struct StopToken {
    stop_path: PathBuf,
    signal: Arc<AtomicBool>,
}

impl StopToken {
    #[must_use]
    pub fn new(stop_path: impl Into<PathBuf>, signal: Arc<AtomicBool>) -> Self {
        Self {
            stop_path: stop_path.into(),
            signal,
        }
    }

    #[must_use]
    pub fn is_requested(&self) -> bool {
        self.signal.load(Ordering::Relaxed) || self.stop_path.exists()
    }

    /// Records a graceful stop request.
    ///
    /// # Errors
    ///
    /// Returns an error when the control file cannot be created.
    pub fn request(&self) -> Result<()> {
        if let Some(parent) = self.stop_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                PipelineError::io(format!("create {}", parent.display()), error)
            })?;
        }
        File::create(&self.stop_path)
            .map(|_| ())
            .map_err(|error| PipelineError::io("request graceful stop", error))
    }

    /// Clears a previous stop request.
    ///
    /// # Errors
    ///
    /// Returns an error when an existing control file cannot be removed.
    pub fn clear(&self) -> Result<()> {
        match fs::remove_file(&self.stop_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PipelineError::io("clear graceful stop", error)),
        }
    }
}

/// Computes a lowercase SHA-256 digest.
///
/// # Errors
///
/// Returns an error when the file cannot be opened or read.
pub fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path)
        .map_err(|error| PipelineError::io(format!("open {}", path.display()), error))?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| PipelineError::io(format!("read {}", path.display()), error))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let bytes = digest.finalize();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(encoded)
}

/// Returns the UNIX modified timestamp for a file.
///
/// # Errors
///
/// Returns an error when metadata or the timestamp cannot be read.
pub fn modified_seconds(path: &Path) -> Result<u64> {
    let modified = fs::metadata(path)
        .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
        .modified()
        .map_err(|error| PipelineError::io(format!("mtime {}", path.display()), error))?;
    modified
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| PipelineError::Message("file timestamp predates UNIX epoch".to_owned()))
}

/// Validates a recorded output in either the staging output directory or the
/// optional final library directory.
///
/// # Errors
///
/// Returns an error when an existing candidate cannot be inspected or hashed.
pub fn completion_output_valid(
    profile: &ProfileConfig,
    record: &CompletionRecord,
    reverify: bool,
) -> Result<bool> {
    let mut roots = vec![profile.output_dir.as_path()];
    if let Some(library) = profile.library_dir.as_deref() {
        if library != profile.output_dir {
            roots.push(library);
        }
    }
    for root in roots {
        let output = root.join(&record.output_name);
        let metadata = match fs::metadata(&output) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(PipelineError::io(
                    format!("stat {}", output.display()),
                    error,
                ));
            }
        };
        if metadata.len() != record.size || modified_seconds(&output)? != record.modified_seconds {
            continue;
        }
        if reverify && sha256_file(&output)? != record.sha256 {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| PipelineError::io(format!("create {}", parent.display()), error))?;
    }
    let temporary = path.with_extension("new");
    fs::write(&temporary, content)
        .map_err(|error| PipelineError::io(format!("write {}", temporary.display()), error))?;
    fs::rename(&temporary, path)
        .map_err(|error| PipelineError::io(format!("publish {}", path.display()), error))
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::{CompletionRecord, completion_output_valid, modified_seconds, sha256_file};
    use crate::{ProfileConfig, SystemKind};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn legacy_completion_marker_round_trips() {
        let line = "abc123\t42\t1000\tGame Name.wua";
        let record = CompletionRecord::parse(line).expect("parse marker");
        assert_eq!(record.sha256, "abc123");
        assert_eq!(record.size, 42);
        assert_eq!(record.modified_seconds, 1000);
        assert_eq!(record.output_name, "Game Name.wua");
        assert_eq!(record.component_fingerprint, None);
        assert_eq!(record.encode(), format!("{line}\n"));
    }

    #[test]
    fn component_fingerprint_round_trips() {
        let line = "abc123\t42\t1000\tGame Name.wua\tset456";
        let record = CompletionRecord::parse(line).expect("parse marker");
        assert_eq!(record.component_fingerprint.as_deref(), Some("set456"));
        assert_eq!(record.encode(), format!("{line}\n"));
    }

    #[test]
    fn completion_can_be_validated_in_final_library() {
        let root = fixture_path();
        let staging = root.join("staging");
        let library = root.join("library");
        fs::create_dir_all(&library).expect("create library");
        let output = library.join("Game.chd");
        fs::write(&output, b"validated output").expect("write output");
        let record = CompletionRecord {
            sha256: sha256_file(&output).expect("hash"),
            size: 16,
            modified_seconds: modified_seconds(&output).expect("mtime"),
            output_name: "Game.chd".to_owned(),
            component_fingerprint: Some("components".to_owned()),
        };
        let profile = ProfileConfig {
            id: "test".to_owned(),
            name: "Test".to_owned(),
            system: SystemKind::PlayStationPortable,
            source_format: "iso".to_owned(),
            source_dir: root.join("source"),
            done_dir: root.join("done"),
            work_dir: root.join("work"),
            state_dir: root.join("state"),
            log_dir: root.join("logs"),
            output_dir: staging,
            library_dir: Some(library.clone()),
            output_format: "chd".to_owned(),
            batch_limit: 5,
            wiiu: None,
            gamecube: None,
            nintendo_3ds: None,
            psp: None,
            ps2: None,
            vita: None,
        };
        assert!(completion_output_valid(&profile, &record, true).expect("validate"));
        fs::remove_file(output).expect("remove output");
        fs::remove_dir(library).expect("remove library");
        fs::remove_dir(root).expect("remove root");
    }

    fn fixture_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rom-pipeline-library-status-{}-{nonce}",
            std::process::id()
        ))
    }
}
