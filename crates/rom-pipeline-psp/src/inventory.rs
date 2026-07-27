use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rom_pipeline_core::{
    ComponentKind, Job, PipelineError, Result, SourceArtifact, modified_seconds, sha256_file,
};
use sha2::{Digest, Sha256};

use crate::format::{PspIdentity, inspect_iso};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PspInventory {
    pub jobs: Vec<Job>,
}

#[derive(Clone, Debug)]
struct ScannedIso {
    name: String,
    path: PathBuf,
    size: u64,
    identity: PspIdentity,
    modified_seconds: u64,
    cached_hash: Option<String>,
    cache_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct Variant {
    disc_id: String,
    title: String,
    sources: Vec<ScannedIso>,
}

impl PspInventory {
    /// Inventories and validates PSP ISO files from source and done folders.
    ///
    /// Exact copies with the same embedded disc ID are grouped into one job.
    /// Different images that reuse a disc ID remain separate jobs.
    ///
    /// # Errors
    ///
    /// Returns an error for conflicting filenames, malformed PSP images, or
    /// unreadable duplicate candidates.
    pub fn scan(
        source: &Path,
        done: &Path,
        cache_dir: &Path,
        only_job: Option<&str>,
    ) -> Result<Self> {
        let mut paths = BTreeMap::<String, PathBuf>::new();
        collect(source, &mut paths)?;
        collect(done, &mut paths)?;
        let mut scanned = Vec::with_capacity(paths.len());
        for (name, path) in paths {
            scanned.push(inspect_cached(name, path, cache_dir)?);
        }
        let variants = group_variants(scanned)?;
        Ok(Self {
            jobs: jobs_from_variants(variants, only_job),
        })
    }
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
            || !is_iso(&path)
        {
            continue;
        }
        let name = entry.file_name().into_string().map_err(|_| {
            PipelineError::Message(format!("invalid UTF-8 filename: {}", path.display()))
        })?;
        if let Some(existing) = files.insert(name.clone(), path.clone()) {
            return Err(PipelineError::Message(format!(
                "PSP ISO exists in source and done: {name} ({}, {})",
                existing.display(),
                path.display()
            )));
        }
    }
    Ok(())
}

fn inspect_cached(name: String, path: PathBuf, cache_dir: &Path) -> Result<ScannedIso> {
    let metadata = fs::metadata(&path)
        .map_err(|error| PipelineError::io(format!("stat {}", path.display()), error))?;
    let size = metadata.len();
    let modified_seconds = modified_seconds(&path)?;
    let cache_path = cache_dir.join(format!("{}.tsv", sha256_name(&name)));
    if let Some((identity, cached_hash)) = read_cache(&cache_path, size, modified_seconds)? {
        return Ok(ScannedIso {
            name,
            path,
            size,
            identity,
            modified_seconds,
            cached_hash,
            cache_path: Some(cache_path),
        });
    }
    let identity = inspect_iso(&path)?;
    let image = ScannedIso {
        name,
        path,
        size,
        identity,
        modified_seconds,
        cached_hash: None,
        cache_path: Some(cache_path),
    };
    write_cache(&image)?;
    Ok(image)
}

fn ensure_hash(image: &mut ScannedIso) -> Result<String> {
    if let Some(hash) = &image.cached_hash {
        return Ok(hash.clone());
    }
    let hash = sha256_file(&image.path)?;
    image.cached_hash = Some(hash.clone());
    write_cache(image)?;
    Ok(hash)
}

fn read_cache(
    path: &Path,
    size: u64,
    modified_seconds: u64,
) -> Result<Option<(PspIdentity, Option<String>)>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(PipelineError::io(format!("read {}", path.display()), error)),
    };
    let fields: Vec<_> = text.trim_end().split('\t').collect();
    if !(3..=4).contains(&fields.len())
        || fields[0].parse::<u64>().ok() != Some(size)
        || fields[1].parse::<u64>().ok() != Some(modified_seconds)
        || !valid_cached_disc_id(fields[2])
        || fields.get(3).is_some_and(|hash| {
            hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Ok(None);
    }
    Ok(Some((
        PspIdentity {
            disc_id: fields[2].to_owned(),
            title: fields[2].to_owned(),
        },
        fields.get(3).map(|hash| (*hash).to_owned()),
    )))
}

fn write_cache(image: &ScannedIso) -> Result<()> {
    let Some(path) = &image.cache_path else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| PipelineError::Message("PSP cache path has no parent".to_owned()))?;
    fs::create_dir_all(parent)
        .map_err(|error| PipelineError::io(format!("create {}", parent.display()), error))?;
    let hash = image
        .cached_hash
        .as_ref()
        .map_or_else(String::new, |hash| format!("\t{hash}"));
    let content = format!(
        "{}\t{}\t{}{}\n",
        image.size, image.modified_seconds, image.identity.disc_id, hash
    );
    let temporary = path.with_extension("new");
    fs::write(&temporary, content)
        .map_err(|error| PipelineError::io(format!("write {}", temporary.display()), error))?;
    fs::rename(&temporary, path)
        .map_err(|error| PipelineError::io(format!("publish {}", path.display()), error))
}

fn valid_cached_disc_id(value: &str) -> bool {
    value.len() == 9
        && value[..4].bytes().all(|byte| byte.is_ascii_uppercase())
        && value[4..].bytes().all(|byte| byte.is_ascii_digit())
}

