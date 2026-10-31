use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::json;

use crate::document;
use crate::frames::{self, FrameFile};
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
    /// List frames (the default command)
    List,
    /// Check frames without rendering
    Validate {
        /// Frame name, path relative to the frames directory, or explicit ./ path
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        frame: Option<String>,
        /// Validate every frame
        #[arg(long)]
        all: bool,
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
        Some(Command::List) | None => run_list(&arguments)?,
        Some(Command::Validate { frame, .. }) => {
            return run_validate(&arguments, frame.as_deref());
        }
        Some(Command::Version) => println!("forma {}", env!("CARGO_PKG_VERSION")),
    }
    Ok(ExitCode::SUCCESS)
}

fn open_project(arguments: &Cli) -> Result<Project> {
    let current_directory = env::current_dir().context("cannot read current directory")?;
    Project::locate(arguments.project.as_deref(), &current_directory)
}

fn selected_frames(project: &Project, frame: Option<&str>) -> Result<Vec<FrameFile>> {
    match frame {
        Some(reference) => Ok(vec![frames::resolve_reference(project, reference)?]),
        None => frames::list(project),
    }
}

fn run_list(arguments: &Cli) -> Result<()> {
    let project = open_project(arguments)?;
    let tokens = project.tokens()?;
    let rows = frames::list(&project)?
        .iter()
        .map(|file| {
            let name = file.display_name();
            match frames::load_scene(file, &tokens) {
                Ok(scene) => (name, Some((scene.width, scene.height)), Vec::new()),
                Err(failure) => (name, None, failure.0),
            }
        })
        .collect::<Vec<_>>();

    if arguments.json {
        let rows = rows
            .iter()
            .map(|(name, size, errors)| {
                json!({
                    "frame": name,
                    "width": size.map(|size| size.0),
                    "height": size.map(|size| size.1),
                    "valid": errors.is_empty(),
                    "errors": errors,
                })
            })
            .collect::<Vec<_>>();
        println!("{:#}", json!({ "frames": rows }));
        return Ok(());
    }

    println!("Forma — {} frames\n", rows.len());
    println!("{:<30} {:<12} STATUS", "FRAME", "SIZE");
    for (name, size, _) in &rows {
        let (size, status) = match size {
            Some((width, height)) => (format!("{width}×{height}"), "valid"),
            None => ("-".to_owned(), "invalid"),
        };
        println!("{name:<30} {size:<12} {status}");
    }
    if let Some((name, ..)) = rows.first() {
        println!("\nRender: forma render {name}");
    }
    Ok(())
}

fn run_validate(arguments: &Cli, frame: Option<&str>) -> Result<ExitCode> {
    let project = open_project(arguments)?;
    let tokens = project.tokens()?;
    let mut all_valid = true;
    let mut results = Vec::new();
    for file in selected_frames(&project, frame)? {
        let name = file.display_name();
        let (errors, warnings) = match frames::load_scene(&file, &tokens) {
            Ok(scene) => (Vec::new(), scene.warnings),
            Err(failure) => (failure.0, Vec::new()),
        };
        all_valid &= errors.is_empty();
        for error in &errors {
            eprintln!("{name}: error: {error}");
        }
        for warning in &warnings {
            eprintln!("{name}: warning: {warning}");
        }
        if errors.is_empty() && !arguments.json {
            println!("{name}: valid");
        }
        results.push(json!({
            "frame": name,
            "valid": errors.is_empty(),
            "errors": errors,
            "warnings": warnings,
        }));
    }
    if arguments.json {
        println!("{:#}", json!({ "success": all_valid, "frames": results }));
    }
    Ok(if all_valid {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
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
