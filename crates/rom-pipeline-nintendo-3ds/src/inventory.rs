use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rom_pipeline_core::{ComponentKind, Job, PipelineError, Result, SourceArtifact};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Nintendo3dsInventory {
    pub jobs: Vec<Job>,
}

impl Nintendo3dsInventory {
    /// Builds stable ZIP jobs from the Archive.org downloader manifest.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest is unreadable or malformed.
    pub fn from_manifest(manifest: &Path, only_job: Option<&str>) -> Result<Self> {
        let text = fs::read_to_string(manifest)
            .map_err(|error| PipelineError::io(format!("read {}", manifest.display()), error))?;
        let only = only_job.map(str::to_ascii_uppercase);
        let mut entries = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let fields: Vec<_> = line.split('\t').collect();
            if fields.len() < 2 {
                return Err(PipelineError::Message(format!(
                    "invalid 3DS manifest line {}",
                    index + 1
                )));
            }
            let expected_size = fields[0].parse::<u64>().map_err(|_| {
                PipelineError::Message(format!("invalid 3DS size on line {}", index + 1))
            })?;
            let archive_name = fields[fields.len() - 1].to_owned();
            if !archive_name.to_ascii_lowercase().ends_with(".zip") {
                continue;
            }
            entries.push((archive_name, expected_size));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let mut duplicate_names = BTreeMap::<String, usize>::new();
        let mut jobs = Vec::with_capacity(entries.len());
        for (name, expected_size) in entries {
            let display_name = Path::new(&name)
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| PipelineError::Message(format!("invalid ZIP filename: {name}")))?
                .to_owned();
            let duplicate = duplicate_names.entry(display_name.clone()).or_default();
            *duplicate += 1;
            let output_name = if *duplicate == 1 {
                format!("{display_name}.cia")
            } else {
                format!("{display_name}-{duplicate}.cia")
            };
            let id = format!("3DS-{}", &sha256_name(&name)[..12]);
            if only.as_ref().is_some_and(|wanted| wanted != &id) {
                continue;
            }
            jobs.push(Job {
                id,
                display_name,
                output_name,
                sources: vec![SourceArtifact {
                    title_id: "archive".to_owned(),
                    expected_size,
                    name,
                    role: ComponentKind::Base,
                }],
            });
        }
        Ok(Self { jobs })
    }
}

fn sha256_name(name: &str) -> String {
    let digest = Sha256::digest(name.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(encoded, "{byte:02X}").expect("writing to a String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::Nintendo3dsInventory;

    #[test]
    fn manifest_inventory_is_stable_and_filters_non_zip_files() {
        let path = fixture_path();
        fs::write(&path, "20\tB.zip\n10\tA.zip\n2\treadme.txt\n").expect("write");
        let inventory = Nintendo3dsInventory::from_manifest(&path, None).expect("inventory");
        assert_eq!(inventory.jobs.len(), 2);
        assert_eq!(inventory.jobs[0].output_name, "A.cia");
        assert_eq!(inventory.jobs[1].output_name, "B.cia");
        fs::remove_file(path).expect("remove");
    }

    fn fixture_path() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("rom-pipeline-3ds-manifest-{nonce}.tsv"))
    }
}
