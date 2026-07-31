use std::sync::Arc;

use axum::Json;
use axum::extract::{Form, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use rom_pipeline_core::{AppConfig, BatchPolicy, PipelineError, SystemKind};
use rom_pipeline_service::{
    profile_status, request_stop, service_state, start_prune_service, start_publish_service,
    start_service,
};
use serde::Deserialize;

use crate::{WebState, page};

type WebResult<T> = std::result::Result<T, WebError>;

#[derive(Debug)]
pub struct WebError(PipelineError);

impl From<PipelineError> for WebError {
    fn from(error: PipelineError) -> Self {
        Self(error)
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("ROM Pipeline error: {}", self.0),
        )
            .into_response()
    }
}

pub async fn index(State(state): State<Arc<WebState>>) -> WebResult<Html<String>> {
    let config = AppConfig::load(&state.config_path)?;
    let mut statuses = Vec::with_capacity(config.profiles.len());
    for profile in &config.profiles {
        statuses.push(profile_status(profile)?);
    }
    Ok(Html(page::render(&config, &statuses)))
}

#[derive(Debug, Deserialize)]
pub struct ProfileQuery {
    profile: String,
}

pub async fn status(
    State(state): State<Arc<WebState>>,
    Query(query): Query<ProfileQuery>,
) -> WebResult<Json<rom_pipeline_service::ProfileStatus>> {
    let config = AppConfig::load(&state.config_path)?;
    Ok(Json(profile_status(config.profile(&query.profile)?)?))
}

#[derive(Debug, Deserialize)]
pub struct StartForm {
    profile: String,
    limit: usize,
}

pub async fn start_profile(
    State(state): State<Arc<WebState>>,
    Form(form): Form<StartForm>,
) -> WebResult<Redirect> {
    let config = AppConfig::load(&state.config_path)?;
    let profile = config.profile(&form.profile)?;
    start_service(
        &state.executable,
        &state.config_path,
        profile,
        BatchPolicy::new(form.limit)?,
    )?;
    Ok(Redirect::to("/"))
}

#[derive(Debug, Deserialize)]
pub struct StopForm {
    profile: String,
}

pub async fn stop_profile(
    State(state): State<Arc<WebState>>,
    Form(form): Form<StopForm>,
) -> WebResult<Redirect> {
    let config = AppConfig::load(&state.config_path)?;
    request_stop(config.profile(&form.profile)?)?;
    Ok(Redirect::to("/"))
}

#[derive(Debug, Deserialize)]
pub struct LibraryActionForm {
    profile: String,
    limit: usize,
    confirm: Option<String>,
}

pub async fn publish_profile(
    State(state): State<Arc<WebState>>,
    Form(form): Form<LibraryActionForm>,
) -> WebResult<Redirect> {
    let config = AppConfig::load(&state.config_path)?;
    let profile = config.profile(&form.profile)?;
    if !matches!(
        profile.system,
        SystemKind::GameCube | SystemKind::PlayStationPortable | SystemKind::PlayStation2
    ) {
        return Err(WebError(PipelineError::Message(
            "publish is currently implemented only for GameCube, PSP, and PS2".to_owned(),
        )));
    }
    start_publish_service(
        &state.executable,
        &state.config_path,
        profile,
        BatchPolicy::new(form.limit)?,
    )?;
    Ok(Redirect::to("/"))
}

pub async fn prune_profile(
    State(state): State<Arc<WebState>>,
    Form(form): Form<LibraryActionForm>,
) -> WebResult<Redirect> {
    if form.confirm.as_deref() != Some("yes") {
        return Err(WebError(PipelineError::Message(
            "source pruning requires explicit confirmation".to_owned(),
        )));
    }
    let config = AppConfig::load(&state.config_path)?;
    let profile = config.profile(&form.profile)?;
    if !matches!(
        profile.system,
        SystemKind::GameCube | SystemKind::PlayStationPortable | SystemKind::PlayStation2
    ) {
        return Err(WebError(PipelineError::Message(
            "prune is currently implemented only for GameCube, PSP, and PS2".to_owned(),
        )));
    }
    start_prune_service(
        &state.executable,
        &state.config_path,
        profile,
        BatchPolicy::new(form.limit)?,
    )?;
    Ok(Redirect::to("/"))
}

#[derive(Debug, Deserialize)]
pub struct SaveProfileForm {
    profile: String,
    name: String,
    source_format: String,
    source_dir: String,
    done_dir: String,
    work_dir: String,
    state_dir: String,
    log_dir: String,
    output_dir: String,
    library_dir: Option<String>,
    output_format: String,
    batch_limit: usize,
    manifest: Option<String>,
    cdecrypt: Option<String>,
    zarchive: Option<String>,
    wait_seconds: Option<u64>,
    chdman: Option<String>,
    dolphin_tool: Option<String>,
    block_size: Option<u32>,
    compression: Option<String>,
    compression_level: Option<i32>,
    codec: Option<String>,
    hunk_size: Option<u32>,
    verify_round_trip: Option<bool>,
    minimum_savings_percent: Option<u8>,
    preserve_when_compression_is_not_worthwhile: Option<bool>,
}

