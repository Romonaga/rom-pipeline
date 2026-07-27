use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rom_pipeline_core::{ComponentKind, Job, PipelineError, Result, SourceArtifact};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WiiUInventory {
    pub jobs: Vec<Job>,
}

impl WiiUInventory {
    /// Reads a Wii U Archive.org manifest and groups base, update, and DLC
    /// archives by the low eight hexadecimal digits of their title IDs.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be read or contains malformed
    /// Wii U archive entries.
    pub fn from_manifest(path: &Path, only_job: Option<&str>) -> Result<Self> {
        let text = fs::read_to_string(path)
            .map_err(|error| PipelineError::io(format!("read {}", path.display()), error))?;
        Self::parse(&text, only_job)
    }

    fn parse(text: &str, only_job: Option<&str>) -> Result<Self> {
        let only_job = only_job.map(str::to_ascii_uppercase);
        let mut groups: BTreeMap<String, Vec<SourceArtifact>> = BTreeMap::new();

        for (index, line) in text.lines().enumerate() {
            let Some((size, name)) = line.split_once('\t') else {
                if line.trim().is_empty() {
                    continue;
                }
                return Err(PipelineError::Message(format!(
                    "manifest line {} has no tab separator",
                    index + 1
                )));
            };
            if !name.to_ascii_lowercase().ends_with(".7z") {
                continue;
            }
            let expected_size = size.parse::<u64>().map_err(|_| {
                PipelineError::Message(format!("invalid size on manifest line {}", index + 1))
            })?;
            let title_id = title_id_from_name(name).ok_or_else(|| {
                PipelineError::Message(format!("no Wii U title ID in archive name: {name}"))
            })?;
            let group = title_id[8..].to_owned();
            if only_job.as_ref().is_some_and(|only| only != &group) {
                continue;
            }
            groups.entry(group).or_default().push(SourceArtifact {
                role: component_kind(&title_id, name)?,
                title_id,
                expected_size,
                name: name.to_owned(),
            });
        }

        let mut duplicate_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut jobs = Vec::with_capacity(groups.len());
        for (id, mut sources) in groups {
            sources.sort_by(|left, right| {
                (left.role, &left.title_id, &left.name).cmp(&(
                    right.role,
                    &right.title_id,
                    &right.name,
                ))
            });
            validate_components(&id, &sources)?;
            let display_name = display_name(&sources)?;
            let duplicate_number = duplicate_counts.entry(display_name.clone()).or_default();
            *duplicate_number += 1;
            let output_name = if *duplicate_number == 1 {
                format!("{display_name}.wua")
            } else {
                format!("{display_name}-{duplicate_number}.wua")
            };
            jobs.push(Job {
                id,
                display_name,
                output_name,
                sources,
            });
        }

        Ok(Self { jobs })
    }
}

fn component_kind(title_id: &str, name: &str) -> Result<ComponentKind> {
    if name.contains("[UPDATE ") {
        return Ok(ComponentKind::Update);
    }
    if name.contains("[DLC]") {
        return Ok(ComponentKind::Dlc);
    }
    match &title_id[..8] {
        "00050000" => Ok(ComponentKind::Base),
        "0005000E" => Ok(ComponentKind::Update),
        "0005000C" => Ok(ComponentKind::Dlc),
        prefix => Err(PipelineError::Message(format!(
            "unsupported Wii U title type {prefix}: {name}"
        ))),
    }
}

fn validate_components(group: &str, sources: &[SourceArtifact]) -> Result<()> {
    let counts = |role| sources.iter().filter(|source| source.role == role).count();
    let base = counts(ComponentKind::Base);
    let update = counts(ComponentKind::Update);
    let dlc = counts(ComponentKind::Dlc);
    if base != 1 || update > 1 || dlc > 1 {
        return Err(PipelineError::Message(format!(
            "invalid component set group={group} base={base} update={update} dlc={dlc}"
        )));
    }
    Ok(())
}

fn title_id_from_name(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    for start in 0..bytes.len().saturating_sub(17) {
        if bytes[start] != b'[' || bytes.get(start + 17) != Some(&b']') {
            continue;
        }
        let candidate = &name[start + 1..start + 17];
        if candidate.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Some(candidate.to_ascii_uppercase());
        }
    }
    None
}

fn display_name(sources: &[SourceArtifact]) -> Result<String> {
    let preferred = sources
        .iter()
        .find(|source| source.role == ComponentKind::Base)
        .or_else(|| sources.first())
        .ok_or_else(|| PipelineError::Message("Wii U group has no sources".to_owned()))?;
    let marker = format!(" [{}]", preferred.title_id);
    let end = preferred.name.find(&marker).ok_or_else(|| {
        PipelineError::Message(format!(
            "title ID marker missing from archive name: {}",
            preferred.name
        ))
    })?;
    Ok(preferred.name[..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::WiiUInventory;
    use rom_pipeline_core::ComponentKind;

    #[test]
    fn groups_base_update_and_dlc() {
        let manifest = "\
100\tGame [0005000012345600].7z
20\tGame [0005000E12345600] [UPDATE v16].7z
10\tGame [0005000C12345600] [DLC].7z
";
        let inventory = WiiUInventory::parse(manifest, None).expect("inventory");
        assert_eq!(inventory.jobs.len(), 1);
        let job = &inventory.jobs[0];
        assert_eq!(job.id, "12345600");
        assert_eq!(job.display_name, "Game");
        assert_eq!(job.output_name, "Game.wua");
        assert_eq!(job.sources.len(), 3);
        assert_eq!(job.component_count(ComponentKind::Base), 1);
        assert_eq!(job.component_count(ComponentKind::Update), 1);
        assert_eq!(job.component_count(ComponentKind::Dlc), 1);
    }

    #[test]
    fn duplicate_display_names_get_numeric_suffixes() {
        let manifest = "\
100\tSame Game [0005000011111100].7z
100\tSame Game [0005000022222200].7z
";
        let inventory = WiiUInventory::parse(manifest, None).expect("inventory");
        assert_eq!(inventory.jobs[0].output_name, "Same Game.wua");
        assert_eq!(inventory.jobs[1].output_name, "Same Game-2.wua");
    }

    #[test]
    fn only_job_filters_inventory() {
        let manifest = "\
100\tOne [0005000011111100].7z
100\tTwo [0005000022222200].7z
";
        let inventory = WiiUInventory::parse(manifest, Some("22222200")).expect("inventory");
        assert_eq!(inventory.jobs.len(), 1);
        assert_eq!(inventory.jobs[0].display_name, "Two");
    }

    #[test]
    fn update_label_overrides_malformed_base_prefix() {
        let manifest = "\
100\tGame [0005000012345600].7z
20\tGame [0005000012345600] [UPDATE v32].7z
";
        let inventory = WiiUInventory::parse(manifest, None).expect("inventory");
        let job = &inventory.jobs[0];
        assert_eq!(job.component_count(ComponentKind::Base), 1);
        assert_eq!(job.component_count(ComponentKind::Update), 1);
    }

    #[test]
    fn group_without_exactly_one_base_is_rejected() {
        let manifest = "20\tGame [0005000E12345600] [UPDATE v32].7z\n";
        assert!(WiiUInventory::parse(manifest, None).is_err());
    }
}
