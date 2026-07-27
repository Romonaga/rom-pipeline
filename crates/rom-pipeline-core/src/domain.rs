use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{PipelineError, Result};

pub const DEFAULT_BATCH_LIMIT: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BatchPolicy(NonZeroUsize);

impl BatchPolicy {
    /// Creates a completed-job batch limit.
    ///
    /// # Errors
    ///
    /// Returns an error when `limit` is zero.
    pub fn new(limit: usize) -> Result<Self> {
        NonZeroUsize::new(limit)
            .map(Self)
            .ok_or_else(|| PipelineError::InvalidConfig("batch limit must exceed zero".to_owned()))
    }

    #[must_use]
    pub const fn limit(self) -> usize {
        self.0.get()
    }
}

impl Default for BatchPolicy {
    fn default() -> Self {
        Self(NonZeroUsize::new(DEFAULT_BATCH_LIMIT).expect("default limit is non-zero"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceArtifact {
    pub title_id: String,
    pub expected_size: u64,
    pub name: String,
    pub role: ComponentKind,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ComponentKind {
    Base,
    Update,
    Dlc,
}

impl ComponentKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Update => "update",
            Self::Dlc => "dlc",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Job {
    pub id: String,
    pub display_name: String,
    pub output_name: String,
    pub sources: Vec<SourceArtifact>,
}

impl Job {
    #[must_use]
    pub fn component_fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        for source in &self.sources {
            digest.update(source.role.as_str().as_bytes());
            digest.update([0]);
            digest.update(source.title_id.as_bytes());
            digest.update([0]);
            digest.update(source.expected_size.to_be_bytes());
            digest.update(source.name.as_bytes());
            digest.update([0xff]);
        }
        let bytes = digest.finalize();
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
        }
        encoded
    }

    #[must_use]
    pub fn component_count(&self, role: ComponentKind) -> usize {
        self.sources
            .iter()
            .filter(|source| source.role == role)
            .count()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Readiness {
    Ready,
    Waiting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobOutcome {
    Completed,
    Interrupted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunOptions {
    pub limit: BatchPolicy,
    pub only_job: Option<String>,
    pub reverify: bool,
    pub wait_for_source: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            limit: BatchPolicy::default(),
            only_job: None,
            reverify: false,
            wait_for_source: true,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RunSummary {
    pub completed: usize,
    pub failed: usize,
    pub waiting: usize,
}

#[cfg(test)]
mod tests {
    use super::{BatchPolicy, ComponentKind, DEFAULT_BATCH_LIMIT, Job, SourceArtifact};

    #[test]
    fn batch_default_is_five() {
        assert_eq!(BatchPolicy::default().limit(), DEFAULT_BATCH_LIMIT);
        assert_eq!(DEFAULT_BATCH_LIMIT, 5);
    }

    #[test]
    fn zero_batch_is_rejected() {
        assert!(BatchPolicy::new(0).is_err());
    }

    #[test]
    fn component_fingerprint_changes_with_the_set() {
        let base = SourceArtifact {
            title_id: "0005000012345600".to_owned(),
            expected_size: 100,
            name: "Game [0005000012345600].7z".to_owned(),
            role: ComponentKind::Base,
        };
        let mut job = Job {
            id: "12345600".to_owned(),
            display_name: "Game".to_owned(),
            output_name: "Game.wua".to_owned(),
            sources: vec![base],
        };
        let base_only = job.component_fingerprint();
        job.sources.push(SourceArtifact {
            title_id: "0005000E12345600".to_owned(),
            expected_size: 20,
            name: "Game [0005000E12345600] [UPDATE v16].7z".to_owned(),
            role: ComponentKind::Update,
        });
        assert_ne!(job.component_fingerprint(), base_only);
    }
}
