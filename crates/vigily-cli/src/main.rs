//! `vigily`: the command line. It talks to the engine that serves the repository (see the
//! library, `vigily_cli`).

use std::process::ExitCode;

use clap::Parser;
use vigily_cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    match vigily_cli::run(Cli::parse()).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
