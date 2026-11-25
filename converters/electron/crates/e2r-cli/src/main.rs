//! `e2r` — convert Electron desktop apps to Rust (Tauri 2).
//!
//! Five subcommands:
//!
//! * `scan` — X-ray an Electron app and report how it maps to Tauri 2,
//!   with a feasibility score. Prints a human summary by default, or
//!   JSON with `--json`.
//! * `simulate` — predict the migration's shape and payoff before any
//!   code changes: migratable breakdown, UI preservation estimate,
//!   performance predictions with confidence ranges, warnings, and an
//!   effort hint. Prints a human summary by default; `--format` picks
//!   `human`, `json`, or `markdown`, and `--json` is a shorthand for
//!   `--format json`.
//! * `calibrate` — compare a predictions record (from
//!   `simulate --record`) against measured post-migration actuals
//!   (`measurements.json`), report per-metric error, and append the
//!   comparisons to the calibration log so future simulations adjust
//!   their confidence intervals.
//! * `convert` — generate a working Tauri 2 project from the scanned app
//!   (Rust backend, TypeScript bridge, carried-over renderer, migration
//!   notes).
//! * `serve` — run the MCP server on stdio, exposing the scanner,
//!   simulator, mappings database, validator, and decision registry to
//!   AI agents as six tools (see `e2r_lib::mcp`).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use e2r_lib::simulate::{CalibrationInput, ReportFormat, SimulateOptions, TargetPlatform};
use e2r_lib::{calibrate, convert, mcp, scan, simulate};
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
    /// Simulate the migration before any code changes: migratable
    /// breakdown, UI preservation estimate, predicted gains with
    /// confidence ranges, warnings, and an effort hint.
    Simulate {
        /// Directory of the Electron app to simulate.
        app_dir: PathBuf,
        /// Print the report as JSON instead of a human summary
        /// (shorthand for `--format json`; wins over `--format`).
        #[arg(long)]
        json: bool,
        /// Report format: `human` (default), `json`, or `markdown`
        /// (condensed, for pasting into issues and PRs).
        #[arg(long, value_enum)]
        format: Option<FormatArg>,
        /// Target platform for the per-platform predictions (memory,
        /// process count): `macos`, `windows`, or `linux`. Defaults to
        /// the platform this binary runs on.
        #[arg(long, value_enum)]
        target_platform: Option<PlatformArg>,
        /// Calibration log to read for interval adjustment. Defaults
        /// to `~/.cache/e2r/calibration.jsonl` (or
        /// `E2R_CALIBRATION_LOG` when set). Use `--no-calibration` to
        /// simulate with the model's unadjusted intervals.
        #[arg(long)]
        calibration_log: Option<PathBuf>,
        /// Ignore the calibration log: simulate with the model's
        /// unadjusted intervals.
        #[arg(long)]
        no_calibration: bool,
        /// Write the report to this file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Also write a predictions-record JSON (for the calibration
        /// loop) to this file.
        #[arg(long)]
        record: Option<PathBuf>,
    },
    /// Calibrate the simulator against measured post-migration actuals:
    /// compare a predictions record (from `simulate --record`) with a
    /// measurements file, report per-metric error, and append the
    /// comparisons to the calibration log.
    Calibrate {
        /// Directory of the Electron app that was simulated.
        app_dir: PathBuf,
        /// Measurements file (JSON): the measured post-migration
        /// actuals. See converters/electron/README.md for the schema.
        #[arg(long)]
        actual: PathBuf,
        /// Predictions-record JSON (from `simulate --record`).
        /// Defaults to `<app-dir>/.e2r/predictions.json`.
        #[arg(long)]
        record: Option<PathBuf>,
        /// Calibration log to append to. Defaults to
        /// `~/.cache/e2r/calibration.jsonl` (or `E2R_CALIBRATION_LOG`
        /// when set).
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Convert an Electron app into a Tauri 2 project.
    Convert {
        /// Directory of the Electron app to convert.
        app_dir: PathBuf,
        /// Directory to generate the Tauri 2 project in.
        #[arg(long)]
        out: PathBuf,
    },
    /// Run the MCP server on stdio: six tools for AI agents
    /// (`analyse_electron_app`, `get_migration_plan`,
    /// `get_rust_replacement`, `simulate_migration`, `validate_migration`,
    /// `get_decision_log`). Runs until the client disconnects.
    Serve,
}

/// `--format` values for `e2r simulate`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum FormatArg {
    Human,
    Json,
    Markdown,
}

impl From<FormatArg> for ReportFormat {
    fn from(arg: FormatArg) -> Self {
        match arg {
            FormatArg::Human => ReportFormat::Human,
            FormatArg::Json => ReportFormat::Json,
            FormatArg::Markdown => ReportFormat::Markdown,
        }
    }
}

/// `--target-platform` values for `e2r simulate`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum PlatformArg {
    Macos,
    Windows,
    Linux,
}

impl From<PlatformArg> for TargetPlatform {
    fn from(arg: PlatformArg) -> Self {
        match arg {
            PlatformArg::Macos => TargetPlatform::Macos,
            PlatformArg::Windows => TargetPlatform::Windows,
            PlatformArg::Linux => TargetPlatform::Linux,
        }
    }
}

/// Write `text` to `path`, creating parent directories as needed.
fn write_text_file(path: &PathBuf, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
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
        Commands::Simulate {
            app_dir,
            json,
            format,
            target_platform,
            calibration_log,
            no_calibration,
            out,
            record,
        } => {
            let calibration = if no_calibration {
                CalibrationInput::Disabled
            } else if let Some(path) = calibration_log {
                CalibrationInput::Path(path)
            } else {
                CalibrationInput::Default
            };
            let options = SimulateOptions {
                target_platform: target_platform
                    .map(TargetPlatform::from)
                    .unwrap_or_else(TargetPlatform::host),
                calibration,
            };
            let report = simulate::simulate_with_options(&app_dir, &options)?;
            let fmt = if json {
                ReportFormat::Json
            } else {
                format
                    .map(ReportFormat::from)
                    .unwrap_or(ReportFormat::Human)
            };
            let text = simulate::format_report(&report, fmt);
            match out {
                Some(path) => {
                    write_text_file(&path, &text)?;
                    println!("Wrote simulation report to {}", path.display());
                }
                None => print!("{text}"),
            }
            if let Some(record_path) = record {
                let record = simulate::build_record(&report, &app_dir);
                let record_json = serde_json::to_string_pretty(&record)
                    .context("failed to serialize predictions record as JSON")?;
                write_text_file(&record_path, &record_json)?;
                println!("Wrote predictions record to {}", record_path.display());
            }
        }
        Commands::Calibrate {
            app_dir,
            actual,
            record,
            log,
        } => {
            let report =
                calibrate::calibrate(&app_dir, record.as_deref(), &actual, log.as_deref())?;
            print!("{}", calibrate::format_human_report(&report));
        }
        Commands::Serve => {
            // The MCP server is async (rmcp + tokio); the rest of the
            // CLI is synchronous, so `serve` builds a small runtime
            // just for itself instead of making `main` async.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("failed to build async runtime for the MCP server")?;
            runtime.block_on(mcp::serve_stdio())?;
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
