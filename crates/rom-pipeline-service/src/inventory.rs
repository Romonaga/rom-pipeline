use rom_pipeline_core::{Job, PipelineAdapter, ProfileConfig, Result, SystemKind};
use rom_pipeline_gamecube::GameCubeAdapter;
use rom_pipeline_nintendo_3ds::Nintendo3dsAdapter;
use rom_pipeline_playstation_vita::VitaAdapter;
use rom_pipeline_ps2::Ps2Adapter;
use rom_pipeline_psp::PspAdapter;
use rom_pipeline_wiiu::WiiUAdapter;
use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct JobChoice {
    pub id: String,
    pub name: String,
}

/// Returns the selectable jobs for a profile without running its preflight.
///
/// # Errors
///
/// Returns an error when the profile adapter or its inventory is invalid.
pub fn profile_jobs(profile: &ProfileConfig) -> Result<Vec<JobChoice>> {
    let jobs = match profile.system {
        SystemKind::WiiU => WiiUAdapter::new(profile.clone())?.inventory(None)?,
        SystemKind::GameCube => GameCubeAdapter::new(profile.clone())?.inventory(None)?,
        SystemKind::Nintendo3ds => Nintendo3dsAdapter::new(profile.clone())?.inventory(None)?,
        SystemKind::PlayStationPortable => PspAdapter::new(profile.clone())?.inventory(None)?,
        SystemKind::PlayStation2 => Ps2Adapter::new(profile.clone())?.inventory(None)?,
        SystemKind::PlayStationVita => VitaAdapter::new(profile.clone())?.inventory(None)?,
    };
    Ok(jobs.into_iter().map(job_choice).collect())
}

fn job_choice(job: Job) -> JobChoice {
    JobChoice {
        id: job.id,
        name: job.display_name,
    }
}
