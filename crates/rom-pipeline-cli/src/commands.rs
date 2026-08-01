use std::env;
use std::path::PathBuf;

use rom_pipeline_core::{
    AppConfig, BatchPolicy, PipelineAdapter, PipelineError, Readiness, Result, RunOptions,
    StateStore, SystemKind,
};
use rom_pipeline_gamecube::{
    GameCubeAdapter, prune_sources as prune_gamecube_sources,
    publish_library as publish_gamecube_library,
};
use rom_pipeline_nintendo_3ds::{
    Nintendo3dsAdapter, migrate_cci_library, prune_sources as prune_3ds_sources,
    publish_library as publish_3ds_library,
};
use rom_pipeline_playstation_vita::VitaAdapter;
use rom_pipeline_ps2::{
    Ps2Adapter, prune_sources as prune_ps2_sources, publish_library as publish_ps2_library,
};
use rom_pipeline_psp::{PspAdapter, prune_sources, publish_library};
use rom_pipeline_service::{
    profile_status, request_stop, run_profile, start_job_service, start_service,
};
use rom_pipeline_wiiu::WiiUAdapter;

use crate::args::{Action, Cli};
use crate::output;

const EXAMPLE_CONFIG: &str = include_str!("../../../config/profiles.example.toml");

pub enum Execution {
    Complete,
    Serve {
        config: PathBuf,
        executable: PathBuf,
    },
}

pub fn execute(cli: Cli) -> Result<Execution> {
    match cli.action {
        Action::Help => output::help(),
        Action::ExampleConfig => print!("{EXAMPLE_CONFIG}"),
        Action::Validate => {
            let config = AppConfig::load(&cli.config)?;
            println!(
                "valid configuration: {} profiles ({})",
                config.profiles.len(),
                cli.config.display()
            );
        }
        Action::Doctor => doctor(&cli)?,
        Action::Inventory => inventory(&cli)?,
        Action::Migrate3dsLibrary => migrate_3ds_library(&cli)?,
        Action::Publish => publish(&cli)?,
        Action::Prune => prune(&cli)?,
        Action::Run => run(&cli)?,
        Action::Start => start(&cli)?,
        Action::Stop => stop(&cli)?,
        Action::Status => status(&cli)?,
        Action::Serve => {
            let _ = AppConfig::load(&cli.config)?;
            return Ok(Execution::Serve {
                config: cli.config,
                executable: env::current_exe()
                    .map_err(|error| PipelineError::io("locate current executable", error))?,
            });
        }
    }
    Ok(Execution::Complete)
}

fn migrate_3ds_library(cli: &Cli) -> Result<()> {
    let config = AppConfig::load(&cli.config)?;
    let profile = config.profile(&cli.profile)?.clone();
    if !matches!(profile.system, SystemKind::Nintendo3ds) {
        return Err(PipelineError::Message(
            "migrate-3ds-library requires a Nintendo 3DS profile".to_owned(),
        ));
    }
    let limit = BatchPolicy::new(cli.limit.unwrap_or(profile.batch_limit))?;
    let summary = migrate_cci_library(&Nintendo3dsAdapter::new(profile)?, limit)?;
    println!(
        "converted={} already_complete={} failed={}",
        summary.converted, summary.already_complete, summary.failed
    );
    Ok(())
}

