use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rom_pipeline_core::{ComponentKind, Job, PipelineError, Result, SourceArtifact};

use crate::format::identify_cci;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Nintendo3dsInventory {
    pub jobs: Vec<Job>,
}

impl Nintendo3dsInventory {
    /// Inventories CCI/3DS files from source and done directories.
    ///
    /// # Errors
    ///
    /// Returns an error for conflicting filenames or malformed CCI images.
    pub fn scan(source: &Path, done: &Path, only_job: Option<&str>) -> Result<Self> {
        let mut files = BTreeMap::<String, PathBuf>::new();
        collect(source, &mut files)?;
        collect(done, &mut files)?;
        let only_job = only_job.map(str::to_ascii_uppercase);
        let mut duplicate_names = BTreeMap::<String, usize>::new();
        let mut jobs = Vec::with_capacity(files.len());
        for (name, path) in files {
            let identity = identify_cci(&path)?;
            let name_hash = sha256_file_name(&name);
            let id = format!("{}-{}", identity.title_id, &name_hash[..8]);
            if only_job.as_ref().is_some_and(|only| only != &id) {
                continue;
            }
            let expected_size = fs::metadata(&path)
                .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
                .len();
            let source_stem = Path::new(&name)
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    PipelineError::Message(format!("invalid UTF-8 filename: {}", path.display()))
                })?;
            let display_name = clean_display_name(source_stem);
            let duplicate_number = duplicate_names.entry(display_name.clone()).or_default();
            *duplicate_number += 1;
            let output_name = if *duplicate_number == 1 {
                format!("{display_name}.cci")
            } else {
                format!("{display_name}-{duplicate_number}.cci")
            };
            jobs.push(Job {
                id,
                display_name,
                output_name,
                sources: vec![SourceArtifact {
                    title_id: identity.title_id,
                    expected_size,
                    name,
                    role: ComponentKind::Base,
                }],
            });
        }
        Ok(Self { jobs })
    }
}

fn clean_display_name(stem: &str) -> String {
    stem.strip_suffix(" (Decrypted)")
        .or_else(|| stem.strip_suffix(" Decrypted"))
        .unwrap_or(stem)
        .to_owned()
}

fn collect(root: &Path, files: &mut BTreeMap<String, PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(PipelineError::io(format!("read {}", root.display()), error)),
    };
    for entry in entries {
        let entry =
            entry.map_err(|error| PipelineError::io(format!("read {}", root.display()), error))?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
            .is_file()
            || !is_cci(&path)
        {
            continue;
        }
        let name = entry.file_name().into_string().map_err(|_| {
            PipelineError::Message(format!("invalid UTF-8 filename: {}", path.display()))
        })?;
        if let Some(existing) = files.insert(name.clone(), path.clone()) {
            return Err(PipelineError::Message(format!(
                "3DS image exists in source and done: {name} ({}, {})",
                existing.display(),
                path.display()
            )));
        }
    }
    Ok(())
}

fn is_cci(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("3ds") || extension.eq_ignore_ascii_case("cci")
    })
}

fn sha256_file_name(name: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(name.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::clean_display_name;

    #[test]
    fn removes_redundant_decrypted_suffix() {
        assert_eq!(clean_display_name("Game Decrypted"), "Game");
        assert_eq!(clean_display_name("Game (Decrypted)"), "Game");
        assert_eq!(clean_display_name("Game"), "Game");
    }
}
