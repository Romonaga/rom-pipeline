mod args;
mod commands;
mod output;

#[tokio::main]
async fn main() {
    let result = args::Cli::parse().and_then(commands::execute);
    match result {
        Ok(commands::Execution::Complete) => {}
        Ok(commands::Execution::Serve { config, executable }) => {
            if let Err(error) = rom_pipeline_web::serve(&config, &executable).await {
                output::fatal(&error);
            }
        }
        Err(error) => output::fatal(&error),
    }
}