fn publish(cli: &Cli) -> Result<()> {
    let config = AppConfig::load(&cli.config)?;
    let profile = config.profile(&cli.profile)?.clone();
    let limit = BatchPolicy::new(cli.limit.unwrap_or(profile.batch_limit))?;
    let (completed, failed, removed, reclaimed) = match profile.system {
        SystemKind::Nintendo3ds => {
            let summary = publish_3ds_library(&Nintendo3dsAdapter::new(profile)?, limit)?;
            (
                summary.completed_jobs,
                summary.failed_jobs,
                summary.files_removed,
                summary.bytes_reclaimed,
            )
        }
        SystemKind::GameCube => {
            let summary = publish_gamecube_library(&GameCubeAdapter::new(profile)?, limit)?;
            (
                summary.completed_jobs,
                summary.failed_jobs,
                summary.files_removed,
                summary.bytes_reclaimed,
            )
        }
        SystemKind::PlayStationPortable => {
            let summary = publish_library(&PspAdapter::new(profile)?, limit)?;
            (
                summary.completed_jobs,
                summary.failed_jobs,
                summary.files_removed,
                summary.bytes_reclaimed,
            )
        }
        SystemKind::PlayStation2 => {
            let summary = publish_ps2_library(&Ps2Adapter::new(profile)?, limit)?;
            (
                summary.completed_jobs,
                summary.failed_jobs,
                summary.files_removed,
                summary.bytes_reclaimed,
            )
        }
        SystemKind::WiiU | SystemKind::PlayStationVita => {
            return Err(PipelineError::Message(
                "publish is not used for Wii U or Vita device deployments".to_owned(),
            ));
        }
    };
    println!(
        "published={completed} failed={failed} staging_files_removed={removed} \
         bytes_reclaimed={reclaimed}"
    );
    Ok(())
}

fn prune(cli: &Cli) -> Result<()> {
    if !cli.confirm_prune {
        return Err(PipelineError::Message(
            "prune permanently deletes processed source files; pass --confirm-prune".to_owned(),
        ));
    }
    let config = AppConfig::load(&cli.config)?;
    let profile = config.profile(&cli.profile)?.clone();
    let limit = BatchPolicy::new(cli.limit.unwrap_or(profile.batch_limit))?;
    let (completed, failed, removed, reclaimed) = match profile.system {
        SystemKind::Nintendo3ds => {
            let summary = prune_3ds_sources(&Nintendo3dsAdapter::new(profile)?, limit)?;
            (
                summary.completed_jobs,
                summary.failed_jobs,
                summary.files_removed,
                summary.bytes_reclaimed,
            )
        }
        SystemKind::GameCube => {
            let summary = prune_gamecube_sources(&GameCubeAdapter::new(profile)?, limit)?;
            (
                summary.completed_jobs,
                summary.failed_jobs,
                summary.files_removed,
                summary.bytes_reclaimed,
            )
        }
        SystemKind::PlayStationPortable => {
            let summary = prune_sources(&PspAdapter::new(profile)?, limit)?;
            (
                summary.completed_jobs,
                summary.failed_jobs,
                summary.files_removed,
                summary.bytes_reclaimed,
            )
        }
        SystemKind::PlayStation2 => {
            let summary = prune_ps2_sources(&Ps2Adapter::new(profile)?, limit)?;
            (
                summary.completed_jobs,
                summary.failed_jobs,
                summary.files_removed,
                summary.bytes_reclaimed,
            )
        }
        SystemKind::WiiU | SystemKind::PlayStationVita => {
            return Err(PipelineError::Message(
                "prune is not available for Wii U or preserved Vita archives".to_owned(),
            ));
        }
    };
    println!(
        "pruned_jobs={completed} failed={failed} source_files_removed={removed} \
         bytes_reclaimed={reclaimed}"
    );
    Ok(())
}

fn doctor(cli: &Cli) -> Result<()> {
    let config = AppConfig::load(&cli.config)?;
    let profile = config.profile(&cli.profile)?.clone();
    match profile.system {
        SystemKind::WiiU => WiiUAdapter::new(profile.clone())?.preflight()?,
        SystemKind::GameCube => GameCubeAdapter::new(profile.clone())?.preflight()?,
        SystemKind::Nintendo3ds => Nintendo3dsAdapter::new(profile.clone())?.preflight()?,
        SystemKind::PlayStationPortable => PspAdapter::new(profile.clone())?.preflight()?,
        SystemKind::PlayStation2 => Ps2Adapter::new(profile.clone())?.preflight()?,
        SystemKind::PlayStationVita => VitaAdapter::new(profile.clone())?.preflight()?,
    }
    println!("configuration: valid");
    println!("profile: {} ({})", profile.name, profile.id);
    println!("source: {}", profile.source_dir.display());
    println!("output: {}", profile.output_dir.display());
    println!("default_batch_limit: {}", profile.batch_limit);
    println!("preflight: passed");
    Ok(())
}

