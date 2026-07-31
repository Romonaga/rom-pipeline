use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rom_pipeline_core::{
    CompletionRecord, GameCubeSettings, Job, JobOutcome, PipelineAdapter, PipelineError,
    ProfileConfig, Readiness, Result, StateStore, StopToken, completion_output_valid,
    modified_seconds,
};

use crate::inventory::GameCubeInventory;
use crate::process::process_job;

#[derive(Clone, Debug)]
pub struct GameCubeAdapter {
    profile: ProfileConfig,
}

impl GameCubeAdapter {
    /// Creates a validated `GameCube` adapter.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile is invalid or belongs to another
    /// system.
    pub fn new(profile: ProfileConfig) -> Result<Self> {
        profile.validate()?;
        if !matches!(profile.system, rom_pipeline_core::SystemKind::GameCube) {
            return Err(PipelineError::InvalidConfig(format!(
                "profile {} is not GameCube",
                profile.id
            )));
        }
        Ok(Self { profile })
    }

    #[must_use]
    pub fn profile(&self) -> &ProfileConfig {
        &self.profile
    }

    pub(crate) fn settings(&self) -> Result<&GameCubeSettings> {
        self.profile
            .gamecube
            .as_ref()
            .ok_or_else(|| PipelineError::InvalidConfig("missing GameCube settings".to_owned()))
    }

    /// Validates tools and filesystems and creates adapter-owned directories.
    ///
    /// # Errors
    ///
    /// Returns an error when a required tool, manifest, path, or filesystem is
    /// unavailable.
    pub fn preflight(&self) -> Result<()> {
        if !self.profile.source_dir.is_dir() {
            return Err(PipelineError::MissingPath(self.profile.source_dir.clone()));
        }
        if !self.profile.source_format.eq_ignore_ascii_case("iso")
            || !self.profile.output_format.eq_ignore_ascii_case("rvz")
        {
            return Err(PipelineError::InvalidConfig(
                "GameCube conversion requires source_format=iso and output_format=rvz".to_owned(),
            ));
        }
        let settings = self.settings()?;
        if !settings.manifest.is_file() {
            return Err(PipelineError::MissingPath(settings.manifest.clone()));
        }
        let metadata = fs::metadata(&settings.dolphin_tool).map_err(|error| {
            PipelineError::io(format!("stat {}", settings.dolphin_tool.display()), error)
        })?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(PipelineError::Message(format!(
                "required tool is not executable: {}",
                settings.dolphin_tool.display()
            )));
        }
        for path in [
            &self.profile.done_dir,
            &self.profile.work_dir,
            &self.profile.state_dir,
            &self.profile.log_dir,
            &self.profile.log_dir.join("groups"),
            &self.profile.output_dir,
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
                "GameCube source exists in source and done: {name}"
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
                        "GameCube source exists in source and done: {}",
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
                    state.log(&format!("MOVED GameCube source to done: {}", artifact.name))?;
                }
                (false, true) => {}
                (false, false) => return Err(PipelineError::MissingPath(source)),
            }
        }
        Ok(())
    }

    fn all_sources_are_done(&self, job: &Job) -> bool {
        job.sources.iter().all(|artifact| {
            !self.profile.source_dir.join(&artifact.name).exists()
                && self.profile.done_dir.join(&artifact.name).is_file()
        })
    }
}

impl PipelineAdapter for GameCubeAdapter {
    fn inventory(&self, only_job: Option<&str>) -> Result<Vec<Job>> {
        Ok(GameCubeInventory::from_manifest(&self.settings()?.manifest, only_job)?.jobs)
    }

    fn readiness(&self, job: &Job) -> Result<Readiness> {
        for artifact in &job.sources {
            let Some(path) = self.locate(&artifact.name)? else {
                return Ok(Readiness::Waiting);
            };
            let actual = fs::metadata(&path)
                .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
                .len();
            if actual != artifact.expected_size {
                return Ok(Readiness::Waiting);
            }
        }
        Ok(Readiness::Ready)
    }

    fn is_complete(&self, job: &Job, state: &StateStore, reverify: bool) -> Result<bool> {
        let Some(record) = state.read_completion(&job.id)? else {
            return Ok(false);
        };
        Ok(completion_output_valid(&self.profile, &record, reverify)?
            && record.component_fingerprint.as_deref() == Some(&job.component_fingerprint()))
    }

    fn reconcile_completed(&self, job: &Job, state: &StateStore) -> Result<()> {
        if self.all_sources_are_done(job) {
            Ok(())
        } else {
            self.move_sources_to_done(job, state)
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
