use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use rom_pipeline_core::{ComponentKind, Job, PipelineError, Result, SourceArtifact};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VitaInventory {
    pub jobs: Vec<Job>,
}

impl VitaInventory {
    /// Inventories `NoNpDRM` ZIP archives using the title ID embedded in each filename.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable sources, duplicate title IDs, malformed
    /// filenames, or a requested title that does not exist.
    pub fn from_directory(root: &Path, only_job: Option<&str>) -> Result<Self> {
        let entries = fs::read_dir(root)
            .map_err(|error| PipelineError::io(format!("read {}", root.display()), error))?;
        let mut jobs = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(|error| PipelineError::io(format!("read {}", root.display()), error))?;
            let path = entry.path();
            if !entry
                .file_type()
                .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
                .is_file()
                || path
                    .extension()
                    .is_none_or(|extension| !extension.eq_ignore_ascii_case("zip"))
            {
                continue;
            }
            let name = entry.file_name().into_string().map_err(|_| {
                PipelineError::Message(format!(
                    "Vita ZIP filename is not UTF-8: {}",
                    path.display()
                ))
            })?;
            let title_id = title_id_from_filename(&name).ok_or_else(|| {
                PipelineError::Message(format!("Vita ZIP has no title ID: {name}"))
            })?;
            let id = format!("VITA-{title_id}");
            if only_job.is_some_and(|wanted| !wanted.eq_ignore_ascii_case(&id)) {
                continue;
            }
            let expected_size = fs::metadata(&path)
                .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?
                .len();
            let display_name = name
                .split_once(&format!("[{title_id}]"))
                .map_or(name.trim_end_matches(".zip"), |(prefix, _)| prefix.trim())
                .to_owned();
            jobs.push(Job {
                id,
                display_name,
                output_name: title_id.clone(),
                sources: vec![SourceArtifact {
                    title_id,
                    expected_size,
                    name,
                    role: ComponentKind::Base,
                }],
            });
        }
        jobs.sort_by(|left, right| {
            left.display_name
                .to_ascii_lowercase()
                .cmp(&right.display_name.to_ascii_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut ids = BTreeSet::new();
        if let Some(duplicate) = jobs.iter().find(|job| !ids.insert(job.id.clone())) {
            return Err(PipelineError::Message(format!(
                "duplicate Vita title ID: {}",
                duplicate.id
            )));
        }
        if only_job.is_some() && jobs.is_empty() {
            return Err(PipelineError::Message(format!(
                "requested Vita title was not found: {}",
                only_job.unwrap_or_default()
            )));
        }
        Ok(Self { jobs })
    }
}

fn title_id_from_filename(name: &str) -> Option<String> {
    name.split('[')
        .skip(1)
        .filter_map(|suffix| suffix.split_once(']').map(|(value, _)| value))
        .find(|value| {
            value.len() == 9
                && value.starts_with("PCS")
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::VitaInventory;

    #[test]
    fn inventories_unique_titles_and_supports_exact_selection() {
        let root = fixture_dir();
        fs::create_dir_all(&root).expect("create fixture");
        fs::write(root.join("Second [PCSE00002] [USA] [NoNpDRM].zip"), b"two")
            .expect("write second");
        fs::write(root.join("First [PCSA00001] [USA] [NoNpDRM].zip"), b"one").expect("write first");
        fs::write(root.join("metadata.xml"), b"ignored").expect("write metadata");

        let inventory = VitaInventory::from_directory(&root, None).expect("inventory");
        assert_eq!(inventory.jobs.len(), 2);
        assert_eq!(inventory.jobs[0].id, "VITA-PCSA00001");
        let selected = VitaInventory::from_directory(&root, Some("vita-pcse00002"))
            .expect("selected inventory");
        assert_eq!(selected.jobs.len(), 1);
        assert_eq!(selected.jobs[0].display_name, "Second");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    fn fixture_dir() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("rom-pipeline-vita-inventory-{nonce}"))
    }
}