pub async fn save_profile(
    State(state): State<Arc<WebState>>,
    Form(form): Form<SaveProfileForm>,
) -> WebResult<Redirect> {
    if service_state(&form.profile)? == "active" {
        return Err(WebError(PipelineError::Message(
            "stop the profile before changing its configuration".to_owned(),
        )));
    }
    let mut config = AppConfig::load(&state.config_path)?;
    let profile = config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == form.profile)
        .ok_or_else(|| {
            PipelineError::InvalidConfig(format!("unknown profile: {}", form.profile))
        })?;
    profile.name = form.name;
    profile.source_format = form.source_format;
    profile.source_dir = form.source_dir.into();
    profile.done_dir = form.done_dir.into();
    profile.work_dir = form.work_dir.into();
    profile.state_dir = form.state_dir.into();
    profile.log_dir = form.log_dir.into();
    profile.output_dir = form.output_dir.into();
    if let Some(library_dir) = form.library_dir {
        profile.library_dir = (!library_dir.trim().is_empty()).then(|| library_dir.into());
    }
    profile.output_format = form.output_format;
    profile.batch_limit = form.batch_limit;
    match profile.system {
        SystemKind::WiiU => {
            let wiiu = profile
                .wiiu
                .as_mut()
                .ok_or_else(|| PipelineError::InvalidConfig("missing Wii U settings".to_owned()))?;
            wiiu.manifest = required(form.manifest, "manifest")?.into();
            wiiu.cdecrypt = required(form.cdecrypt, "cdecrypt")?.into();
            wiiu.zarchive = required(form.zarchive, "zarchive")?.into();
            wiiu.wait_seconds = form.wait_seconds.ok_or_else(|| {
                PipelineError::InvalidConfig("wait_seconds is required".to_owned())
            })?;
        }
        SystemKind::GameCube => {
            let gamecube = profile.gamecube.as_mut().ok_or_else(|| {
                PipelineError::InvalidConfig("missing GameCube settings".to_owned())
            })?;
            gamecube.manifest = required(form.manifest, "manifest")?.into();
            gamecube.dolphin_tool = required(form.dolphin_tool, "dolphin_tool")?.into();
            gamecube.block_size = form
                .block_size
                .ok_or_else(|| PipelineError::InvalidConfig("block_size is required".to_owned()))?;
            gamecube.compression = required(form.compression, "compression")?;
            gamecube.compression_level = form.compression_level.ok_or_else(|| {
                PipelineError::InvalidConfig("compression_level is required".to_owned())
            })?;
            gamecube.verify_round_trip = form.verify_round_trip.ok_or_else(|| {
                PipelineError::InvalidConfig("verify_round_trip is required".to_owned())
            })?;
        }
        SystemKind::Nintendo3ds => {}
        SystemKind::PlayStationPortable => {
            let psp = profile
                .psp
                .as_mut()
                .ok_or_else(|| PipelineError::InvalidConfig("missing PSP settings".to_owned()))?;
            psp.chdman = required(form.chdman, "chdman")?.into();
            psp.codec = required(form.codec, "codec")?;
            psp.hunk_size = form
                .hunk_size
                .ok_or_else(|| PipelineError::InvalidConfig("hunk_size is required".to_owned()))?;
            psp.verify_round_trip = form.verify_round_trip.ok_or_else(|| {
                PipelineError::InvalidConfig("verify_round_trip is required".to_owned())
            })?;
        }
        SystemKind::PlayStation2 => {
            let ps2 = profile
                .ps2
                .as_mut()
                .ok_or_else(|| PipelineError::InvalidConfig("missing PS2 settings".to_owned()))?;
            ps2.manifest = required(form.manifest, "manifest")?.into();
            ps2.chdman = required(form.chdman, "chdman")?.into();
            ps2.minimum_savings_percent = form.minimum_savings_percent.ok_or_else(|| {
                PipelineError::InvalidConfig("minimum_savings_percent is required".to_owned())
            })?;
            ps2.preserve_when_compression_is_not_worthwhile = form
                .preserve_when_compression_is_not_worthwhile
                .ok_or_else(|| {
                    PipelineError::InvalidConfig(
                        "preserve_when_compression_is_not_worthwhile is required".to_owned(),
                    )
                })?;
            ps2.verify_round_trip = form.verify_round_trip.ok_or_else(|| {
                PipelineError::InvalidConfig("verify_round_trip is required".to_owned())
            })?;
        }
    }
    config.save(&state.config_path)?;
    Ok(Redirect::to("/"))
}

fn required(value: Option<String>, field: &str) -> Result<String, PipelineError> {
    value.ok_or_else(|| PipelineError::InvalidConfig(format!("{field} is required")))
}
