use std::fs;
use std::path::{Path, PathBuf};

use rom_pipeline_core::{PipelineError, Result};

pub fn clear_owned_directory(path: &Path, allowed_root: &Path) -> Result<()> {
    if path == allowed_root || !path.starts_with(allowed_root) {
        return Err(PipelineError::Message(format!(
            "refusing to clear unsafe path: {}",
            path.display()
        )));
    }
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PipelineError::io(
            format!("clear {}", path.display()),
            error,
        )),
    }
}

pub fn find_named(root: &Path, wanted: &str) -> Result<Vec<PathBuf>> {
    let mut matches = Vec::new();
    visit(root, &mut |path, file_type| {
        if file_type.is_file() && path.file_name().is_some_and(|name| name == wanted) {
            matches.push(path.to_path_buf());
        }
        Ok(())
    })?;
    Ok(matches)
}

pub fn contains_symlink(root: &Path) -> Result<bool> {
    let mut found = false;
    visit(root, &mut |_path, file_type| {
        found |= file_type.is_symlink();
        Ok(())
    })?;
    Ok(found)
}

fn visit(root: &Path, visitor: &mut impl FnMut(&Path, fs::FileType) -> Result<()>) -> Result<()> {
    for entry in fs::read_dir(root)
        .map_err(|error| PipelineError::io(format!("read {}", root.display()), error))?
    {
        let entry =
            entry.map_err(|error| PipelineError::io(format!("read {}", root.display()), error))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?;
        visitor(&path, file_type)?;
        if file_type.is_dir() {
            visit(&path, visitor)?;
        }
    }
    Ok(())
}
