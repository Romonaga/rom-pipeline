use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{BatchPolicy, PipelineError, Result};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SystemKind {
    WiiU,
    GameCube,
    #[serde(rename = "3ds")]
    Nintendo3ds,
    #[serde(rename = "psp")]
    PlayStationPortable,
    #[serde(rename = "ps2")]
    PlayStation2,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceConfig {
    pub bind: String,
    pub default_batch_limit: usize,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8787".to_owned(),
            default_batch_limit: 5,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WiiUSettings {
    pub manifest: PathBuf,
    pub cdecrypt: PathBuf,
    pub zarchive: PathBuf,
    #[serde(default = "default_wait_seconds")]
    pub wait_seconds: u64,
    #[serde(default = "default_source_service")]
    pub source_service: String,
}

const fn default_wait_seconds() -> u64 {
    60
}

fn default_source_service() -> String {
    "archive-wiiu-download.service".to_owned()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ps2Settings {
    pub manifest: PathBuf,
    pub chdman: PathBuf,
    pub minimum_savings_percent: u8,
    pub preserve_when_compression_is_not_worthwhile: bool,
    #[serde(default = "enabled")]
    pub verify_round_trip: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameCubeSettings {
    pub manifest: PathBuf,
    pub dolphin_tool: PathBuf,
    #[serde(default = "default_rvz_block_size")]
    pub block_size: u32,
    #[serde(default = "default_rvz_compression")]
    pub compression: String,
    #[serde(default = "default_rvz_compression_level")]
    pub compression_level: i32,
    #[serde(default = "enabled")]
    pub verify_round_trip: bool,
}

const fn default_rvz_block_size() -> u32 {
    131_072
}

fn default_rvz_compression() -> String {
    "zstd".to_owned()
}

const fn default_rvz_compression_level() -> i32 {
    5
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nintendo3dsSettings {
    pub manifest: PathBuf,
    pub seven_zip: PathBuf,
    pub python: PathBuf,
    pub converter: PathBuf,
    pub ctrtool: PathBuf,
    #[serde(default = "enabled")]
    pub normalize_crypto_flags: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PspSettings {
    pub chdman: PathBuf,
    #[serde(default = "default_psp_codec")]
    pub codec: String,
    #[serde(default = "default_psp_hunk_size")]
    pub hunk_size: u32,
    #[serde(default = "enabled")]
    pub verify_round_trip: bool,
}

fn default_psp_codec() -> String {
    "zstd".to_owned()
}

const fn default_psp_hunk_size() -> u32 {
    2048
}

const fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub id: String,
    pub name: String,
    pub system: SystemKind,
    pub source_format: String,
    pub source_dir: PathBuf,
    pub done_dir: PathBuf,
    pub work_dir: PathBuf,
    pub state_dir: PathBuf,
    pub log_dir: PathBuf,
    pub output_dir: PathBuf,
    pub library_dir: Option<PathBuf>,
    pub output_format: String,
    pub batch_limit: usize,
    pub wiiu: Option<WiiUSettings>,
    pub gamecube: Option<GameCubeSettings>,
    #[serde(rename = "3ds")]
    pub nintendo_3ds: Option<Nintendo3dsSettings>,
    pub psp: Option<PspSettings>,
    pub ps2: Option<Ps2Settings>,
}

impl ProfileConfig {
    /// Validates common paths and required system-specific settings.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe paths, invalid identifiers, a zero batch
    /// limit, or missing adapter settings.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(PipelineError::InvalidConfig(format!(
                "profile id must use lowercase letters, digits, or hyphens: {}",
                self.id
            )));
        }
        for (field, value) in [
            ("name", self.name.as_str()),
            ("source_format", self.source_format.as_str()),
            ("output_format", self.output_format.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(PipelineError::InvalidConfig(format!(
                    "{field} cannot be empty"
                )));
            }
        }
        let _ = BatchPolicy::new(self.batch_limit)?;

        for (field, path) in [
            ("source_dir", self.source_dir.as_path()),
            ("done_dir", self.done_dir.as_path()),
            ("work_dir", self.work_dir.as_path()),
            ("state_dir", self.state_dir.as_path()),
            ("log_dir", self.log_dir.as_path()),
            ("output_dir", self.output_dir.as_path()),
        ] {
            if !path.is_absolute() {
                return Err(PipelineError::InvalidConfig(format!(
                    "{field} must be absolute: {}",
                    path.display()
                )));
            }
        }
        if let Some(path) = &self.library_dir {
            if !path.is_absolute() {
                return Err(PipelineError::InvalidConfig(format!(
                    "library_dir must be absolute: {}",
                    path.display()
                )));
            }
        }

        for (field, path) in [
            ("done_dir", self.done_dir.as_path()),
            ("work_dir", self.work_dir.as_path()),
            ("state_dir", self.state_dir.as_path()),
            ("log_dir", self.log_dir.as_path()),
            ("output_dir", self.output_dir.as_path()),
        ] {
            if path == self.source_dir {
                return Err(PipelineError::InvalidConfig(format!(
                    "{field} cannot equal source_dir"
                )));
            }
        }
        if self.output_dir == self.done_dir {
            return Err(PipelineError::InvalidConfig(
                "output_dir cannot equal done_dir".to_owned(),
            ));
        }

        validate_system_settings(self)
    }

    /// Returns the validated batch policy for this profile.
    ///
    /// # Errors
    ///
    /// Returns an error if the configured limit is zero.
    pub fn batch_policy(&self) -> Result<BatchPolicy> {
        BatchPolicy::new(self.batch_limit)
    }
}

