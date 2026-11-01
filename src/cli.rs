use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;

use crate::diagnostics::{Diagnostic, Failure};
use crate::document;
use crate::frames::{self, FrameFile};
use crate::icons;
use crate::inspect;
use crate::project::{self, Project};
use crate::renderer;
use crate::scene::Resources;

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
    /// Render frames to image files
    Render {
        /// Frame name, path relative to the frames directory, or explicit ./ path
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        frame: Option<String>,
        /// Render every frame
        #[arg(long)]
        all: bool,
        /// Destination file
        #[arg(long, conflicts_with = "all")]
        output: Option<PathBuf>,
        /// Output format (only png in V1)
        #[arg(long)]
        format: Option<String>,
        /// Multiplies raster dimensions, not logical coordinates
        #[arg(long)]
        scale: Option<f64>,
        /// Also copy the rendered bitmap to the clipboard
        #[arg(long, conflicts_with = "all")]
        clipboard: bool,
    },
    /// Check frames without rendering
    Validate {
        /// Frame name, path relative to the frames directory, or explicit ./ path
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        frame: Option<String>,
        /// Validate every frame
        #[arg(long)]
        all: bool,
    },
    /// Report the resolved hierarchy, transforms and bounds of a frame
    Inspect {
        /// Frame name, path relative to the frames directory, or explicit ./ path
        frame: String,
        /// Report only the layer with this id
        #[arg(long)]
        layer: Option<String>,
    },
    /// Find and install Lucide icons
    Icons {
        #[command(subcommand)]
        command: IconsCommand,
    },
    /// Print the resolved design tokens
    Tokens,
    /// Print the document JSON Schema
    Schema,
    /// Print the forma version
    Version,
}

#[derive(Subcommand)]
pub enum IconsCommand {
    /// Search the Lucide collection
    Search { query: String },
    /// Show an icon's source and license
    Info { name: String },
}

