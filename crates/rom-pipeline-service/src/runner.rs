use std::fs::{self, File, OpenOptions};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;

use fs2::FileExt;
use rom_pipeline_core::{
    Job, JobOutcome, PipelineAdapter, PipelineError, ProfileConfig, Readiness, Result, RunOptions,
    RunSummary, StateStore, StopToken, SystemKind,
};
use rom_pipeline_gamecube::GameCubeAdapter;
use rom_pipeline_nintendo_3ds::Nintendo3dsAdapter;
use rom_pipeline_ps2::Ps2Adapter;
use rom_pipeline_psp::PspAdapter;
use rom_pipeline_wiiu::WiiUAdapter;
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};

/// Runs one configured profile in the foreground.
///
/// # Errors
///
/// Returns an error on invalid configuration, preflight failure, lock
/// contention, state failure, source conflict, or conversion failure.
pub fn run_profile(profile: ProfileConfig, options: &RunOptions) -> Result<RunSummary> {
    match profile.system {
        SystemKind::WiiU => {
            let adapter = WiiUAdapter::new(profile.clone())?;
            adapter.preflight()?;
            Runner::new(profile, adapter).run(options)
        }
        SystemKind::GameCube => {
            let adapter = GameCubeAdapter::new(profile.clone())?;
            adapter.preflight()?;
            Runner::new(profile, adapter).run(options)
        }
        SystemKind::Nintendo3ds => {
            let adapter = Nintendo3dsAdapter::new(profile.clone())?;
            adapter.preflight()?;
            Runner::new(profile, adapter).run(options)
        }
        SystemKind::PlayStationPortable => {
            let adapter = PspAdapter::new(profile.clone())?;
            adapter.preflight()?;
            Runner::new(profile, adapter).run(options)
        }
        SystemKind::PlayStation2 => {
            let adapter = Ps2Adapter::new(profile.clone())?;
            adapter.preflight()?;
            Runner::new(profile, adapter).run(options)
        }
    }
}

struct Runner<A> {
    profile: ProfileConfig,
    adapter: A,
    state: StateStore,
}

struct Session {
    _lock: File,
    stop: StopToken,
    jobs: Vec<Job>,
}

enum Pass {
    BatchComplete,
    Finished,
    Pending { waiting: usize, made_progress: bool },
    Stopped,
}

impl<A: PipelineAdapter> Runner<A> {
    fn new(profile: ProfileConfig, adapter: A) -> Self {
        let state = StateStore::new(&profile.state_dir, &profile.log_dir);
        Self {
            profile,
            adapter,
            state,
        }
    }

    fn run(&self, options: &RunOptions) -> Result<RunSummary> {
        let session = self.prepare(options)?;
        let mut summary = RunSummary::default();
        loop {
            match self.run_pass(options, &session, &mut summary)? {
                Pass::BatchComplete => {
                    self.finish_batch(options, &summary)?;
                    return Ok(summary);
                }
                Pass::Finished => return self.finish_all(summary),
                Pass::Stopped => {
                    self.state.write_current("stopped cleanly")?;
                    self.state.log("STOPPED cleanly before next group")?;
                    return Ok(summary);
                }
                Pass::Pending {
                    waiting,
                    made_progress,
                } => {
                    summary.waiting = waiting;
                    if !options.wait_for_source {
                        self.state
                            .write_current(&format!("waiting groups={waiting}"))?;
                        self.state.log(&format!(
                            "INCOMPLETE waiting_groups={waiting} and no-wait was requested"
                        ))?;
                        return Ok(summary);
                    }
                    if !made_progress {
                        self.wait_for_sources(waiting)?;
                    }
                }
            }
        }
    }

