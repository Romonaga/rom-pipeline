use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use rom_pipeline_core::{
    ComponentKind, Job, PipelineError, Result, SourceArtifact, StateStore, StopToken,
};

use crate::adapter::WiiUAdapter;
use crate::command::{check_stop, run_logged};
use crate::filesystem::{clear_owned_directory, contains_symlink, find_named};
use crate::metadata::{TitleMetadata, parse_meta_xml, parse_tmd_title_id};
use crate::process::JobWorkspace;

pub fn extract_and_decrypt(
    adapter: &WiiUAdapter,
    job: &Job,
    state: &StateStore,
    stop: &StopToken,
    work: &JobWorkspace,
) -> Result<()> {
    for artifact in &job.sources {
        convert_artifact(adapter, artifact, state, stop, work)?;
    }
    Ok(())
}

fn convert_artifact(
    adapter: &WiiUAdapter,
    artifact: &SourceArtifact,
    state: &StateStore,
    stop: &StopToken,
    work: &JobWorkspace,
) -> Result<()> {
    check_stop(stop)?;
    let archive = adapter.locate(&artifact.name)?.ok_or_else(|| {
        PipelineError::MissingPath(adapter.profile().source_dir.join(&artifact.name))
    })?;
    let key = format!(
        "{}-{}",
        artifact.role.as_str(),
        artifact.title_id.to_ascii_lowercase()
    );
    let extract_dir = work.group_work.join(format!("extracted-{key}"));
    let decrypt_dir = work.group_work.join(format!("decrypted-{key}"));
    fs::create_dir_all(&extract_dir)
        .map_err(|error| PipelineError::io(format!("create {}", extract_dir.display()), error))?;

    test_and_extract(artifact, &archive, &extract_dir, state, stop, work)?;
    let nus_dir = locate_nus_payload(artifact, &extract_dir)?;
    let metadata =
        decrypt_and_validate(adapter, artifact, &nus_dir, &decrypt_dir, state, stop, work)?;
    publish_decrypted_title(artifact, &metadata, &decrypt_dir, &work.pack_root)?;
    clear_owned_directory(&extract_dir, &work.groups_root)
}

fn test_and_extract(
    artifact: &SourceArtifact,
    archive: &Path,
    extract_dir: &Path,
    state: &StateStore,
    stop: &StopToken,
    work: &JobWorkspace,
) -> Result<()> {
    state.log(&format!("TEST archive: {}", artifact.name))?;
    run_logged(
        "7z",
        [
            OsString::from("t"),
            OsString::from("-bd"),
            OsString::from("-bso0"),
            OsString::from("-bsp0"),
            OsString::from("--"),
            archive.as_os_str().to_owned(),
        ],
        &work.group_log,
    )?;
    check_stop(stop)?;
    state.log(&format!("EXTRACT archive: {}", artifact.name))?;
    run_logged(
        "7z",
        [
            OsString::from("x"),
            OsString::from("-y"),
            OsString::from("-bd"),
            OsString::from("-bso0"),
            OsString::from("-bsp0"),
            OsString::from(format!("-o{}", extract_dir.display())),
            OsString::from("--"),
            archive.as_os_str().to_owned(),
        ],
        &work.group_log,
    )
}

fn locate_nus_payload(artifact: &SourceArtifact, extract_dir: &Path) -> Result<PathBuf> {
    let title_manifests = find_named(extract_dir, "title.tmd")?;
    if title_manifests.len() != 1 {
        return Err(PipelineError::Message(format!(
            "expected one title.tmd in {}, found {}",
            artifact.name,
            title_manifests.len()
        )));
    }
    let tmd_path = &title_manifests[0];
    let tmd = fs::read(tmd_path)
        .map_err(|error| PipelineError::io(format!("read {}", tmd_path.display()), error))?;
    let actual_title_id = parse_tmd_title_id(&tmd)?;
    let expected_title_id = component_title_id(artifact.role, &artifact.title_id[8..]);
    if actual_title_id != expected_title_id {
        return Err(PipelineError::Message(format!(
            "TMD title ID mismatch role={} expected={} actual={} archive={}",
            artifact.role.as_str(),
            expected_title_id,
            actual_title_id,
            artifact.name
        )));
    }
    let nus_dir = tmd_path
        .parent()
        .ok_or_else(|| PipelineError::Message("title.tmd has no parent".to_owned()))?
        .to_path_buf();
    if !nus_dir.join("title.tik").is_file() {
        return Err(PipelineError::MissingPath(nus_dir.join("title.tik")));
    }
    Ok(nus_dir)
}

