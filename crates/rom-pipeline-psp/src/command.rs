use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use rom_pipeline_core::{PipelineError, Result};

pub fn output<I, S>(program: impl AsRef<OsStr>, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args
        .into_iter()
        .map(|value| value.as_ref().to_owned())
        .collect();
    let result = Command::new(program.as_ref())
        .args(&args)
        .output()
        .map_err(|error| {
            PipelineError::io(
                format!("execute {}", program.as_ref().to_string_lossy()),
                error,
            )
        })?;
    if result.status.success() {
        Ok(result)
    } else {
        Err(command_failed(
            program.as_ref(),
            &args,
            &result.status.to_string(),
        ))
    }
}

pub fn run_logged<I, S>(program: impl AsRef<OsStr>, args: I, log_path: &Path) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args
        .into_iter()
        .map(|value| value.as_ref().to_owned())
        .collect();
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| PipelineError::io(format!("open {}", log_path.display()), error))?;
    let error_log = log
        .try_clone()
        .map_err(|error| PipelineError::io("clone PSP group log", error))?;
    let status = Command::new(program.as_ref())
        .args(&args)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(error_log))
        .status()
        .map_err(|error| {
            PipelineError::io(
                format!("execute {}", program.as_ref().to_string_lossy()),
                error,
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(command_failed(program.as_ref(), &args, &status.to_string()))
    }
}

fn command_failed(program: &OsStr, args: &[OsString], status: &str) -> PipelineError {
    PipelineError::CommandFailed {
        command: format!(
            "{} {}",
            program.to_string_lossy(),
            args.iter()
                .map(|value| value.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        status: status.to_owned(),
    }
}
