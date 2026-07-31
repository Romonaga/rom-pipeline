use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use rom_pipeline_core::{PipelineError, Result};

pub fn run_logged(program: &Path, args: &[OsString], log: &Path) -> Result<()> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| PipelineError::io(format!("create {}", parent.display()), error))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|error| PipelineError::io(format!("open {}", log.display()), error))?;
    writeln!(file, "command={} {:?}", program.display(), args)
        .map_err(|error| PipelineError::io(format!("write {}", log.display()), error))?;
    let stdout = file
        .try_clone()
        .map_err(|error| PipelineError::io(format!("clone {}", log.display()), error))?;
    let stderr = file
        .try_clone()
        .map_err(|error| PipelineError::io(format!("clone {}", log.display()), error))?;
    let status = Command::new(program)
        .args(args)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .map_err(|error| PipelineError::io(format!("run {}", program.display()), error))?;
    if status.success() {
        Ok(())
    } else {
        Err(PipelineError::CommandFailed {
            command: format!("{} {:?}", program.display(), args),
            status: status.to_string(),
        })
    }
}
