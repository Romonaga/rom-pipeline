use rom_pipeline_core::PipelineError;
use rom_pipeline_service::ProfileStatus;

pub fn help() {
    println!(
        "\
ROM Pipeline

Usage:
  rom-pipeline doctor [options]
  rom-pipeline validate [options]
  rom-pipeline inventory [options]
  rom-pipeline publish [COUNT] [options]
  rom-pipeline prune [COUNT] --confirm-prune [options]
  rom-pipeline run [COUNT] [options]
  rom-pipeline start [COUNT] [options]
  rom-pipeline stop [options]
  rom-pipeline status [options]
  rom-pipeline serve [options]

Options:
  --config FILE       Configuration file
  --profile ID        Profile to use (default: wiiu)
  --limit N           Completed titles in this run
  --only GROUP        Process one internal group ID
  --reverify          Rehash completed outputs
  --no-wait           Do not wait for unfinished downloads
  --confirm-prune     Confirm permanent deletion of verified source files

The default batch limit comes from the selected profile."
    );
}

pub fn status(status: &ProfileStatus) {
    println!("profile={}", status.profile);
    println!("service={}", status.service);
    println!("activity={}", status.activity);
    println!("current={}", status.current);
    println!("batch_limit={}", status.batch_limit);
    println!("groups={}/{}", status.completed_groups, status.total_groups);
    println!("output_files={}", status.output_files);
    println!(
        "source_archives_moved_to_done={}",
        status.source_archives_moved_to_done
    );
    println!("current_run_failures={}", status.current_run_failures);
    if let Some(publication) = &status.publication {
        println!(
            "publication={}/{} remaining={} ready={} partial={} phase={}",
            publication.published,
            publication.total,
            publication.remaining,
            publication.ready,
            publication.partial_files,
            publication.phase
        );
        println!(
            "publication_current={}",
            publication.current_game.as_deref().unwrap_or("none")
        );
    }
    if let Some(prune) = &status.prune {
        println!(
            "prune={}/{} remaining={} phase={}",
            prune.removed, prune.total, prune.remaining, prune.phase
        );
        println!(
            "prune_current={}",
            prune.current_game.as_deref().unwrap_or("none")
        );
    }
    println!(
        "active_worker={}",
        status.active_worker.as_deref().unwrap_or("none")
    );
    println!("log={}", status.log);
}

pub fn fatal(error: &PipelineError) -> ! {
    eprintln!("error: {error}");
    std::process::exit(2);
}
