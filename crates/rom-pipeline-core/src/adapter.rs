use crate::{Job, JobOutcome, Readiness, Result, StateStore, StopToken};

pub trait PipelineAdapter {
    /// Builds a deterministic list of title jobs.
    ///
    /// # Errors
    ///
    /// Returns an error when source metadata cannot be read or is malformed.
    fn inventory(&self, only_job: Option<&str>) -> Result<Vec<Job>>;

    /// Checks whether every required source artifact is present and complete.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate, conflicting, or unreadable artifacts.
    fn readiness(&self, job: &Job) -> Result<Readiness>;

    /// Checks whether the job has a valid completion marker and output.
    ///
    /// # Errors
    ///
    /// Returns an error when marker or output metadata cannot be read.
    fn is_complete(&self, job: &Job, state: &StateStore, reverify: bool) -> Result<bool>;

    /// Reconciles source moves for an already-completed job.
    ///
    /// # Errors
    ///
    /// Returns an error if source artifacts cannot be safely moved.
    fn reconcile_completed(&self, job: &Job, state: &StateStore) -> Result<()>;

    /// Converts and fully validates one job.
    ///
    /// # Errors
    ///
    /// Returns an error on extraction, conversion, validation, or finalization
    /// failure.
    fn process_job(&self, job: &Job, state: &StateStore, stop: &StopToken) -> Result<JobOutcome>;
}
