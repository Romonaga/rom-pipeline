pub mod adapter;
pub mod config;
pub mod domain;
pub mod error;
pub mod state;

pub use adapter::PipelineAdapter;
pub use config::{
    AppConfig, GameCubeSettings, Nintendo3dsSettings, ProfileConfig, Ps2Settings, PspSettings,
    ServiceConfig, SystemKind, WiiUSettings,
};
pub use domain::{
    BatchPolicy, ComponentKind, DEFAULT_BATCH_LIMIT, Job, JobOutcome, Readiness, RunOptions,
    RunSummary, SourceArtifact,
};
pub use error::{PipelineError, Result};
pub use state::{
    CompletionRecord, StateStore, StopToken, completion_output_valid, modified_seconds, sha256_file,
};
