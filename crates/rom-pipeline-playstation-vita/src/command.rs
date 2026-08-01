use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use rom_pipeline_core::{PipelineError, Result};

pub fn run_logged(program: &Path, args: &[OsString], log: &Path) -> Result<()> {
    let mut file = log_file(log)?;
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

pub fn capture(program: &Path, args: &[OsString], log: &Path) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| PipelineError::io(format!("run {}", program.display()), error))?;
    let mut file = log_file(log)?;
    writeln!(file, "command={} {:?}", program.display(), args)
        .map_err(|error| PipelineError::io(format!("write {}", log.display()), error))?;
    file.write_all(&output.stderr)
        .map_err(|error| PipelineError::io(format!("write {}", log.display()), error))?;
    if !output.status.success() {
        return Err(PipelineError::CommandFailed {
            command: format!("{} {:?}", program.display(), args),
            status: output.status.to_string(),
        });
    }
    String::from_utf8(output.stdout)
        .map_err(|_| PipelineError::Message("7-Zip emitted non-UTF-8 output".to_owned()))
}

fn log_file(path: &Path) -> Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| PipelineError::io(format!("create {}", parent.display()), error))?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| PipelineError::io(format!("open {}", path.display()), error))
}
