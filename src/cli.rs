use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "forma", about = "Agent-first, headless 2D design engine")]
pub struct Cli {
    /// Project root containing forma/config.json (default: search upward)
    #[arg(long, global = true, value_name = "PATH")]
    pub project: Option<PathBuf>,

    /// Emit a stable JSON result on stdout
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Print the forma version
    Version,
}

pub fn run(arguments: Cli) -> Result<ExitCode> {
    match arguments.command {
        Some(Command::Version) | None => println!("forma {}", env!("CARGO_PKG_VERSION")),
    }
    Ok(ExitCode::SUCCESS)
}
