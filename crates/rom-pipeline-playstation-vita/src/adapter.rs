use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use rom_pipeline_core::{
    Job, JobOutcome, PipelineAdapter, PipelineError, ProfileConfig, Readiness, Result, StateStore,
    StopToken,
};

use crate::deployment;
use crate::inventory::VitaInventory;

#[derive(Clone, Debug)]
pub struct VitaAdapter {
    profile: ProfileConfig,
}

impl VitaAdapter {
    /// Creates a validated `PlayStation Vita` adapter.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration or a non-Vita profile.
    pub fn new(profile: ProfileConfig) -> Result<Self> {
        profile.validate()?;
        if !matches!(
            profile.system,
            rom_pipeline_core::SystemKind::PlayStationVita
        ) {
            return Err(PipelineError::InvalidConfig(format!(
                "profile {} is not PlayStation Vita",
                profile.id
            )));
        }
        Ok(Self { profile })
    }

    #[must_use]
    pub fn profile(&self) -> &ProfileConfig {
        &self.profile
    }

    pub(crate) fn settings(&self) -> Result<&rom_pipeline_core::VitaSettings> {
        self.profile
            .vita
            .as_ref()
            .ok_or_else(|| PipelineError::InvalidConfig("missing Vita settings".to_owned()))
    }

    pub(crate) fn device_root(&self) -> Result<&Path> {
        self.profile.library_dir.as_deref().ok_or_else(|| {
            PipelineError::InvalidConfig("Vita library_dir must point to mounted ux0".to_owned())
        })
    }

    /// Validates tools, paths, and the mounted `SD2Vita` destination.
    ///
    /// # Errors
    ///
    /// Returns an error when a tool, source, state path, or device mount is unavailable.
    pub fn preflight(&self) -> Result<()> {
        if !self
            .profile
            .source_format
            .eq_ignore_ascii_case("nonpdrm-zip")
            || !self
                .profile
                .output_format
                .eq_ignore_ascii_case("native-vita-tree")
        {
            return Err(PipelineError::InvalidConfig(
                "Vita deployment requires source_format=nonpdrm-zip and \
                 output_format=native-vita-tree"
                    .to_owned(),
            ));
        }
        if !self.profile.source_dir.is_dir() {
            return Err(PipelineError::MissingPath(self.profile.source_dir.clone()));
        }
        for tool in [&self.settings()?.seven_zip, &self.settings()?.mountpoint] {
            let metadata = fs::metadata(tool)
                .map_err(|error| PipelineError::io(format!("stat {}", tool.display()), error))?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return Err(PipelineError::Message(format!(
                    "required Vita tool is not executable: {}",
                    tool.display()
                )));
            }
        }
        for path in [
            &self.profile.work_dir,
            &self.profile.state_dir,
            &self.profile.log_dir,
            &self.profile.output_dir,
            &self.profile.log_dir.join("groups"),
        ] {
            fs::create_dir_all(path)
                .map_err(|error| PipelineError::io(format!("create {}", path.display()), error))?;
        }
        let device = self.device_root()?;
        if !device.is_dir() {
            return Err(PipelineError::MissingPath(device.to_path_buf()));
        }
        let status = Command::new(&self.settings()?.mountpoint)
            .arg("--quiet")
            .arg(device)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| {
                PipelineError::io(format!("check mount {}", device.display()), error)
            })?;
        if !status.success() {
            return Err(PipelineError::Message(format!(
                "Vita destination is not a mounted SD2Vita ux0: {}",
                device.display()
            )));
        }
        Ok(())
    }

    pub(crate) fn ensure_device_space(&self, needed: u64) -> Result<()> {
        let device = self.device_root()?;
        let available = fs2::available_space(device).map_err(|error| {
            PipelineError::io(format!("read free space on {}", device.display()), error)
        })?;
        let required = needed
            .checked_add(self.settings()?.reserve_bytes)
            .ok_or_else(|| {
                PipelineError::Message("Vita space requirement overflowed".to_owned())
            })?;
        if available < required {
            return Err(PipelineError::Message(format!(
                "SD2Vita needs {required} bytes including reserve; only {available} bytes are free"
            )));
        }
        Ok(())
    }
}

impl PipelineAdapter for VitaAdapter {
    fn inventory(&self, only_job: Option<&str>) -> Result<Vec<Job>> {
        Ok(VitaInventory::from_directory(&self.profile.source_dir, only_job)?.jobs)
    }

    fn readiness(&self, job: &Job) -> Result<Readiness> {
        let artifact = job
            .sources
            .first()
            .ok_or_else(|| PipelineError::Message("Vita job has no source".to_owned()))?;
        let path = self.profile.source_dir.join(&artifact.name);
        let size = match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata.len(),
            Ok(_) => return Ok(Readiness::Waiting),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Readiness::Waiting);
            }
            Err(error) => {
                return Err(PipelineError::io(format!("stat {}", path.display()), error));
            }
        };
        Ok(if size == artifact.expected_size {
            Readiness::Ready
        } else {
            Readiness::Waiting
        })
    }

    fn is_complete(&self, job: &Job, state: &StateStore, reverify: bool) -> Result<bool> {
        // A stop request belongs to a running deployment. Status and inventory
        // checks must remain readable after a clean stop has left that marker.
        let stop = StopToken::new(
            state.root().join(".status-check-never-stopped"),
            Arc::new(AtomicBool::new(false)),
        );
        deployment::installed(self, job, state, reverify, &stop)
    }

    fn reconcile_completed(&self, _job: &Job, _state: &StateStore) -> Result<()> {
        Ok(())
    }

    fn process_job(&self, job: &Job, state: &StateStore, stop: &StopToken) -> Result<JobOutcome> {
        deployment::deploy(self, job, state, stop)
    }
}
