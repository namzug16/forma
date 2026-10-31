mod cli;
mod diagnostics;
mod project;

use std::process::ExitCode;

use clap::Parser;
use diagnostics::{Diagnostic, Failure};

fn main() -> ExitCode {
    let arguments = cli::Cli::parse();
    let emit_json = arguments.json;
    match cli::run(arguments) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            report_failure(&error, emit_json);
            ExitCode::FAILURE
        }
    }
}

fn report_failure(error: &anyhow::Error, emit_json: bool) {
    let diagnostics = match error.downcast_ref::<Failure>() {
        Some(failure) => failure.0.clone(),
        None => vec![Diagnostic::new("error", format!("{error:#}"))],
    };
    for diagnostic in &diagnostics {
        eprintln!("error: {diagnostic}");
    }
    if emit_json {
        let result = serde_json::json!({ "success": false, "errors": diagnostics });
        println!("{result:#}");
    }
}
