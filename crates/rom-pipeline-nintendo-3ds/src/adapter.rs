use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rom_pipeline_core::{
    CompletionRecord, Job, JobOutcome, PipelineAdapter, PipelineError, ProfileConfig, Readiness,
    Result, StateStore, StopToken, completion_output_valid, modified_seconds,
};

use crate::inventory::Nintendo3dsInventory;
use crate::process::process_job;

#[derive(Clone, Debug)]
pub struct Nintendo3dsAdapter {
    profile: ProfileConfig,
}

impl Nintendo3dsAdapter {
    /// Creates a validated Nintendo 3DS adapter.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile is invalid or is not a 3DS profile.
    pub fn new(profile: ProfileConfig) -> Result<Self> {
        profile.validate()?;
        if !matches!(profile.system, rom_pipeline_core::SystemKind::Nintendo3ds) {
            return Err(PipelineError::InvalidConfig(format!(
                "profile {} is not Nintendo 3DS",
                profile.id
            )));
        }
        Ok(Self { profile })
    }

    #[must_use]
    pub fn profile(&self) -> &ProfileConfig {
        &self.profile
    }

    /// Creates adapter-owned directories and confirms the source exists.
    ///
    /// # Errors
    ///
    /// Returns an error when a required directory is unavailable.
    pub fn preflight(&self) -> Result<()> {
        if !self.profile.source_dir.is_dir() {
            return Err(PipelineError::MissingPath(self.profile.source_dir.clone()));
        }
        if !self.profile.source_format.eq_ignore_ascii_case("zip-3ds")
            || !self.profile.output_format.eq_ignore_ascii_case("cia")
        {
            return Err(PipelineError::InvalidConfig(
                "Nintendo 3DS conversion requires source_format=zip-3ds and output_format=cia"
                    .to_owned(),
            ));
        }
        let settings = self.settings()?;
        if !settings.normalize_crypto_flags {
            return Err(PipelineError::InvalidConfig(
                "Nintendo 3DS crypto-flag normalization must be enabled".to_owned(),
            ));
        }
        if !settings.manifest.is_file() {
            return Err(PipelineError::MissingPath(settings.manifest.clone()));
        }
        for tool in [
            &settings.seven_zip,
            &settings.python,
            &settings.converter,
            &settings.ctrtool,
        ] {
            let metadata = fs::metadata(tool)
                .map_err(|error| PipelineError::io(format!("stat {}", tool.display()), error))?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return Err(PipelineError::Message(format!(
                    "required 3DS tool is not executable: {}",
                    tool.display()
                )));
            }
        }
        for path in [
            &self.profile.done_dir,
            &self.profile.work_dir,
            &self.profile.state_dir,
            &self.profile.log_dir,
            &self.profile.output_dir,
            &self.profile.log_dir.join("groups"),
            &self.profile.work_dir.join("groups"),
        ] {
            fs::create_dir_all(path)
                .map_err(|error| PipelineError::io(format!("create {}", path.display()), error))?;
        }
        for path in [
            &self.profile.source_dir,
            &self.profile.work_dir,
            &self.profile.output_dir,
        ] {
            let status = Command::new("timeout")
                .args(["20", "stat", "-f"])
                .arg(path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|error| PipelineError::io(format!("probe {}", path.display()), error))?;
            if !status.success() {
                return Err(PipelineError::Message(format!(
                    "filesystem is not responding: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn settings(&self) -> Result<&rom_pipeline_core::Nintendo3dsSettings> {
        self.profile
            .nintendo_3ds
            .as_ref()
            .ok_or_else(|| PipelineError::InvalidConfig("missing Nintendo 3DS settings".to_owned()))
    }

    pub(crate) fn locate(&self, name: &str) -> Result<Option<PathBuf>> {
        let source = self.profile.source_dir.join(name);
        let done = self.profile.done_dir.join(name);
        match (source.is_file(), done.is_file()) {
            (true, true) => Err(PipelineError::Message(format!(
                "3DS image exists in source and done: {name}"
            ))),
            (true, false) => Ok(Some(source)),
            (false, true) => Ok(Some(done)),
            (false, false) => Ok(None),
        }
    }

    pub(crate) fn output_path(&self, job: &Job) -> PathBuf {
        self.profile.output_dir.join(&job.output_name)
    }

    pub(crate) fn move_source_to_done(&self, job: &Job, state: &StateStore) -> Result<()> {
        let artifact = job
            .sources
            .first()
            .ok_or_else(|| PipelineError::Message("3DS job has no source".to_owned()))?;
        fs::create_dir_all(&self.profile.done_dir).map_err(|error| {
            PipelineError::io(format!("create {}", self.profile.done_dir.display()), error)
        })?;
        let source = self.profile.source_dir.join(&artifact.name);
        let done = self.profile.done_dir.join(&artifact.name);
        match (source.is_file(), done.is_file()) {
            (true, true) => Err(PipelineError::Message(format!(
                "3DS image exists in source and done: {}",
                artifact.name
            ))),
            (true, false) => {
                fs::rename(&source, &done).map_err(|error| {
                    PipelineError::io(
                        format!("move {} to {}", source.display(), done.display()),
                        error,
                    )
                })?;
                state.log(&format!("MOVED original source to done: {}", artifact.name))
            }
            (false, true) => Ok(()),
            (false, false) => Err(PipelineError::MissingPath(source)),
        }
    }

    fn marker_valid(&self, record: &CompletionRecord, reverify: bool) -> Result<bool> {
        completion_output_valid(&self.profile, record, reverify)
    }

    fn source_is_done(&self, job: &Job) -> bool {
        job.sources.first().is_some_and(|artifact| {
            !self.profile.source_dir.join(&artifact.name).exists()
                && self.profile.done_dir.join(&artifact.name).is_file()
        })
    }
}

impl PipelineAdapter for Nintendo3dsAdapter {
    fn inventory(&self, only_job: Option<&str>) -> Result<Vec<Job>> {
        Ok(Nintendo3dsInventory::from_manifest(&self.settings()?.manifest, only_job)?.jobs)
    }

    fn readiness(&self, job: &Job) -> Result<Readiness> {
        let artifact = job
            .sources
            .first()
            .ok_or_else(|| PipelineError::Message("3DS job has no source".to_owned()))?;
        let Some(path) = self.locate(&artifact.name)? else {
            return Ok(Readiness::Waiting);
        };
        let size = fs::metadata(&path)
            .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
            .len();
        Ok(if size == artifact.expected_size {
            Readiness::Ready
        } else {
            Readiness::Waiting
        })
    }

    fn is_complete(&self, job: &Job, state: &StateStore, reverify: bool) -> Result<bool> {
        let Some(record) = state.read_completion(&job.id)? else {
            return Ok(false);
        };
        Ok(self.marker_valid(&record, reverify)?
            && record.component_fingerprint.as_deref() == Some(&job.component_fingerprint()))
    }

    fn reconcile_completed(&self, job: &Job, state: &StateStore) -> Result<()> {
        if self.source_is_done(job) {
            Ok(())
        } else {
            self.move_source_to_done(job, state)
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
    Ok(CompletionRecord {
        sha256: hash,
        size: metadata.len(),
        modified_seconds: modified_seconds(output)?,
        output_name: job.output_name.clone(),
        component_fingerprint: Some(job.component_fingerprint()),
    })
}