fn validate_system_settings(profile: &ProfileConfig) -> Result<()> {
    match profile.system {
        SystemKind::WiiU if profile.wiiu.is_none() => Err(PipelineError::InvalidConfig(
            "Wii U profile requires [profiles.wiiu] settings".to_owned(),
        )),
        SystemKind::GameCube if profile.gamecube.is_none() => Err(PipelineError::InvalidConfig(
            "GameCube profile requires [profiles.gamecube] settings".to_owned(),
        )),
        SystemKind::GameCube
            if profile
                .gamecube
                .as_ref()
                .is_some_and(|settings| settings.block_size == 0) =>
        {
            Err(PipelineError::InvalidConfig(
                "GameCube RVZ block_size must exceed zero".to_owned(),
            ))
        }
        SystemKind::GameCube
            if profile
                .gamecube
                .as_ref()
                .is_some_and(|settings| settings.compression.trim().is_empty()) =>
        {
            Err(PipelineError::InvalidConfig(
                "GameCube RVZ compression cannot be empty".to_owned(),
            ))
        }
        SystemKind::Nintendo3ds if profile.nintendo_3ds.is_none() => {
            Err(PipelineError::InvalidConfig(
                "Nintendo 3DS profile requires [profiles.3ds] settings".to_owned(),
            ))
        }
        SystemKind::PlayStationPortable if profile.psp.is_none() => Err(
            PipelineError::InvalidConfig("PSP profile requires [profiles.psp] settings".to_owned()),
        ),
        SystemKind::PlayStation2 if profile.ps2.is_none() => Err(PipelineError::InvalidConfig(
            "PS2 profile requires [profiles.ps2] settings".to_owned(),
        )),
        SystemKind::PlayStation2
            if profile
                .ps2
                .as_ref()
                .is_some_and(|settings| settings.minimum_savings_percent > 100) =>
        {
            Err(PipelineError::InvalidConfig(
                "PS2 minimum_savings_percent cannot exceed 100".to_owned(),
            ))
        }
        _ => Ok(()),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default)]
    pub service: ServiceConfig,
    pub profiles: Vec<ProfileConfig>,
}