fn inventory(cli: &Cli) -> Result<()> {
    let config = AppConfig::load(&cli.config)?;
    let profile = config.profile(&cli.profile)?.clone();
    let state = StateStore::new(&profile.state_dir, &profile.log_dir);
    match profile.system {
        SystemKind::WiiU => {
            inventory_adapter(&WiiUAdapter::new(profile.clone())?, &profile, &state, cli)?;
        }
        SystemKind::GameCube => {
            inventory_adapter(
                &GameCubeAdapter::new(profile.clone())?,
                &profile,
                &state,
                cli,
            )?;
        }
        SystemKind::Nintendo3ds => {
            inventory_adapter(
                &Nintendo3dsAdapter::new(profile.clone())?,
                &profile,
                &state,
                cli,
            )?;
        }
        SystemKind::PlayStationPortable => {
            inventory_adapter(&PspAdapter::new(profile.clone())?, &profile, &state, cli)?;
        }
        SystemKind::PlayStation2 => {
            inventory_adapter(&Ps2Adapter::new(profile.clone())?, &profile, &state, cli)?;
        }
        SystemKind::PlayStationVita => {
            inventory_adapter(&VitaAdapter::new(profile.clone())?, &profile, &state, cli)?;
        }
    }
    Ok(())
}

fn inventory_adapter(
    adapter: &impl PipelineAdapter,
    profile: &rom_pipeline_core::ProfileConfig,
    state: &StateStore,
    cli: &Cli,
) -> Result<()> {
    let jobs = adapter.inventory(cli.only.as_deref())?;
    println!("groups={}", jobs.len());
    for job in jobs {
        let status = if adapter.is_complete(&job, state, cli.reverify)? {
            "complete"
        } else {
            match adapter.readiness(&job)? {
                Readiness::Ready => "ready",
                Readiness::Waiting => "waiting",
            }
        };
        println!(
            "{}\t{}\t{}",
            job.id,
            status,
            profile.output_dir.join(job.output_name).display()
        );
    }
    Ok(())
}

fn run(cli: &Cli) -> Result<()> {
    let config = AppConfig::load(&cli.config)?;
    let profile = config.profile(&cli.profile)?.clone();
    let options = options(cli, &profile)?;
    let summary = run_profile(profile, &options)?;
    println!(
        "completed={} failed={} waiting={}",
        summary.completed, summary.failed, summary.waiting
    );
    Ok(())
}

fn start(cli: &Cli) -> Result<()> {
    let config = AppConfig::load(&cli.config)?;
    let profile = config.profile(&cli.profile)?;
    let limit = BatchPolicy::new(cli.limit.unwrap_or(profile.batch_limit))?;
    let executable = env::current_exe()
        .map_err(|error| PipelineError::io("locate current executable", error))?;
    if let Some(job_id) = cli.only.as_deref() {
        start_job_service(&executable, &cli.config, profile, limit, job_id)?;
    } else {
        start_service(&executable, &cli.config, profile, limit)?;
    }
    println!(
        "started profile={} batch_limit={}",
        profile.id,
        limit.limit()
    );
    Ok(())
}

fn stop(cli: &Cli) -> Result<()> {
    let config = AppConfig::load(&cli.config)?;
    let profile = config.profile(&cli.profile)?;
    request_stop(profile)?;
    println!(
        "graceful stop requested for profile={}; the current safe unit of work will finish",
        profile.id
    );
    Ok(())
}

fn status(cli: &Cli) -> Result<()> {
    let config = AppConfig::load(&cli.config)?;
    let status = profile_status(config.profile(&cli.profile)?)?;
    output::status(&status);
    Ok(())
}

fn options(cli: &Cli, profile: &rom_pipeline_core::ProfileConfig) -> Result<RunOptions> {
    Ok(RunOptions {
        limit: BatchPolicy::new(cli.limit.unwrap_or(profile.batch_limit))?,
        only_job: cli.only.clone(),
        reverify: cli.reverify,
        wait_for_source: cli.wait_for_source,
    })
}
