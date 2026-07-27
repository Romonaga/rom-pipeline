use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use rom_pipeline_core::{PipelineError, Result};

pub fn run_logged(
    program: &Path,
    arguments: impl IntoIterator<Item = OsString>,
    log: &Path,
) -> Result<()> {
    let arguments: Vec<_> = arguments.into_iter().collect();
    let output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|error| PipelineError::io(format!("open {}", log.display()), error))?;
    let errors = output
        .try_clone()
        .map_err(|error| PipelineError::io(format!("clone {}", log.display()), error))?;
    let status = Command::new(program)
        .args(&arguments)
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(errors))
        .status()
        .map_err(|error| PipelineError::io(format!("execute {}", program.display()), error))?;
    if status.success() {
        Ok(())
    } else {
        Err(command_failed(program.as_os_str(), &arguments, status))
    }
}

fn command_failed(program: &OsStr, arguments: &[OsString], status: ExitStatus) -> PipelineError {
    let mut command = program.to_string_lossy().into_owned();
    for argument in arguments {
        command.push(' ');
        command.push_str(&argument.to_string_lossy());
    }
    PipelineError::CommandFailed {
        command,
        status: status.to_string(),
    }
}