    fn prepare(&self, options: &RunOptions) -> Result<Session> {
        self.state.prepare()?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.state.lock_path())
            .map_err(|error| PipelineError::io("open pipeline lock", error))?;
        lock.try_lock_exclusive().map_err(|error| {
            PipelineError::io("another pipeline process holds the profile lock", error)
        })?;
        let signal = Arc::new(AtomicBool::new(false));
        for signal_number in [SIGINT, SIGTERM, SIGHUP] {
            signal_hook::flag::register(signal_number, Arc::clone(&signal))
                .map_err(|error| PipelineError::io("register stop signal", error))?;
        }
        let stop = StopToken::new(self.state.stop_path(), signal);
        stop.clear()?;
        self.state.clear_current_failures()?;
        fs::write(
            self.profile.state_dir.join("batch.limit"),
            format!("{}\n", options.limit.limit()),
        )
        .map_err(|error| PipelineError::io("write batch limit", error))?;
        Ok(Session {
            _lock: lock,
            stop,
            jobs: self.adapter.inventory(options.only_job.as_deref())?,
        })
    }

    fn run_pass(
        &self,
        options: &RunOptions,
        session: &Session,
        summary: &mut RunSummary,
    ) -> Result<Pass> {
        let mut made_progress = false;
        let mut waiting = 0_usize;
        for job in &session.jobs {
            if session.stop.is_requested() {
                return Ok(Pass::Stopped);
            }
            if self
                .adapter
                .is_complete(job, &self.state, options.reverify)?
            {
                self.reconcile(job, summary)?;
                continue;
            }
            match self.adapter.readiness(job) {
                Ok(Readiness::Waiting) => {
                    waiting += 1;
                    continue;
                }
                Err(error) => {
                    summary.failed += 1;
                    self.state
                        .record_failure(&job.id, &format!("source readiness failed: {error}"))?;
                    continue;
                }
                Ok(Readiness::Ready) => {}
            }
            made_progress = true;
            if self.process_ready(job, &session.stop, summary)? {
                return Ok(Pass::Stopped);
            }
            if summary.completed >= options.limit.limit() {
                return Ok(Pass::BatchComplete);
            }
        }
        if waiting == 0 {
            Ok(Pass::Finished)
        } else {
            Ok(Pass::Pending {
                waiting,
                made_progress,
            })
        }
    }

    fn reconcile(&self, job: &Job, summary: &mut RunSummary) -> Result<()> {
        if let Err(error) = self.adapter.reconcile_completed(job, &self.state) {
            summary.failed += 1;
            self.state.record_failure(
                &job.id,
                &format!("verified output exists but source move failed: {error}"),
            )?;
        }
        Ok(())
    }

    fn process_ready(&self, job: &Job, stop: &StopToken, summary: &mut RunSummary) -> Result<bool> {
        match self.adapter.process_job(job, &self.state, stop) {
            Ok(JobOutcome::Completed) => summary.completed += 1,
            Ok(JobOutcome::Interrupted) | Err(PipelineError::Interrupted) => {
                self.state.write_current(&format!(
                    "stopped cleanly; group={} will retry on resume",
                    job.id
                ))?;
                self.state.log(&format!(
                    "STOPPED cleanly; group={} remains resumable",
                    job.id
                ))?;
                return Ok(true);
            }
            Err(error) => {
                summary.failed += 1;
                self.state.record_failure(
                    &job.id,
                    &format!(
                        "processing failed: {error}; see {}/groups/{}.log",
                        self.profile.log_dir.display(),
                        job.id
                    ),
                )?;
            }
        }
        Ok(false)
    }

    fn finish_batch(&self, options: &RunOptions, summary: &RunSummary) -> Result<()> {
        self.state.write_current(&format!(
            "batch complete converted={} limit={}",
            summary.completed,
            options.limit.limit()
        ))?;
        self.state.log(&format!(
            "BATCH COMPLETE converted={} limit={}",
            summary.completed,
            options.limit.limit()
        ))
    }

    fn finish_all(&self, summary: RunSummary) -> Result<RunSummary> {
        self.state
            .write_current(&format!("finished failures={}", summary.failed))?;
        if summary.failed > 0 {
            self.state
                .log(&format!("FINISHED with failures={}", summary.failed))?;
            return Err(PipelineError::Message(format!(
                "pipeline finished with {} failures",
                summary.failed
            )));
        }
        self.state
            .log("FINISHED all inventoried groups successfully")?;
        Ok(summary)
    }

    fn wait_for_sources(&self, waiting: usize) -> Result<()> {
        let settings = self
            .profile
            .wiiu
            .as_ref()
            .ok_or_else(|| PipelineError::InvalidConfig("missing Wii U settings".to_owned()))?;
        if !user_service_is_active(&settings.source_service)? {
            return Err(PipelineError::Message(format!(
                "source download is inactive but {waiting} groups remain unavailable"
            )));
        }
        self.state
            .write_current(&format!("waiting for source groups={waiting}"))?;
        self.state.log(&format!(
            "WAITING for source downloads groups={waiting} sleep={}s",
            settings.wait_seconds
        ))?;
        thread::sleep(Duration::from_secs(settings.wait_seconds));
        Ok(())
    }
}

fn user_service_is_active(unit: &str) -> Result<bool> {
    let status = std::process::Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", unit])
        .status()
        .map_err(|error| PipelineError::io(format!("query service {unit}"), error))?;
    Ok(status.success())
}
