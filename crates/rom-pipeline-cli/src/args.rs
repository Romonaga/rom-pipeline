use std::env;
use std::path::PathBuf;

use rom_pipeline_core::{PipelineError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Doctor,
    ExampleConfig,
    Help,
    Inventory,
    Migrate3dsLibrary,
    Prune,
    Publish,
    Run,
    Serve,
    Start,
    Status,
    Stop,
    Validate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cli {
    pub action: Action,
    pub config: PathBuf,
    pub profile: String,
    pub limit: Option<usize>,
    pub only: Option<String>,
    pub reverify: bool,
    pub wait_for_source: bool,
    pub confirm_prune: bool,
}

impl Cli {
    /// Parses command-line arguments.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown commands/options, missing values, or an
    /// invalid numeric limit.
    pub fn parse() -> Result<Self> {
        let mut arguments = env::args().skip(1);
        let action = match arguments.next().as_deref() {
            None | Some("help" | "-h" | "--help") => Action::Help,
            Some("doctor") => Action::Doctor,
            Some("example-config") => Action::ExampleConfig,
            Some("inventory" | "dry-run") => Action::Inventory,
            Some("migrate-3ds-library") => Action::Migrate3dsLibrary,
            Some("publish") => Action::Publish,
            Some("prune") => Action::Prune,
            Some("run") => Action::Run,
            Some("serve") => Action::Serve,
            Some("start" | "resume") => Action::Start,
            Some("status") => Action::Status,
            Some("stop") => Action::Stop,
            Some("validate") => Action::Validate,
            Some(other) => {
                return Err(PipelineError::Message(format!("unknown command: {other}")));
            }
        };

        let mut cli = Self {
            action,
            config: default_config_path(),
            profile: "wiiu".to_owned(),
            limit: None,
            only: None,
            reverify: false,
            wait_for_source: true,
            confirm_prune: false,
        };
        let remaining: Vec<String> = arguments.collect();
        let mut index = 0;
        while index < remaining.len() {
            match remaining[index].as_str() {
                "--config" => {
                    index += 1;
                    cli.config = value(&remaining, index, "--config")?.into();
                }
                "--profile" => {
                    index += 1;
                    value(&remaining, index, "--profile")?.clone_into(&mut cli.profile);
                }
                "--limit" => {
                    index += 1;
                    cli.limit = Some(parse_limit(value(&remaining, index, "--limit")?)?);
                }
                "--only" => {
                    index += 1;
                    cli.only = Some(value(&remaining, index, "--only")?.to_ascii_uppercase());
                }
                "--reverify" => cli.reverify = true,
                "--no-wait" => cli.wait_for_source = false,
                "--confirm-prune" => cli.confirm_prune = true,
                argument if argument.bytes().all(|byte| byte.is_ascii_digit()) => {
                    cli.limit = Some(parse_limit(argument)?);
                }
                unknown => {
                    return Err(PipelineError::Message(format!("unknown option: {unknown}")));
                }
            }
            index += 1;
        }
        Ok(cli)
    }
}

fn value<'a>(arguments: &'a [String], index: usize, option: &str) -> Result<&'a str> {
    arguments
        .get(index)
        .map(String::as_str)
        .ok_or_else(|| PipelineError::Message(format!("{option} requires a value")))
}

fn parse_limit(value: &str) -> Result<usize> {
    let limit = value
        .parse::<usize>()
        .map_err(|_| PipelineError::Message(format!("invalid batch limit: {value}")))?;
    if limit == 0 {
        return Err(PipelineError::Message(
            "batch limit must exceed zero".to_owned(),
        ));
    }
    Ok(limit)
}

fn default_config_path() -> PathBuf {
    if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(path).join("rom-pipeline/config.toml");
    }
    env::var_os("HOME").map_or_else(
        || PathBuf::from(".config/rom-pipeline/config.toml"),
        |path| PathBuf::from(path).join(".config/rom-pipeline/config.toml"),
    )
}
