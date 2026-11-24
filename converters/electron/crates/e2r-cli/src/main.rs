//! `e2r` — convert Electron desktop apps to Rust (Tauri 2).
//!
//! Two subcommands:
//!
//! * `scan` — X-ray an Electron app and report how it maps to Tauri 2,
//!   with a feasibility score. Prints a human summary by default, or
//!   JSON with `--json`.
//! * `convert` — generate a working Tauri 2 project from the scanned app
//!   (currently a stub; the converter lands in a follow-up).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use e2r_lib::{convert, scan};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "e2r",
    version,
    about = "Convert Electron desktop apps to Rust (Tauri 2)",
    long_about = "Scan an Electron app, get a feasibility report, and generate a working Tauri 2 project — plug and play."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan an Electron app and report Tauri 2 migration feasibility.
    Scan {
        /// Directory of the Electron app to scan.
        app_dir: PathBuf,
        /// Print the report as JSON instead of a human summary.
        #[arg(long)]
        json: bool,
        /// Write the report to this file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Convert an Electron app into a Tauri 2 project.
    Convert {
        /// Directory of the Electron app to convert.
        app_dir: PathBuf,
        /// Directory to generate the Tauri 2 project in.
        #[arg(long)]
        out: PathBuf,
    },
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Scan { app_dir, json, out } => {
            let report = scan::scan(&app_dir)?;
            let text = if json {
                serde_json::to_string_pretty(&report)
                    .context("failed to serialize scan report as JSON")?
            } else {
                scan::format_human_report(&report)
            };
            match out {
                Some(path) => {
                    if let Some(parent) = path.parent() {
                        if !parent.as_os_str().is_empty() {
                            fs::create_dir_all(parent).with_context(|| {
                                format!("failed to create {}", parent.display())
                            })?;
                        }
                    }
                    fs::write(&path, text)
                        .with_context(|| format!("failed to write {}", path.display()))?;
                    println!("Wrote scan report to {}", path.display());
                }
                None => print!("{text}"),
            }
        }
        Commands::Convert { app_dir, out } => {
            let report = convert::convert(&app_dir, &out)?;
            println!("Converted {} → {}", app_dir.display(), out.display());
            println!("Files written: {}", report.files_written.len());
            for file in &report.files_written {
                println!("  {}", file.display());
            }
            if !report.warnings.is_empty() {
                println!("Warnings ({}):", report.warnings.len());
                for warning in &report.warnings {
                    println!("  - {warning}");
                }
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
