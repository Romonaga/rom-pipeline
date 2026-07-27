use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use rom_pipeline_core::{
    CompletionRecord, Job, JobOutcome, PipelineAdapter, PipelineError, ProfileConfig, Readiness,
    Result, StateStore, StopToken, completion_output_valid, modified_seconds,
};

use crate::inventory::WiiUInventory;
use crate::process::process_job;

#[derive(Clone, Debug)]
pub struct WiiUAdapter {
    profile: ProfileConfig,
}

impl WiiUAdapter {
    /// Creates a validated Wii U adapter.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile is invalid or is not a Wii U profile.
    pub fn new(profile: ProfileConfig) -> Result<Self> {
        profile.validate()?;
        if !matches!(profile.system, rom_pipeline_core::SystemKind::WiiU) {
            return Err(PipelineError::InvalidConfig(format!(
                "profile {} is not Wii U",
                profile.id
            )));
        }
        Ok(Self { profile })
    }

    #[must_use]
    pub fn profile(&self) -> &ProfileConfig {
        &self.profile
    }

    /// Validates external tools, source metadata, and creates owned
    /// destination directories.
    ///
    /// # Errors
    ///
    /// Returns an error when a required path, tool, or filesystem is
    /// unavailable.
    pub fn preflight(&self) -> Result<()> {
        if !self.profile.source_dir.is_dir() {
            return Err(PipelineError::MissingPath(self.profile.source_dir.clone()));
        }
        let settings = self
            .profile
            .wiiu
            .as_ref()
            .ok_or_else(|| PipelineError::InvalidConfig("missing Wii U settings".to_owned()))?;
        if !settings.manifest.is_file() {
            return Err(PipelineError::MissingPath(settings.manifest.clone()));
        }
        for tool in [&settings.cdecrypt, &settings.zarchive] {
            let metadata = fs::metadata(tool)
                .map_err(|error| PipelineError::io(format!("stat {}", tool.display()), error))?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return Err(PipelineError::Message(format!(
                    "required tool is not executable: {}",
                    tool.display()
                )));
            }
        }
        for command in ["7z", "diff", "systemctl", "systemd-run"] {
            let status = Command::new("sh")
                .args(["-c", &format!("command -v {command} >/dev/null")])
                .status()
                .map_err(|error| PipelineError::io(format!("look up {command}"), error))?;
            if !status.success() {
                return Err(PipelineError::Message(format!(
                    "required command is missing: {command}"
                )));
            }
        }
        for path in [
            &self.profile.done_dir,
            &self.profile.work_dir,
            &self.profile.state_dir,
            &self.profile.log_dir,
            &self.profile.output_dir,
        ] {
            fs::create_dir_all(path)
                .map_err(|error| PipelineError::io(format!("create {}", path.display()), error))?;
        }
        for path in [&self.profile.source_dir, &self.profile.work_dir] {
            let status = Command::new("timeout")
                .args(["20", "stat", "-f"])
                .arg(path)
                .status()
                .map_err(|error| {
                    PipelineError::io(format!("probe filesystem {}", path.display()), error)
                })?;
            if !status.success() {
                return Err(PipelineError::Message(format!(
                    "filesystem is not responding: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn locate(&self, name: &str) -> Result<Option<PathBuf>> {
        let source = self.profile.source_dir.join(name);
        let done = self.profile.done_dir.join(name);
        match (source.is_file(), done.is_file()) {
            (true, true) => Err(PipelineError::Message(format!(
                "archive exists in source and done: {name}"
            ))),
            (true, false) => Ok(Some(source)),
            (false, true) => Ok(Some(done)),
            (false, false) => Ok(None),
        }
    }

    pub(crate) fn move_sources_to_done(&self, job: &Job, state: &StateStore) -> Result<()> {
        fs::create_dir_all(&self.profile.done_dir).map_err(|error| {
            PipelineError::io(format!("create {}", self.profile.done_dir.display()), error)
        })?;
        for artifact in &job.sources {
            let source = self.profile.source_dir.join(&artifact.name);
            let done = self.profile.done_dir.join(&artifact.name);
            match (source.is_file(), done.is_file()) {
                (true, true) => {
                    return Err(PipelineError::Message(format!(
                        "archive exists in source and done: {}",
                        artifact.name
                    )));
                }
                (true, false) => {
                    fs::rename(&source, &done).map_err(|error| {
                        PipelineError::io(
                            format!("move {} to {}", source.display(), done.display()),
                            error,
                        )
                    })?;
                    state.log(&format!("MOVED source to done: {}", artifact.name))?;
                }
                (false, true) => {}
                (false, false) => {
                    return Err(PipelineError::MissingPath(source));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn output_path(&self, job: &Job) -> PathBuf {
        self.profile.output_dir.join(&job.output_name)
    }

    fn marker_valid(&self, record: &CompletionRecord, reverify: bool) -> Result<bool> {
        completion_output_valid(&self.profile, record, reverify)
    }

    fn all_sources_are_done(&self, job: &Job) -> bool {
        job.sources.iter().all(|artifact| {
            !self.profile.source_dir.join(&artifact.name).exists()
                && self.profile.done_dir.join(&artifact.name).is_file()
        })
    }
}

impl PipelineAdapter for WiiUAdapter {
    fn inventory(&self, only_job: Option<&str>) -> Result<Vec<Job>> {
        let settings = self
            .profile
            .wiiu
            .as_ref()
            .ok_or_else(|| PipelineError::InvalidConfig("missing Wii U settings".to_owned()))?;
        Ok(WiiUInventory::from_manifest(&settings.manifest, only_job)?.jobs)
    }

    fn readiness(&self, job: &Job) -> Result<Readiness> {
        for artifact in &job.sources {
            let Some(path) = self.locate(&artifact.name)? else {
                return Ok(Readiness::Waiting);
            };
            let actual = fs::metadata(&path)
                .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
                .len();
            if artifact.expected_size > 0 && actual != artifact.expected_size {
                return Ok(Readiness::Waiting);
            }
        }
        Ok(Readiness::Ready)
    }

    fn is_complete(&self, job: &Job, state: &StateStore, reverify: bool) -> Result<bool> {
        let Some(record) = state.read_completion(&job.id)? else {
            return Ok(false);
        };
        if !self.marker_valid(&record, reverify)? {
            return Ok(false);
        }
        Ok(record.component_fingerprint.as_ref().map_or_else(
            || self.all_sources_are_done(job),
            |fingerprint| fingerprint == &job.component_fingerprint(),
        ))
    }

    fn reconcile_completed(&self, job: &Job, state: &StateStore) -> Result<()> {
        let mut record = state.read_completion(&job.id)?.ok_or_else(|| {
            PipelineError::Message(format!("completion marker disappeared: {}", job.id))
        })?;
        match record.component_fingerprint.as_ref() {
            Some(fingerprint) if fingerprint == &job.component_fingerprint() => {
                self.move_sources_to_done(job, state)
            }
            Some(_) => Err(PipelineError::Message(format!(
                "component set changed after completion: {}",
                job.id
            ))),
            None if self.all_sources_are_done(job) => {
                record.component_fingerprint = Some(job.component_fingerprint());
                state.write_completion(&job.id, &record)?;
                state.log(&format!(
                    "MIGRATED legacy completion marker with component fingerprint: {}",
                    job.id
                ))
            }
            None => Err(PipelineError::Message(format!(
                "legacy completion cannot prove all components are done: {}",
                job.id
            ))),
        }
    }

    fn process_job(&self, job: &Job, state: &StateStore, stop: &StopToken) -> Result<JobOutcome> {
        process_job(self, job, state, stop)
    }
}

pub(crate) fn completion_record(
    job: &Job,
    output: &Path,
    hash: String,
) -> Result<CompletionRecord> {
    let metadata = fs::metadata(output)
        .map_err(|error| PipelineError::io(format!("stat {}", output.display()), error))?;
    let output_name = output
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| PipelineError::Message("output filename is not valid UTF-8".to_owned()))?
        .to_owned();
    Ok(CompletionRecord {
        sha256: hash,
        size: metadata.len(),
        modified_seconds: modified_seconds(output)?,
        output_name,
        component_fingerprint: Some(job.component_fingerprint()),
    })
}
