use std::path::Path;
use std::process::Command;

use rom_pipeline_core::{BatchPolicy, PipelineError, ProfileConfig, Result, StateStore, StopToken};

#[must_use]
pub fn unit_name(profile_id: &str) -> String {
    format!("rom-pipeline-{profile_id}.service")
}

/// Starts a profile as a transient user service.
///
/// # Errors
///
/// Returns an error if service state cannot be queried or `systemd-run` fails.
pub fn start_service(
    executable: &Path,
    config_path: &Path,
    profile: &ProfileConfig,
    limit: BatchPolicy,
) -> Result<()> {
    start_action_service(executable, config_path, profile, limit, "run", false)
}

/// Starts PSP publication as the profile's transient worker.
///
/// # Errors
///
/// Returns an error when another profile action is active or systemd-run
/// fails.
pub fn start_publish_service(
    executable: &Path,
    config_path: &Path,
    profile: &ProfileConfig,
    limit: BatchPolicy,
) -> Result<()> {
    start_action_service(executable, config_path, profile, limit, "publish", true)
}

/// Starts confirmed PSP source pruning as the profile's transient worker.
///
/// # Errors
///
/// Returns an error when another profile action is active or systemd-run
/// fails.
pub fn start_prune_service(
    executable: &Path,
    config_path: &Path,
    profile: &ProfileConfig,
    limit: BatchPolicy,
) -> Result<()> {
    start_action_service(executable, config_path, profile, limit, "prune", true)
}

fn start_action_service(
    executable: &Path,
    config_path: &Path,
    profile: &ProfileConfig,
    limit: BatchPolicy,
    action: &str,
    reject_active: bool,
) -> Result<()> {
    let unit = unit_name(&profile.id);
    if service_state(&profile.id)? == "active" {
        if reject_active {
            return Err(PipelineError::Message(format!(
                "profile {} is active; stop or wait for it before {action}",
                profile.id
            )));
        }
        return Ok(());
    }
    let _ = Command::new("systemctl")
        .args(["--user", "reset-failed", &unit])
        .status();
    let mut command = Command::new("systemd-run");
    command
        .args([
            "--user",
            &format!("--unit={}", unit.trim_end_matches(".service")),
            "--description=ROM Pipeline conversion worker",
            "--property=Nice=10",
            "--property=IOSchedulingClass=best-effort",
            "--property=IOSchedulingPriority=6",
        ])
        .arg(executable)
        .args([action, "--config"])
        .arg(config_path)
        .args([
            "--profile",
            &profile.id,
            "--limit",
            &limit.limit().to_string(),
        ]);
    if action == "prune" {
        command.arg("--confirm-prune");
    }
    let status = command
        .status()
        .map_err(|error| PipelineError::io("start profile service", error))?;
    if status.success() {
        Ok(())
    } else {
        Err(PipelineError::CommandFailed {
            command: format!("systemd-run ROM Pipeline {action} worker"),
            status: status.to_string(),
        })
    }
}

/// Requests a clean stop after the current safe processing boundary.
///
/// # Errors
///
/// Returns an error if the stop request cannot be recorded.
pub fn request_stop(profile: &ProfileConfig) -> Result<()> {
    let state = StateStore::new(&profile.state_dir, &profile.log_dir);
    state.prepare()?;
    StopToken::new(
        state.stop_path(),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .request()
}

/// Returns the user service state for a profile.
///
/// # Errors
///
/// Returns an error when `systemctl` cannot be executed.
pub fn service_state(profile_id: &str) -> Result<String> {
    let output = Command::new("systemctl")
        .args(["--user", "is-active", &unit_name(profile_id)])
        .output()
        .map_err(|error| PipelineError::io("query profile service", error))?;
    let state = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if state.is_empty() {
        Ok("inactive".to_owned())
    } else {
        Ok(state)
    }
}