impl AppConfig {
    /// Loads and validates a TOML configuration file.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, TOML cannot be decoded,
    /// or any profile is invalid.
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .map_err(|error| PipelineError::io(format!("read {}", path.display()), error))?;
        let config: Self = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    /// Validates service defaults, unique profile IDs, and every profile.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero default, duplicate profile ID, or invalid
    /// profile.
    pub fn validate(&self) -> Result<()> {
        let _ = BatchPolicy::new(self.service.default_batch_limit)?;
        let mut ids = std::collections::BTreeSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            if !ids.insert(&profile.id) {
                return Err(PipelineError::InvalidConfig(format!(
                    "duplicate profile id: {}",
                    profile.id
                )));
            }
        }
        Ok(())
    }

    /// Finds a configured profile.
    ///
    /// # Errors
    ///
    /// Returns an error when no profile has the requested ID.
    pub fn profile(&self, id: &str) -> Result<&ProfileConfig> {
        self.profiles
            .iter()
            .find(|profile| profile.id == id)
            .ok_or_else(|| PipelineError::InvalidConfig(format!("unknown profile: {id}")))
    }

    /// Saves the configuration by writing a sibling temporary file and
    /// atomically renaming it into place.
    ///
    /// # Errors
    ///
    /// Returns an error if validation, serialization, directory creation,
    /// writing, or renaming fails.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let text = toml::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                PipelineError::io(format!("create {}", parent.display()), error)
            })?;
        }
        let temporary = path.with_extension("toml.new");
        fs::write(&temporary, text)
            .map_err(|error| PipelineError::io(format!("write {}", temporary.display()), error))?;
        fs::rename(&temporary, path)
            .map_err(|error| PipelineError::io(format!("publish {}", path.display()), error))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppConfig, Nintendo3dsSettings, ProfileConfig, ServiceConfig, SystemKind, WiiUSettings,
    };
    use std::path::PathBuf;

    fn profile() -> ProfileConfig {
        ProfileConfig {
            id: "wiiu".to_owned(),
            name: "Wii U".to_owned(),
            system: SystemKind::WiiU,
            source_format: "nus-7z".to_owned(),
            source_dir: PathBuf::from("/source"),
            done_dir: PathBuf::from("/source/done"),
            work_dir: PathBuf::from("/work"),
            state_dir: PathBuf::from("/state"),
            log_dir: PathBuf::from("/logs"),
            output_dir: PathBuf::from("/output"),
            library_dir: None,
            output_format: "wua".to_owned(),
            batch_limit: 5,
            wiiu: Some(WiiUSettings {
                manifest: PathBuf::from("/manifest.tsv"),
                cdecrypt: PathBuf::from("/cdecrypt"),
                zarchive: PathBuf::from("/zarchive"),
                wait_seconds: 60,
                source_service: "download.service".to_owned(),
            }),
            gamecube: None,
            nintendo_3ds: None,
            psp: None,
            ps2: None,
        }
    }

    #[test]
    fn valid_configuration_round_trips_through_toml() {
        let config = AppConfig {
            service: ServiceConfig::default(),
            profiles: vec![profile()],
        };
        let encoded = toml::to_string(&config).expect("serialize");
        let decoded: AppConfig = toml::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, config);
        assert!(decoded.validate().is_ok());
    }

    #[test]
    fn duplicate_profile_ids_are_rejected() {
        let one = profile();
        let two = one.clone();
        let config = AppConfig {
            service: ServiceConfig::default(),
            profiles: vec![one, two],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn nintendo_3ds_settings_round_trip_under_numeric_table_name() {
        let mut profile = profile();
        profile.id = "3ds".to_owned();
        profile.name = "Nintendo 3DS".to_owned();
        profile.system = SystemKind::Nintendo3ds;
        profile.wiiu = None;
        profile.nintendo_3ds = Some(Nintendo3dsSettings {
            manifest: PathBuf::from("/manifest.tsv"),
            seven_zip: PathBuf::from("/usr/bin/7z"),
            python: PathBuf::from("/usr/bin/python3"),
            converter: PathBuf::from("/converter.py"),
            ctrtool: PathBuf::from("/ctrtool"),
            normalize_crypto_flags: true,
        });
        let config = AppConfig {
            service: ServiceConfig::default(),
            profiles: vec![profile],
        };
        let encoded = toml::to_string(&config).expect("serialize");
        assert!(encoded.contains("[[profiles]]"));
        assert!(encoded.contains("[profiles.3ds]"));
        let decoded: AppConfig = toml::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, config);
        assert!(decoded.validate().is_ok());
    }
}
