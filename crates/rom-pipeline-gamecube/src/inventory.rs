use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use rom_pipeline_core::{ComponentKind, Job, PipelineError, Result, SourceArtifact};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GameCubeInventory {
    pub jobs: Vec<Job>,
}

impl GameCubeInventory {
    /// Builds deterministic `GameCube` jobs from the downloader manifest.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed entries, unsafe names, duplicate source
    /// names, invalid sizes, or a selected job that cannot be found.
    pub fn from_manifest(manifest: &Path, only_job: Option<&str>) -> Result<Self> {
        let text = fs::read_to_string(manifest)
            .map_err(|error| PipelineError::io(format!("read {}", manifest.display()), error))?;
        let mut seen = BTreeSet::new();
        let mut duplicate_names = BTreeMap::<String, usize>::new();
        let mut jobs = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let Some((size, name)) = line.split_once('\t') else {
                if line.trim().is_empty() {
                    continue;
                }
                return Err(PipelineError::Message(format!(
                    "GameCube manifest line {} has no tab separator",
                    index + 1
                )));
            };
            if !is_iso_name(name) {
                continue;
            }
            if !is_safe_file_name(name) {
                return Err(PipelineError::Message(format!(
                    "GameCube manifest entry is not a plain filename: {name}"
                )));
            }
            if !seen.insert(name.to_owned()) {
                return Err(PipelineError::Message(format!(
                    "duplicate GameCube manifest entry: {name}"
                )));
            }
            let expected_size = size.parse::<u64>().map_err(|_| {
                PipelineError::Message(format!(
                    "invalid GameCube size on manifest line {}",
                    index + 1
                ))
            })?;
            if expected_size == 0 {
                return Err(PipelineError::Message(format!(
                    "GameCube manifest line {} has a zero-sized image",
                    index + 1
                )));
            }
            let id = format!("GC-{}", &sha256_name(name)[..12]);
            let display_name = Path::new(name)
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    PipelineError::Message(format!("invalid GameCube filename: {name}"))
                })?
                .to_owned();
            let count = duplicate_names
                .entry(display_name.to_ascii_lowercase())
                .or_default();
            *count += 1;
            let output_name = if *count == 1 {
                format!("{display_name}.rvz")
            } else {
                format!("{display_name}-{}.rvz", *count)
            };
            jobs.push(Job {
                id,
                display_name,
                output_name,
                sources: vec![SourceArtifact {
                    title_id: sha256_name(name),
                    expected_size,
                    name: name.to_owned(),
                    role: ComponentKind::Base,
                }],
            });
        }
        if let Some(only) = only_job {
            jobs.retain(|job| only.eq_ignore_ascii_case(&job.id));
            if jobs.is_empty() {
                return Err(PipelineError::Message(format!(
                    "unknown GameCube group: {only}"
                )));
            }
        }
        Ok(Self { jobs })
    }
}

fn is_iso_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("iso"))
}

fn is_safe_file_name(name: &str) -> bool {
    let path = Path::new(name);
    path.file_name().is_some_and(|file_name| file_name == name) && path.components().count() == 1
}

fn sha256_name(name: &str) -> String {
    let digest = Sha256::digest(name.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(encoded, "{byte:02X}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::GameCubeInventory;

    #[test]
    fn inventories_only_iso_images_with_stable_ids() {
        let root = fixture_path();
        fs::create_dir_all(&root).expect("root");
        let manifest = root.join("manifest.tsv");
        fs::write(&manifest, "1459978240\tGame.iso\n12\tmetadata.xml\n").expect("manifest");

        let inventory = GameCubeInventory::from_manifest(&manifest, None).expect("inventory");

        assert_eq!(inventory.jobs.len(), 1);
        assert!(inventory.jobs[0].id.starts_with("GC-"));
        assert_eq!(inventory.jobs[0].output_name, "Game.rvz");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn only_filter_preserves_duplicate_output_numbering() {
        let root = fixture_path();
        fs::create_dir_all(&root).expect("root");
        let manifest = root.join("manifest.tsv");
        fs::write(&manifest, "100\tGame.iso\n200\tGame.ISO\n").expect("manifest");
        let all = GameCubeInventory::from_manifest(&manifest, None).expect("all inventory");

        let selected =
            GameCubeInventory::from_manifest(&manifest, Some(&all.jobs[1].id)).expect("selected");

        assert_eq!(selected.jobs[0].output_name, "Game-2.rvz");
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn fixture_path() -> std::path::PathBuf {
        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
        let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "rom-pipeline-gamecube-inventory-{}-{nonce}",
            std::process::id()
        ))
    }
}
