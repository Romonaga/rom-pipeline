use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, PipelineError>;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("cannot decode TOML configuration: {0}")]
    TomlDecode(#[from] toml::de::Error),

    #[error("cannot encode TOML configuration: {0}")]
    TomlEncode(#[from] toml::ser::Error),

    #[error("external command failed ({status}): {command}")]
    CommandFailed { command: String, status: String },

    #[error("required path does not exist: {}", .0.display())]
    MissingPath(PathBuf),

    #[error("processing was interrupted cleanly")]
    Interrupted,

    #[error("{0}")]
    Message(String),
}

impl PipelineError {
    #[must_use]
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}
