use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::json;

use crate::document;
use crate::project::{self, Project};

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
    /// Create a forma/ project
    Init {
        /// Directory to initialize (default: current directory)
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Print the resolved design tokens
    Tokens,
    /// Print the document JSON Schema
    Schema,
    /// Print the forma version
    Version,
}

pub fn run(arguments: Cli) -> Result<ExitCode> {
    match &arguments.command {
        Some(Command::Init { path }) => run_init(path.clone(), arguments.json)?,
        Some(Command::Tokens) => run_tokens(&arguments)?,
        Some(Command::Schema) => println!("{:#}", document::json_schema()),
        Some(Command::Version) | None => println!("forma {}", env!("CARGO_PKG_VERSION")),
    }
    Ok(ExitCode::SUCCESS)
}

fn open_project(arguments: &Cli) -> Result<Project> {
    let current_directory = env::current_dir().context("cannot read current directory")?;
    Project::locate(arguments.project.as_deref(), &current_directory)
}

fn run_tokens(arguments: &Cli) -> Result<()> {
    let project = open_project(arguments)?;
    println!("{:#}", project.tokens()?.resolved()?);
    Ok(())
}

fn run_init(path: Option<PathBuf>, emit_json: bool) -> Result<()> {
    let root = path.unwrap_or_else(|| PathBuf::from("."));
    let report = project::initialize(&root)?;
    if emit_json {
        let result = json!({
            "success": true,
            "created": report.created,
            "alreadyPresent": report.already_present,
        });
        println!("{result:#}");
        return Ok(());
    }
    for path in &report.created {
        println!("created  {}", path.display());
    }
    for path in &report.already_present {
        println!("exists   {}", path.display());
    }
    Ok(())
}