fn group_variants(scanned: Vec<ScannedIso>) -> Result<Vec<Variant>> {
    let mut by_disc_id = BTreeMap::<String, Vec<ScannedIso>>::new();
    for image in scanned {
        by_disc_id
            .entry(image.identity.disc_id.clone())
            .or_default()
            .push(image);
    }

    let mut variants = Vec::new();
    for (disc_id, images) in by_disc_id {
        let mut by_size = BTreeMap::<u64, Vec<ScannedIso>>::new();
        for image in images {
            by_size.entry(image.size).or_default().push(image);
        }
        for same_size in by_size.into_values() {
            if same_size.len() == 1 {
                variants.push(make_variant(disc_id.clone(), same_size));
                continue;
            }
            let mut by_hash = BTreeMap::<String, Vec<ScannedIso>>::new();
            for mut image in same_size {
                let hash = ensure_hash(&mut image)?;
                by_hash.entry(hash).or_default().push(image);
            }
            for exact_copies in by_hash.into_values() {
                variants.push(make_variant(disc_id.clone(), exact_copies));
            }
        }
    }
    variants.sort_by(|left, right| {
        left.disc_id
            .cmp(&right.disc_id)
            .then_with(|| left.sources[0].name.cmp(&right.sources[0].name))
    });
    Ok(variants)
}

fn make_variant(disc_id: String, mut sources: Vec<ScannedIso>) -> Variant {
    sources.sort_by(canonical_source_order);
    let title = sources[0].identity.title.clone();
    Variant {
        disc_id,
        title,
        sources,
    }
}

fn canonical_source_order(left: &ScannedIso, right: &ScannedIso) -> Ordering {
    right
        .name
        .len()
        .cmp(&left.name.len())
        .then_with(|| left.name.cmp(&right.name))
}

fn jobs_from_variants(variants: Vec<Variant>, only_job: Option<&str>) -> Vec<Job> {
    let mut variants_per_id = BTreeMap::<String, usize>::new();
    for variant in &variants {
        *variants_per_id.entry(variant.disc_id.clone()).or_default() += 1;
    }
    let mut output_names = BTreeMap::<String, usize>::new();
    let only_job = only_job.map(str::to_ascii_uppercase);
    let mut jobs = Vec::with_capacity(variants.len());
    for variant in variants {
        let canonical_name = &variant.sources[0].name;
        let stem = Path::new(canonical_name)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(&variant.title);
        let collision = output_names.entry(stem.to_ascii_lowercase()).or_default();
        *collision += 1;
        let output_name = if *collision == 1 {
            format!("{stem}.chd")
        } else {
            format!("{stem}-{collision}.chd")
        };
        let id = if variants_per_id[&variant.disc_id] == 1 {
            variant.disc_id.clone()
        } else {
            format!("{}-{}", variant.disc_id, &sha256_name(canonical_name)[..8])
        };
        if only_job.as_ref().is_some_and(|only| only != &id) {
            continue;
        }
        jobs.push(Job {
            id,
            display_name: stem.to_owned(),
            output_name,
            sources: variant
                .sources
                .into_iter()
                .map(|image| SourceArtifact {
                    title_id: variant.disc_id.clone(),
                    expected_size: image.size,
                    name: image.name,
                    role: ComponentKind::Base,
                })
                .collect(),
        });
    }
    jobs
}

fn is_iso(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("iso"))
}

fn sha256_name(name: &str) -> String {
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
    use super::{ScannedIso, group_variants, jobs_from_variants};
    use crate::format::PspIdentity;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn exact_copies_share_one_job_and_use_the_descriptive_name() {
        let first = fixture_path("short");
        let second = fixture_path("descriptive");
        fs::write(&first, b"same image").expect("write first");
        fs::write(&second, b"same image").expect("write second");
        let identity = PspIdentity {
            disc_id: "ULUS10055".to_owned(),
            title: "Pac-Man World 3".to_owned(),
        };
        let scanned = vec![
            ScannedIso {
                name: "Pac-Man World 3 (US).iso".to_owned(),
                path: first.clone(),
                size: 10,
                identity: identity.clone(),
                modified_seconds: 0,
                cached_hash: None,
                cache_path: None,
            },
            ScannedIso {
                name: "Pac-Man World 3 (USA) (En,Fr).iso".to_owned(),
                path: second.clone(),
                size: 10,
                identity,
                modified_seconds: 0,
                cached_hash: None,
                cache_path: None,
            },
        ];
        let jobs = jobs_from_variants(group_variants(scanned).expect("group"), None);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].sources.len(), 2);
        assert_eq!(jobs[0].output_name, "Pac-Man World 3 (USA) (En,Fr).chd");
        fs::remove_file(first).expect("remove first");
        fs::remove_file(second).expect("remove second");
    }

    #[test]
    fn different_images_with_one_disc_id_remain_separate() {
        let first = fixture_path("revision-a");
        let second = fixture_path("revision-b");
        fs::write(&first, b"revision a").expect("write first");
        fs::write(&second, b"revision b is larger").expect("write second");
        let identity = PspIdentity {
            disc_id: "ULUS10055".to_owned(),
            title: "Pac-Man World 3".to_owned(),
        };
        let scanned = vec![
            ScannedIso {
                name: "Game.iso".to_owned(),
                path: first.clone(),
                size: 10,
                identity: identity.clone(),
                modified_seconds: 0,
                cached_hash: None,
                cache_path: None,
            },
            ScannedIso {
                name: "Game Revised.iso".to_owned(),
                path: second.clone(),
                size: 20,
                identity,
                modified_seconds: 0,
                cached_hash: None,
                cache_path: None,
            },
        ];
        let jobs = jobs_from_variants(group_variants(scanned).expect("group"), None);
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().all(|job| job.id.starts_with("ULUS10055-")));
        fs::remove_file(first).expect("remove first");
        fs::remove_file(second).expect("remove second");
    }

    fn fixture_path(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rom-pipeline-psp-inventory-{label}-{}-{nonce}.iso",
            std::process::id()
        ))
    }
}