fn decrypt_and_validate(
    adapter: &WiiUAdapter,
    artifact: &SourceArtifact,
    nus_dir: &Path,
    decrypt_dir: &Path,
    state: &StateStore,
    stop: &StopToken,
    work: &JobWorkspace,
) -> Result<TitleMetadata> {
    check_stop(stop)?;
    state.log(&format!("DECRYPT archive: {}", artifact.name))?;
    let settings = adapter
        .profile()
        .wiiu
        .as_ref()
        .ok_or_else(|| PipelineError::InvalidConfig("missing Wii U settings".to_owned()))?;
    run_logged(
        &settings.cdecrypt,
        [nus_dir.as_os_str(), decrypt_dir.as_os_str()],
        &work.group_log,
    )?;
    let metadata = read_and_validate_metadata(artifact, decrypt_dir)?;
    if artifact.role == ComponentKind::Base && !decrypt_dir.join("code").is_dir() {
        return Err(PipelineError::Message(format!(
            "base title has no decrypted code directory: {}",
            artifact.name
        )));
    }
    if contains_symlink(decrypt_dir)? {
        return Err(PipelineError::Message(format!(
            "decrypted title contains a symbolic link: {}",
            artifact.name
        )));
    }
    Ok(metadata)
}

fn read_and_validate_metadata(
    artifact: &SourceArtifact,
    decrypt_dir: &Path,
) -> Result<TitleMetadata> {
    let meta_path = decrypt_dir.join("meta/meta.xml");
    let xml = fs::read_to_string(&meta_path)
        .map_err(|error| PipelineError::io(format!("read {}", meta_path.display()), error))?;
    let metadata = parse_meta_xml(&xml)?;
    let expected_title_id = match artifact.role {
        ComponentKind::Base | ComponentKind::Update => {
            component_title_id(ComponentKind::Base, &artifact.title_id[8..])
        }
        ComponentKind::Dlc => component_title_id(ComponentKind::Dlc, &artifact.title_id[8..]),
    };
    if metadata.title_id != expected_title_id {
        return Err(PipelineError::Message(format!(
            "decrypted metadata title ID mismatch role={} expected={} actual={}",
            artifact.role.as_str(),
            expected_title_id,
            metadata.title_id
        )));
    }
    Ok(metadata)
}

fn component_title_id(role: ComponentKind, suffix: &str) -> String {
    let prefix = match role {
        ComponentKind::Base => "00050000",
        ComponentKind::Update => "0005000E",
        ComponentKind::Dlc => "0005000C",
    };
    format!("{prefix}{suffix}")
}

fn packaged_title_id(artifact: &SourceArtifact) -> String {
    component_title_id(artifact.role, &artifact.title_id[8..])
}

fn publish_decrypted_title(
    artifact: &SourceArtifact,
    metadata: &TitleMetadata,
    decrypt_dir: &Path,
    pack_root: &Path,
) -> Result<()> {
    let title_folder = pack_root.join(format!(
        "{}_v{}",
        packaged_title_id(artifact).to_ascii_lowercase(),
        metadata.version
    ));
    if title_folder.exists() {
        return Err(PipelineError::Message(format!(
            "duplicate title/version folder for {}: {}",
            artifact.name,
            title_folder.display()
        )));
    }
    fs::rename(decrypt_dir, &title_folder).map_err(|error| {
        PipelineError::io(
            format!(
                "move {} to {}",
                decrypt_dir.display(),
                title_folder.display()
            ),
            error,
        )
    })
}

#[cfg(test)]
mod tests {
    use rom_pipeline_core::{ComponentKind, SourceArtifact};

    use super::packaged_title_id;

    #[test]
    fn packaged_title_id_distinguishes_base_update_and_dlc() {
        let artifact = |role, title_id: &str| SourceArtifact {
            role,
            title_id: title_id.to_owned(),
            expected_size: 1,
            name: "fixture.7z".to_owned(),
        };

        assert_eq!(
            packaged_title_id(&artifact(ComponentKind::Base, "0005000012345600")),
            "0005000012345600"
        );
        assert_eq!(
            packaged_title_id(&artifact(ComponentKind::Update, "0005000E12345600")),
            "0005000E12345600"
        );
        assert_eq!(
            packaged_title_id(&artifact(ComponentKind::Dlc, "0005000C12345600")),
            "0005000C12345600"
        );
    }

    #[test]
    fn packaged_update_id_uses_role_when_archive_name_has_base_prefix() {
        let artifact = SourceArtifact {
            role: ComponentKind::Update,
            title_id: "0005000012345600".to_owned(),
            expected_size: 1,
            name: "Game [0005000012345600] [UPDATE v32].7z".to_owned(),
        };

        assert_eq!(packaged_title_id(&artifact), "0005000E12345600");
    }
}
