use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use rom_pipeline_core::{ComponentKind, Job, PipelineError, Result, SourceArtifact};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Ps2Inventory {
    pub jobs: Vec<Job>,
}

impl Ps2Inventory {
    /// Builds deterministic PS2 jobs from the downloader's size/name manifest.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed entries or duplicate source names.
    pub fn from_manifest(
        manifest: &Path,
        _source: &Path,
        _done: &Path,
        only_job: Option<&str>,
    ) -> Result<Self> {
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
                    "PS2 manifest line {} has no tab separator",
                    index + 1
                )));
            };
            if !is_disc_name(name) {
                continue;
            }
            if !is_safe_file_name(name) {
                return Err(PipelineError::Message(format!(
                    "PS2 manifest entry is not a plain filename: {name}"
                )));
            }
            if !seen.insert(name.to_owned()) {
                return Err(PipelineError::Message(format!(
                    "duplicate PS2 manifest entry: {name}"
                )));
            }
            let expected_size = size.parse::<u64>().map_err(|_| {
                PipelineError::Message(format!("invalid PS2 size on manifest line {}", index + 1))
            })?;
            if expected_size == 0 {
                return Err(PipelineError::Message(format!(
                    "PS2 manifest line {} has a zero-sized disc image",
                    index + 1
                )));
            }
            let id = format!("PS2-{}", &sha256_name(name)[..12]);
            let display_name = Path::new(name)
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| PipelineError::Message(format!("invalid PS2 filename: {name}")))?
                .to_owned();
            let count = duplicate_names
                .entry(display_name.to_ascii_lowercase())
                .or_default();
            *count += 1;
            let output_name = if *count == 1 {
                format!("{display_name}.chd")
            } else {
                format!("{display_name}-{}.chd", *count)
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
        }
        Ok(Self { jobs })
    }
}

fn is_disc_name(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("iso") || extension.eq_ignore_ascii_case("bin")
    })
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::Ps2Inventory;

    #[test]
    fn inventories_only_disc_images_with_stable_ids() {
        let root = fixture_path();
        let source = root.join("source");
        let done = root.join("done");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&done).expect("done");
        let manifest = root.join("manifest.tsv");
        fs::write(
            &manifest,
            "100\tGame.iso\n200\tDisc.bin\n12\tmetadata.xml\n",
        )
        .expect("manifest");
        let inventory =
            Ps2Inventory::from_manifest(&manifest, &source, &done, None).expect("inventory");
        assert_eq!(inventory.jobs.len(), 2);
        assert!(inventory.jobs[0].id.starts_with("PS2-"));
        assert_eq!(inventory.jobs[0].output_name, "Game.chd");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn only_filter_preserves_duplicate_output_numbering() {
        let root = fixture_path();
        let source = root.join("source");
        let done = root.join("done");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&done).expect("done");
        let manifest = root.join("manifest.tsv");
        fs::write(&manifest, "100\tGame.iso\n200\tGame.bin\n").expect("manifest");
        let all =
            Ps2Inventory::from_manifest(&manifest, &source, &done, None).expect("all inventory");
        let selected =
            Ps2Inventory::from_manifest(&manifest, &source, &done, Some(&all.jobs[1].id))
                .expect("selected inventory");
        assert_eq!(selected.jobs[0].output_name, "Game-2.chd");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn inventory_allows_published_iso_alongside_done_source() {
        let root = fixture_path();
        let source = root.join("source");
        let done = root.join("done");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&done).expect("done");
        fs::write(source.join("Game.iso"), b"published").expect("published output");
        fs::write(done.join("Game.iso"), b"original").expect("done source");
        let manifest = root.join("manifest.tsv");
        fs::write(&manifest, "8\tGame.iso\n").expect("manifest");

        let inventory =
            Ps2Inventory::from_manifest(&manifest, &source, &done, None).expect("inventory");

        assert_eq!(inventory.jobs.len(), 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn fixture_path() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rom-pipeline-ps2-inventory-{}-{nonce}",
            std::process::id()
        ))
    }
}
