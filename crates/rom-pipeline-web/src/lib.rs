mod handlers;
mod page;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use rom_pipeline_core::{AppConfig, PipelineError, Result};

#[derive(Clone, Debug)]
struct WebState {
    config_path: PathBuf,
    executable: PathBuf,
}

/// Runs the loopback configuration and status screen.
///
/// # Errors
///
/// Returns an error when configuration, address parsing, listener creation, or
/// HTTP serving fails.
pub async fn serve(config_path: &Path, executable: &Path) -> Result<()> {
    let config = AppConfig::load(config_path)?;
    let address: SocketAddr = config.service.bind.parse().map_err(|error| {
        PipelineError::InvalidConfig(format!(
            "invalid service bind address {}: {error}",
            config.service.bind
        ))
    })?;
    if !address.ip().is_loopback() {
        return Err(PipelineError::InvalidConfig(
            "the first web release must bind to a loopback address".to_owned(),
        ));
    }
    let state = Arc::new(WebState {
        config_path: config_path.to_path_buf(),
        executable: executable.to_path_buf(),
    });
    let app = Router::new()
        .route("/", get(handlers::index))
        .route("/api/status", get(handlers::status))
        .route("/profiles/save", post(handlers::save_profile))
        .route("/profiles/publish", post(handlers::publish_profile))
        .route("/profiles/prune", post(handlers::prune_profile))
        .route("/profiles/start", post(handlers::start_profile))
        .route("/profiles/stop", post(handlers::stop_profile))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| PipelineError::io(format!("bind {address}"), error))?;
    println!("ROM Pipeline screen: http://{address}");
    axum::serve(listener, app)
        .await
        .map_err(|error| PipelineError::io("serve configuration screen", error))
}