pub fn run(arguments: Cli) -> Result<ExitCode> {
    match &arguments.command {
        Some(Command::Init { path }) => run_init(path.clone(), arguments.json)?,
        Some(Command::Inspect { frame, layer }) => {
            run_inspect(&arguments, frame, layer.as_deref())?;
        }
        Some(Command::Icons { command }) => run_icons(command, arguments.json)?,
        Some(Command::Tokens) => run_tokens(&arguments)?,
        Some(Command::Schema) => println!("{:#}", document::json_schema()),
        Some(Command::List) | None => run_list(&arguments)?,
        Some(Command::Render {
            frame,
            output,
            format,
            scale,
            clipboard,
            ..
        }) => {
            let options = RenderOptions {
                output: output.as_deref(),
                format: format.as_deref(),
                scale: *scale,
                copy_to_clipboard: *clipboard,
            };
            return run_render(&arguments, frame.as_deref(), &options);
        }
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
    let resources = project.resources()?;
    let rows = frames::list(&project)?
        .iter()
        .map(|file| {
            let name = file.display_name();
            match frames::load_scene(file, &resources) {
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

    let noun = if rows.len() == 1 { "frame" } else { "frames" };
    println!("Forma — {} {noun}\n", rows.len());
    println!("{:<30} {:<12} STATUS", "FRAME", "SIZE");
    for (name, size, _) in &rows {
        let (size, status) = match size {
            Some((width, height)) => (format!("{width}×{height}"), "valid"),
            None => ("-".to_owned(), "invalid"),
        };
        println!("{name:<30} {size:<12} {status}");
    }
    if let Some((name, ..)) = rows.iter().find(|(_, size, _)| size.is_some()) {
        println!("\nRender: forma render {name}");
    }
    Ok(())
}

struct RenderOptions<'a> {
    output: Option<&'a Path>,
    format: Option<&'a str>,
    scale: Option<f64>,
    copy_to_clipboard: bool,
}

fn run_render(arguments: &Cli, frame: Option<&str>, options: &RenderOptions) -> Result<ExitCode> {
    let project = open_project(arguments)?;
    let format = options.format.unwrap_or(&project.config.render.format);
    if format != "png" {
        return Err(Failure::from(Diagnostic::new(
            "unsupported_format",
            format!("format \"{format}\" is not supported; V1 renders png"),
        ))
        .into());
    }
    let resources = project.resources()?;
    let files = selected_frames(&project, frame)?;

    if frame.is_some() {
        let [file] = files.as_slice() else {
            unreachable!("one reference selects one frame");
        };
        let result = render_frame(&project, &resources, file, options)?;
        print_render_result(&result, arguments.json);
        return Ok(ExitCode::SUCCESS);
    }

    let mut all_succeeded = true;
    let mut results = Vec::new();
    for file in &files {
        match render_frame(&project, &resources, file, options) {
            Ok(result) => {
                print_render_result(&result, false);
                results.push(json!(result));
            }
            Err(error) => {
                all_succeeded = false;
                eprintln!("{}: error: {error:#}", file.display_name());
                let errors = error.downcast_ref::<Failure>().map_or_else(
                    || vec![Diagnostic::new("error", format!("{error:#}"))],
                    |f| f.0.clone(),
                );
                results.push(json!({
                    "success": false,
                    "frame": file.display_name(),
                    "errors": errors,
                }));
            }
        }
    }
    if arguments.json {
        println!(
            "{:#}",
            json!({ "success": all_succeeded, "results": results })
        );
    }
    Ok(if all_succeeded {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[derive(Serialize)]
struct RenderResult {
    success: bool,
    frame: String,
    output: PathBuf,
    width: u32,
    height: u32,
    format: &'static str,
    warnings: Vec<Diagnostic>,
}

fn render_frame(
    project: &Project,
    resources: &Resources,
    file: &FrameFile,
    options: &RenderOptions,
) -> Result<RenderResult> {
    let scene = frames::load_scene(file, resources)?;
    let scale = options.scale.unwrap_or(project.config.render.scale);
    let canvas = renderer::render(&scene, scale).map_err(Failure::from)?;
    let output = match options.output {
        Some(path) => absolute_output_path(path)?,
        None => project.default_output_path(file),
    };
    if output == file.path || project.is_source_path(&output) {
        return Err(Failure::from(Diagnostic::new(
            "output_overwrites_source",
            format!("refusing to write over project source {}", output.display()),
        ))
        .into());
    }
    renderer::write_png(&canvas, &output)?;
    let mut warnings = scene.warnings;
    if options.copy_to_clipboard {
        // The file is already saved, so a missing clipboard must not fail the render.
        if let Err(error) = renderer::copy_to_clipboard(&canvas) {
            warnings.push(Diagnostic::new(
                "clipboard_unavailable",
                format!("the render was saved but not copied: {error}"),
            ));
        }
    }
    Ok(RenderResult {
        success: true,
        frame: scene.id,
        output,
        width: canvas.width(),
        height: canvas.height(),
        format: "png",
        warnings,
    })
}

/// Resolves symlinks in the parent so a link cannot smuggle a write into the source tree.
fn absolute_output_path(path: &Path) -> Result<PathBuf> {
    let absolute = env::current_dir()
        .context("cannot read current directory")?
        .join(path);
    let parent = absolute
        .parent()
        .context("output path has no parent directory")?;
    let file_name = absolute
        .file_name()
        .context("output path has no file name")?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let parent = parent
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", parent.display()))?;
    Ok(parent.join(file_name))
}

fn print_render_result(result: &RenderResult, emit_json: bool) {
    for warning in &result.warnings {
        eprintln!("warning: {warning}");
    }
    if emit_json {
        println!("{:#}", json!(result));
    } else {
        println!(
            "{} ({}×{})",
            result.output.display(),
            result.width,
            result.height
        );
    }
}

fn run_validate(arguments: &Cli, frame: Option<&str>) -> Result<ExitCode> {
    let project = open_project(arguments)?;
    let resources = project.resources()?;
    let mut all_valid = true;
    let mut results = Vec::new();
    for file in selected_frames(&project, frame)? {
        let name = file.display_name();
        let (errors, warnings) = match frames::load_scene(&file, &resources) {
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

fn run_inspect(arguments: &Cli, frame: &str, layer: Option<&str>) -> Result<()> {
    let project = open_project(arguments)?;
    let resources = project.resources()?;
    let file = frames::resolve_reference(&project, frame)?;
    let scene = frames::load_scene(&file, &resources)?;
    let report = inspect::report(&scene, layer)?;
    if arguments.json {
        println!("{report:#}");
    } else {
        inspect::print_outline(&report);
    }
    Ok(())
}

fn run_icons(command: &IconsCommand, emit_json: bool) -> Result<()> {
    match command {
        IconsCommand::Search { query } => {
            let names = icons::search(query).map_err(Failure::from)?;
            if emit_json {
                println!("{:#}", json!({ "icons": names }));
            } else {
                names.iter().for_each(|name| println!("{name}"));
            }
        }
        IconsCommand::Info { name } => {
            let info = icons::info(name).map_err(Failure::from)?;
            if emit_json {
                println!("{:#}", json!(info));
            } else {
                println!("{} ({})", info.name, info.collection);
                println!("license  {} {}", info.license, info.license_url);
                println!("source   {}", info.source);
            }
        }
    }
    Ok(())
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
