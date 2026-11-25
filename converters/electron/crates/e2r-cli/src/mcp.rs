//! MCP server (`e2r serve`): expose the scanner, simulator, converter
//! knowledge, and decision registry to AI agents over the Model Context
//! Protocol (stdio transport).
//!
//! The server is built on [`rmcp`] (the official Rust MCP SDK): the
//! transport, protocol negotiation, and JSON-RPC framing are the SDK's
//! job. The six tool handlers are plain synchronous functions in this
//! module — [`analyse_electron_app`], [`get_migration_plan`],
//! [`get_rust_replacement`], [`simulate_migration`],
//! [`validate_migration`], and [`get_decision_log`] — so they are
//! testable without any transport at all; the `#[tool]` methods on
//! [`E2rServer`] are a thin adapter that turns a handler's
//! `Result<Value, ToolError>` into a [`CallToolResult`].
//!
//! Every tool returns structured JSON: success as
//! `CallToolResult::structured`, failure as
//! `CallToolResult::structured_error` with `isError: true` and a
//! structured error object (`{"code", "message", "details?"}` — see
//! [`ToolError`]). No handler panics on bad input: a missing directory,
//! an unknown API, or a malformed argument is a structured error, never
//! a crash.
//!
//! The six tools:
//!
//! * `analyse_electron_app` — scan an Electron app, return the full
//!   [`ScanReport`](crate::model::ScanReport) JSON.
//! * `get_migration_plan` — scan + simulate, composed into an ordered
//!   migration plan (steps, per-API replacements, MANUAL items first,
//!   effort hint).
//! * `get_rust_replacement` — look one Electron API up in the mappings
//!   database (or dump the whole table with `list_all`).
//! * `simulate_migration` — predict the migration's shape and payoff
//!   (full [`SimulationReport`](crate::model::SimulationReport) JSON).
//! * `validate_migration` — re-check a converted Tauri project without
//!   running `cargo check` (too slow for an MCP tool): expected files,
//!   `TODO(e2r)` stubs remaining, MANUAL work left.
//! * `get_decision_log` — read the decision registry
//!   (see [`crate::decisions`]).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    schemars::JsonSchema,
    tool, tool_handler, tool_router,
    transport::stdio,
    ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};

use crate::model::Classification;
use crate::simulate::{CalibrationInput, SimulateOptions, TargetPlatform};

// ---------------------------------------------------------------------------
// Tool errors
// ---------------------------------------------------------------------------

/// A structured tool error: the `structured_content` of an `isError`
/// tool result, and the `text` content too (rmcp renders the JSON as the
/// text block, so clients without structured-content support still see
/// the error).
///
/// Codes are stable strings agents can match on:
///
/// * `INVALID_ARGUMENT` — a required argument is missing, empty, or
///   malformed (e.g. an unknown `target_platform`).
/// * `NOT_FOUND` — a directory or file the tool needs does not exist.
/// * `API_NOT_FOUND` — no mapping-table row for the requested API.
/// * `NOT_E2R_PROJECT` — `validate_migration` was pointed at a directory
///   that is not an e2r-generated Tauri project.
/// * `SCAN_FAILED` / `SIMULATION_FAILED` — the underlying scan or
///   simulation failed; `message` carries the reason.
/// * `IO_ERROR` — a filesystem operation failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    /// Stable machine-readable code (see the type docs).
    pub code: &'static str,
    /// Human-readable explanation, in one or two sentences.
    pub message: String,
    /// Extra structured detail (e.g. the API that was not found), when
    /// there is any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ToolError {
    /// Build a tool error with no extra detail.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    /// Attach structured detail to the error.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// The error as the structured JSON object clients receive.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            serde_json::json!({
                "code": "INTERNAL_ERROR",
                "message": "failed to serialize tool error",
            })
        })
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ToolError {}

/// Check that `dir` exists and is a directory; otherwise a `NOT_FOUND`
/// tool error naming the argument and the path.
fn require_dir(dir: &Path, arg: &str) -> std::result::Result<(), ToolError> {
    if !dir.is_dir() {
        return Err(ToolError::new(
            "NOT_FOUND",
            format!(
                "{arg} '{}' does not exist or is not a directory",
                dir.display()
            ),
        )
        .with_details(serde_json::json!({ "argument": arg, "path": dir.display().to_string() })));
    }
    Ok(())
}

/// Check that a required string argument is non-empty after trimming.
fn require_non_empty(value: &str, arg: &str) -> std::result::Result<(), ToolError> {
    if value.trim().is_empty() {
        return Err(ToolError::new(
            "INVALID_ARGUMENT",
            format!("argument '{arg}' must not be empty"),
        ));
    }
    Ok(())
}

/// Turn a handler outcome into the [`CallToolResult`] the wire carries:
/// structured JSON on success, `isError` + structured error object on
/// failure.
fn result_from(outcome: std::result::Result<serde_json::Value, ToolError>) -> CallToolResult {
    match outcome {
        Ok(value) => CallToolResult::structured(value),
        Err(err) => CallToolResult::structured_error(err.to_json()),
    }
}

// ---------------------------------------------------------------------------
// Tool handlers (transport-free, directly testable)
// ---------------------------------------------------------------------------

/// Analyse an Electron app: run the scanner and return the full
/// [`ScanReport`](crate::model::ScanReport) as JSON.
///
/// Errors: `NOT_FOUND` when `app_dir` does not exist, `SCAN_FAILED`
/// when the scan itself fails.
pub fn analyse_electron_app(app_dir: &Path) -> std::result::Result<serde_json::Value, ToolError> {
    require_dir(app_dir, "app_dir")?;
    let report = crate::scan::scan(app_dir).map_err(|err| {
        ToolError::new("SCAN_FAILED", format!("scan failed: {err:#}"))
            .with_details(serde_json::json!({ "app_dir": app_dir.display().to_string() }))
    })?;
    serde_json::to_value(&report).map_err(|err| {
        ToolError::new(
            "IO_ERROR",
            format!("failed to serialize scan report: {err}"),
        )
    })
}

/// One step of a migration plan: what to do, in order, and how.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    /// 1-based step number.
    pub order: usize,
    /// Step id: `"analyse"`, `"simulate"`, `"convert"`, `"validate"`.
    pub name: String,
    /// What this step does, in one line.
    pub description: String,
    /// The `e2r` command (or MCP tool) that performs the step.
    pub command: String,
}

/// One Electron API's replacement in a migration plan, deduplicated by
/// API across all its call sites.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiReplacement {
    /// Electron API name, e.g. `"dialog.showOpenDialog"`.
    pub api: String,
    /// How convertible the API is: `"auto"`, `"assisted"`, `"manual"`.
    pub classification: Classification,
    /// The Tauri target for this API, if the mapping table knows one.
    pub tauri_target: Option<String>,
    /// Tauri plugin crate that provides the target, if any.
    pub plugin: Option<String>,
    /// One honest line about what the conversion involves.
    pub notes: String,
    /// Call sites (`"file:line"`, in scan order).
    pub files: Vec<String>,
}

/// One MANUAL item: an API with no direct Tauri equivalent, called out
/// for human work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualItem {
    /// Electron API name.
    pub api: String,
    /// Source file, relative to the app directory.
    pub file: String,
    /// 1-based line number of the call site.
    pub line: usize,
    /// One line on what the manual port involves.
    pub note: String,
}

/// The migration plan returned by [`get_migration_plan`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationPlan {
    /// App name from the scan report.
    pub app_name: String,
    /// Feasibility score, 0–100, from the scan report.
    pub feasibility_score: u8,
    /// Human-readable verdict from the scan report.
    pub verdict: String,
    /// Ordered steps: analyse → simulate → convert → validate.
    pub steps: Vec<PlanStep>,
    /// MANUAL findings, called out first: every API with no direct
    /// Tauri equivalent, in scan order.
    pub manual_first: Vec<ManualItem>,
    /// Per-API replacements (deduplicated by API), in first-seen order.
    pub replacements: Vec<ApiReplacement>,
    /// Rough effort hint from the simulator (`"trivial"`, `"small"`,
    /// `"medium"`, `"large"`).
    pub effort: crate::model::Effort,
    /// What the effort hint is based on, and its limits.
    pub effort_note: String,
    /// Migratable breakdown summary from the simulator (counts +
    /// percentages).
    pub breakdown: crate::model::Breakdown,
}

/// Compose an ordered migration plan for an Electron app: scan it,
/// simulate it, and combine the two into steps, per-API replacements,
/// MANUAL items called out first, and an effort hint.
///
/// The plan is deterministic: same app → same plan (no timestamps, no
/// unordered collections). The effort hint and breakdown come from the
/// simulator ([`simulate_migration`]); the scan is the single source of
/// truth for the replacements.
///
/// Errors: `NOT_FOUND` when `app_dir` does not exist, `SCAN_FAILED` /
/// `SIMULATION_FAILED` when the underlying scan or simulation fails.
pub fn get_migration_plan(app_dir: &Path) -> std::result::Result<serde_json::Value, ToolError> {
    require_dir(app_dir, "app_dir")?;
    let scan_report = crate::scan::scan(app_dir).map_err(|err| {
        ToolError::new("SCAN_FAILED", format!("scan failed: {err:#}"))
            .with_details(serde_json::json!({ "app_dir": app_dir.display().to_string() }))
    })?;
    let sim_report = crate::simulate::simulate(app_dir).map_err(|err| {
        ToolError::new("SIMULATION_FAILED", format!("simulation failed: {err:#}"))
            .with_details(serde_json::json!({ "app_dir": app_dir.display().to_string() }))
    })?;

    let app_dir_display = app_dir.display().to_string();
    let steps = vec![
        PlanStep {
            order: 1,
            name: "analyse".to_string(),
            description: format!(
                "Analyse '{}' (feasibility {}): know what maps to Tauri before touching code.",
                scan_report.app_name, scan_report.feasibility_score
            ),
            command: format!("e2r scan {app_dir_display}"),
        },
        PlanStep {
            order: 2,
            name: "simulate".to_string(),
            description: format!(
                "Simulate the migration: {}% auto, {}% manual, effort {:?} — decide whether to proceed.",
                sim_report.breakdown.auto_pct,
                sim_report.breakdown.manual_pct,
                sim_report.effort,
            ),
            command: format!("e2r simulate {app_dir_display}"),
        },
        PlanStep {
            order: 3,
            name: "convert".to_string(),
            description: "Generate the Tauri 2 project: Rust backend, TypeScript bridge, carried-over renderer.".to_string(),
            command: format!("e2r convert {app_dir_display} --out <output-dir>"),
        },
        PlanStep {
            order: 4,
            name: "validate".to_string(),
            description: "Validate the converted project: expected files, TODO(e2r) stubs, remaining manual work — then run cargo check.".to_string(),
            command: "validate_migration { project_dir: \"<output-dir>\", original_app_dir: \"<app-dir>\" }".to_string(),
        },
    ];

    let manual_first: Vec<ManualItem> = scan_report
        .findings
        .iter()
        .filter(|f| f.classification == Classification::Manual)
        .map(|f| ManualItem {
            api: f.api.clone(),
            file: f.file.clone(),
            line: f.line,
            note: f.note.clone(),
        })
        .collect();

    // Per-API replacements, deduplicated by API in first-seen order,
    // enriched with the plugin from the mappings table.
    let mut replacements: Vec<ApiReplacement> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for finding in &scan_report.findings {
        let site = format!("{}:{}", finding.file, finding.line);
        if let Some(&i) = index.get(&finding.api) {
            replacements[i].files.push(site);
            continue;
        }
        let (plugin, notes) = match crate::mappings::lookup(&finding.api) {
            Some(mapping) => (mapping.plugin.clone(), mapping.notes.clone()),
            None => (None, finding.note.clone()),
        };
        index.insert(finding.api.clone(), replacements.len());
        replacements.push(ApiReplacement {
            api: finding.api.clone(),
            classification: finding.classification,
            tauri_target: finding.tauri_target.clone(),
            plugin,
            notes,
            files: vec![site],
        });
    }

    let plan = MigrationPlan {
        app_name: scan_report.app_name,
        feasibility_score: scan_report.feasibility_score,
        verdict: scan_report.verdict,
        steps,
        manual_first,
        replacements,
        effort: sim_report.effort,
        effort_note: sim_report.effort_note,
        breakdown: sim_report.breakdown,
    };
    serde_json::to_value(&plan).map_err(|err| {
        ToolError::new(
            "IO_ERROR",
            format!("failed to serialize migration plan: {err}"),
        )
    })
}

/// Look one Electron API up in the mappings database, or dump the whole
/// table when `list_all` is true.
///
/// Single lookup returns the mapping row verbatim:
/// `{"electron_api", "tauri_target", "plugin", "status", "notes"}`.
/// The full dump returns `{"count": 48, "mappings": [...]}`.
///
/// Errors: `INVALID_ARGUMENT` when `api` is empty (and `list_all` is
/// false), `API_NOT_FOUND` when the table has no row for the API — never
/// a panic.
pub fn get_rust_replacement(
    api: &str,
    list_all: bool,
) -> std::result::Result<serde_json::Value, ToolError> {
    if list_all {
        let mappings = crate::mappings::load_mappings();
        let count = mappings.len();
        return Ok(serde_json::json!({ "count": count, "mappings": mappings }));
    }
    require_non_empty(api, "api")?;
    match crate::mappings::lookup(api) {
        Some(mapping) => serde_json::to_value(mapping).map_err(|err| {
            ToolError::new(
                "IO_ERROR",
                format!("failed to serialize mapping: {err}"),
            )
        }),
        None => Err(ToolError::new(
            "API_NOT_FOUND",
            format!("no Tauri mapping for Electron API '{api}' — the table is a living document; call get_rust_replacement with list_all to see every known API"),
        )
        .with_details(serde_json::json!({ "api": api }))),
    }
}

/// Simulate a migration: run the simulator and return the full
/// [`SimulationReport`](crate::model::SimulationReport) as JSON.
///
/// `target_platform` is `"macos"`, `"windows"`, or `"linux"`
/// (case-insensitive); `None` simulates for the host platform.
/// Calibration comes from the default log, exactly like
/// `e2r simulate` (a missing log behaves like no calibration).
///
/// Errors: `NOT_FOUND` when `app_dir` does not exist,
/// `INVALID_ARGUMENT` for an unknown platform, `SIMULATION_FAILED`
/// when the simulation itself fails.
pub fn simulate_migration(
    app_dir: &Path,
    target_platform: Option<&str>,
) -> std::result::Result<serde_json::Value, ToolError> {
    require_dir(app_dir, "app_dir")?;
    let platform = match target_platform {
        Some(value) => TargetPlatform::parse(value).ok_or_else(|| {
            ToolError::new(
                "INVALID_ARGUMENT",
                format!(
                    "unknown target_platform '{value}' — expected 'macos', 'windows', or 'linux'"
                ),
            )
            .with_details(serde_json::json!({ "target_platform": value }))
        })?,
        None => TargetPlatform::host(),
    };
    let options = SimulateOptions {
        target_platform: platform,
        calibration: CalibrationInput::Default,
    };
    let report = crate::simulate::simulate_with_options(app_dir, &options).map_err(|err| {
        ToolError::new("SIMULATION_FAILED", format!("simulation failed: {err:#}"))
            .with_details(serde_json::json!({ "app_dir": app_dir.display().to_string() }))
    })?;
    serde_json::to_value(&report).map_err(|err| {
        ToolError::new(
            "IO_ERROR",
            format!("failed to serialize simulation report: {err}"),
        )
    })
}

/// One file's remaining `TODO(e2r)` stubs in a validation report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoStubFile {
    /// File path, relative to the project directory.
    pub file: String,
    /// `TODO(e2r)` occurrences in the file.
    pub count: usize,
}

/// The validation report returned by [`validate_migration`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Project directory, as passed in.
    pub project_dir: String,
    /// Always `true` on success — a non-e2r project is an error, not a
    /// report (see [`ToolError`] code `NOT_E2R_PROJECT`).
    pub is_e2r_project: bool,
    /// Expected generated files that exist, relative to the project
    /// directory, in check order.
    pub carried_over: Vec<String>,
    /// Expected generated files that are missing.
    pub missing: Vec<String>,
    /// Whether a renderer was carried over (build output with an
    /// `index.html`, or renderer source files beyond the bridge).
    pub renderer_carried_over: bool,
    /// What the renderer check found, in one line.
    pub renderer_detail: String,
    /// Total `TODO(e2r)` stub occurrences in the generated Rust and
    /// TypeScript.
    pub todo_stubs: usize,
    /// Per-file stub counts (files with at least one stub).
    pub todo_stub_files: Vec<TodoStubFile>,
    /// MANUAL findings from the original app (only when
    /// `original_app_dir` was given): the manual work still to do.
    pub manual_remaining: Vec<ManualItem>,
    /// Ordered next steps, starting with the most urgent.
    pub next_steps: Vec<String>,
}

/// Files every e2r-generated project contains, relative to the project
/// directory, in check order.
const EXPECTED_FILES: &[&str] = &[
    "src-tauri/src/main.rs",
    "src-tauri/src/lib.rs",
    "src-tauri/src/commands.rs",
    "src-tauri/tauri.conf.json",
    "src-tauri/capabilities/default.json",
    "src-tauri/Cargo.toml",
    "src-tauri/build.rs",
    "src-tauri/icons/icon.png",
    "src/lib/e2r-bridge.ts",
    "package.json",
    "MIGRATION-NOTES.md",
];

/// Files searched for `TODO(e2r)` stubs: the generated Rust sources
/// plus the TypeScript bridge.
const STUB_SEARCH_FILES: &[&str] = &[
    "src-tauri/src/main.rs",
    "src-tauri/src/lib.rs",
    "src-tauri/src/commands.rs",
    "src-tauri/src/listeners.rs",
    "src-tauri/src/menu.rs",
    "src/lib/e2r-bridge.ts",
];

/// Validate a converted Tauri project WITHOUT running `cargo check`
/// (too slow for an MCP tool): check the expected generated files
/// exist, count the `TODO(e2r)` stubs remaining, detect whether the
/// renderer was carried over, and — when `original_app_dir` is given —
/// re-scan the original app and list its MANUAL findings as the
/// remaining manual work.
///
/// A directory is recognised as e2r-generated when it has
/// `src-tauri/tauri.conf.json` plus at least one e2r marker
/// (`.e2r/decisions.jsonl`, `MIGRATION-NOTES.md`, or
/// `src/lib/e2r-bridge.ts`).
///
/// Errors: `NOT_FOUND` when a directory does not exist,
/// `NOT_E2R_PROJECT` when the directory is not an e2r-generated
/// project, `SCAN_FAILED` when the original app cannot be re-scanned.
pub fn validate_migration(
    project_dir: &Path,
    original_app_dir: Option<&Path>,
) -> std::result::Result<serde_json::Value, ToolError> {
    require_dir(project_dir, "project_dir")?;
    if let Some(original) = original_app_dir {
        require_dir(original, "original_app_dir")?;
    }

    // Is this an e2r-generated project at all?
    if !project_dir.join("src-tauri/tauri.conf.json").is_file() {
        return Err(ToolError::new(
            "NOT_E2R_PROJECT",
            format!(
                "'{}' has no src-tauri/tauri.conf.json — it is not a Tauri project, let alone an e2r-generated one",
                project_dir.display()
            ),
        )
        .with_details(serde_json::json!({ "project_dir": project_dir.display().to_string() })));
    }
    let has_marker = project_dir.join(".e2r/decisions.jsonl").is_file()
        || project_dir.join("MIGRATION-NOTES.md").is_file()
        || project_dir.join("src/lib/e2r-bridge.ts").is_file();
    if !has_marker {
        return Err(ToolError::new(
            "NOT_E2R_PROJECT",
            format!(
                "'{}' is a Tauri project but has no e2r marker (no .e2r/decisions.jsonl, no MIGRATION-NOTES.md, no src/lib/e2r-bridge.ts) — it was not generated by e2r",
                project_dir.display()
            ),
        )
        .with_details(serde_json::json!({ "project_dir": project_dir.display().to_string() })));
    }

    let mut carried_over = Vec::new();
    let mut missing = Vec::new();
    for rel in EXPECTED_FILES {
        if project_dir.join(rel).is_file() {
            carried_over.push(rel.to_string());
        } else {
            missing.push(rel.to_string());
        }
    }

    // Renderer carry-over: build output with an index.html, or renderer
    // source files beyond the generated bridge.
    let mut renderer_carried_over = false;
    let mut renderer_detail = String::new();
    for rel in ["dist/index.html", "build/index.html", "out/index.html"] {
        if project_dir.join(rel).is_file() {
            renderer_carried_over = true;
            renderer_detail = format!("build output carried over ({rel} exists)");
            break;
        }
    }
    if !renderer_carried_over {
        let src = project_dir.join("src");
        let extra_files = count_files_except(&src, project_dir, &["src/lib/e2r-bridge.ts"]);
        if extra_files > 0 {
            renderer_carried_over = true;
            renderer_detail = format!(
                "renderer source carried over ({extra_files} file(s) under src/ besides the generated bridge)"
            );
        } else {
            renderer_detail =
                "no renderer files found (no build output, no renderer source under src/)"
                    .to_string();
        }
    }

    // TODO(e2r) stubs: count substring occurrences in the generated
    // Rust and TypeScript. Unreadable files count as zero stubs rather
    // than failing the validation.
    let mut todo_stubs = 0usize;
    let mut todo_stub_files = Vec::new();
    for rel in STUB_SEARCH_FILES {
        let path = project_dir.join(rel);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let count = text.matches("TODO(e2r)").count();
        if count > 0 {
            todo_stubs += count;
            todo_stub_files.push(TodoStubFile {
                file: rel.to_string(),
                count,
            });
        }
    }

    // Remaining manual work: re-scan the original app when given.
    let mut manual_remaining = Vec::new();
    if let Some(original) = original_app_dir {
        let scan_report = crate::scan::scan(original).map_err(|err| {
            ToolError::new(
                "SCAN_FAILED",
                format!("re-scan of original app failed: {err:#}"),
            )
            .with_details(serde_json::json!({ "original_app_dir": original.display().to_string() }))
        })?;
        manual_remaining = scan_report
            .findings
            .iter()
            .filter(|f| f.classification == Classification::Manual)
            .map(|f| ManualItem {
                api: f.api.clone(),
                file: f.file.clone(),
                line: f.line,
                note: f.note.clone(),
            })
            .collect();
    }

    // Next steps, most urgent first.
    let mut next_steps: Vec<String> = Vec::new();
    if !missing.is_empty() {
        next_steps.push(format!(
            "Restore {} missing file(s): {}",
            missing.len(),
            missing.join(", ")
        ));
    }
    if todo_stubs > 0 {
        next_steps.push(format!(
            "Port the {todo_stubs} TODO(e2r) stub(s) in src-tauri/src/ (see MIGRATION-NOTES.md for the original handler locations)"
        ));
    }
    if !manual_remaining.is_empty() {
        next_steps.push(format!(
            "Port {} MANUAL API(s) by hand — they have no direct Tauri equivalent (see manual_remaining)",
            manual_remaining.len()
        ));
    }
    if !renderer_carried_over {
        next_steps.push(
            "Add the real frontend: copy the renderer source into src/ (or point build.frontendDist in src-tauri/tauri.conf.json at existing build output)".to_string(),
        );
    }
    next_steps.push(format!(
        "Run `cargo check` in {}/src-tauri to verify the generated Rust compiles",
        project_dir.display()
    ));
    next_steps.push(
        "Run the frontend build (if any), then `cargo tauri dev` to launch the converted app"
            .to_string(),
    );

    let report = ValidationReport {
        project_dir: project_dir.display().to_string(),
        is_e2r_project: true,
        carried_over,
        missing,
        renderer_carried_over,
        renderer_detail,
        todo_stubs,
        todo_stub_files,
        manual_remaining,
        next_steps,
    };
    serde_json::to_value(&report).map_err(|err| {
        ToolError::new(
            "IO_ERROR",
            format!("failed to serialize validation report: {err}"),
        )
    })
}

/// Count regular files under `dir` (recursively), excluding files whose
/// path relative to `project_dir` is in `exclude` (e.g. the generated
/// bridge, which is not renderer content). A missing directory counts
/// as zero files; unreadable entries are skipped.
fn count_files_except(dir: &Path, project_dir: &Path, exclude: &[&str]) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut count = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            count += count_files_except(&path, project_dir, exclude);
        } else if path.is_file() {
            let rel = path
                .strip_prefix(project_dir)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if !exclude.contains(&rel.as_str()) {
                count += 1;
            }
        }
    }
    count
}

/// Read the decision registry: entries in file order (oldest first,
/// newest last), plus the count and the number of corrupt lines
/// skipped.
///
/// `project_dir` selects `<project-dir>/.e2r/decisions.jsonl`; with no
/// project directory the shared log is read (see
/// [`crate::decisions::default_path`]). A missing log reads as an empty
/// list, not an error.
///
/// Errors: `NOT_FOUND` when the given `project_dir` does not exist.
pub fn get_decision_log(
    project_dir: Option<&Path>,
) -> std::result::Result<serde_json::Value, ToolError> {
    if let Some(dir) = project_dir {
        require_dir(dir, "project_dir")?;
    }
    let path = crate::decisions::default_path(project_dir);
    let log = crate::decisions::read_with_stats(&path);
    let count = log.entries.len();
    Ok(serde_json::json!({
        "path": path.display().to_string(),
        "count": count,
        "skipped": log.skipped,
        "entries": log.entries,
    }))
}

// ---------------------------------------------------------------------------
// rmcp server (thin transport adapter over the handlers above)
// ---------------------------------------------------------------------------

/// Input for `analyse_electron_app`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AnalyseRequest {
    /// Directory of the Electron app to analyse.
    pub app_dir: String,
}

/// Input for `get_migration_plan`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MigrationPlanRequest {
    /// Directory of the Electron app to plan the migration for.
    pub app_dir: String,
}

/// Input for `get_rust_replacement`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReplacementRequest {
    /// Electron API name, e.g. `"dialog.showOpenDialog"`. Required
    /// unless `list_all` is true.
    #[serde(default)]
    pub api: Option<String>,
    /// When true, dump the whole mappings table instead of one lookup.
    #[serde(default)]
    pub list_all: Option<bool>,
}

/// Input for `simulate_migration`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SimulateRequest {
    /// Directory of the Electron app to simulate.
    pub app_dir: String,
    /// Target platform for the per-platform predictions: `"macos"`,
    /// `"windows"`, or `"linux"`. Defaults to the host platform.
    #[serde(default)]
    pub target_platform: Option<String>,
}

/// Input for `validate_migration`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ValidateRequest {
    /// Directory of the converted Tauri project to validate.
    pub project_dir: String,
    /// Directory of the original Electron app, when available: the
    /// validator re-scans it and lists MANUAL findings as remaining
    /// manual work.
    #[serde(default)]
    pub original_app_dir: Option<String>,
}

/// Input for `get_decision_log`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DecisionLogRequest {
    /// Directory of the converted project whose decision log to read.
    /// Omit to read the shared log (`~/.cache/e2r/decisions.jsonl`).
    #[serde(default)]
    pub project_dir: Option<String>,
}

/// The e2r MCP server: six tools over stdio, served by [`serve_stdio`].
///
/// The `#[tool]` methods are a thin adapter: each one calls the plain
/// handler function of the same name and wraps the outcome with
/// `result_from`. All real logic lives in the handlers, which are
/// testable without any transport.
#[derive(Debug, Clone)]
pub struct E2rServer {
    /// The tool router, built by the `#[tool_router]` macro. Dead-code
    /// analysis cannot see the macro-generated reads, hence the allow.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl E2rServer {
    /// Build a new server.
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

impl Default for E2rServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl E2rServer {
    /// Analyse an Electron app and return the full scan report as JSON:
    /// IPC channels, API findings with Tauri targets, preload info,
    /// feasibility score and verdict.
    #[tool(
        description = "Analyse an Electron app: scan it and return the full scan report (IPC channels, API findings with Tauri targets, preload info, feasibility score, verdict) as JSON."
    )]
    fn analyse_electron_app(&self, Parameters(req): Parameters<AnalyseRequest>) -> CallToolResult {
        result_from(analyse_electron_app(Path::new(&req.app_dir)))
    }

    /// Compose an ordered migration plan for an Electron app: steps
    /// (analyse → simulate → convert → validate), per-API replacements,
    /// MANUAL items called out first, and an effort hint.
    #[tool(
        description = "Compose an ordered migration plan for an Electron app: steps (analyse, simulate, convert, validate), per-API Tauri replacements, MANUAL items called out first, and an effort hint."
    )]
    fn get_migration_plan(
        &self,
        Parameters(req): Parameters<MigrationPlanRequest>,
    ) -> CallToolResult {
        result_from(get_migration_plan(Path::new(&req.app_dir)))
    }

    /// Look one Electron API up in the mappings database (Tauri
    /// equivalent, plugin, classification, notes), or dump the whole
    /// table with `list_all`.
    #[tool(
        description = "Look up one Electron API's Tauri replacement (equivalent, plugin, classification, notes), or dump the whole mappings table with list_all=true."
    )]
    fn get_rust_replacement(
        &self,
        Parameters(req): Parameters<ReplacementRequest>,
    ) -> CallToolResult {
        result_from(get_rust_replacement(
            req.api.as_deref().unwrap_or(""),
            req.list_all.unwrap_or(false),
        ))
    }

    /// Simulate a migration before any code changes: migratable
    /// breakdown, UI preservation estimate, performance predictions
    /// with confidence ranges, warnings, and an effort hint.
    #[tool(
        description = "Simulate an Electron-to-Tauri migration before any code changes: migratable breakdown, UI preservation estimate, performance predictions with confidence ranges, warnings, and an effort hint."
    )]
    fn simulate_migration(&self, Parameters(req): Parameters<SimulateRequest>) -> CallToolResult {
        result_from(simulate_migration(
            Path::new(&req.app_dir),
            req.target_platform.as_deref(),
        ))
    }

    /// Validate a converted Tauri project without running `cargo check`:
    /// expected files, renderer carry-over, TODO(e2r) stubs remaining,
    /// and MANUAL work left (when the original app is given).
    #[tool(
        description = "Validate a converted Tauri project without running cargo check: expected files present/missing, renderer carry-over, TODO(e2r) stubs remaining, and MANUAL work left when the original app directory is given."
    )]
    fn validate_migration(&self, Parameters(req): Parameters<ValidateRequest>) -> CallToolResult {
        result_from(validate_migration(
            Path::new(&req.project_dir),
            req.original_app_dir.as_deref().map(Path::new),
        ))
    }

    /// Read the decision registry: the API decisions a conversion
    /// recorded, oldest first, plus the count.
    #[tool(
        description = "Read the decision registry: the Electron-API decisions a conversion recorded (API, Tauri replacement, classification, rationale, confidence), oldest first, plus the count."
    )]
    fn get_decision_log(&self, Parameters(req): Parameters<DecisionLogRequest>) -> CallToolResult {
        result_from(get_decision_log(req.project_dir.as_deref().map(Path::new)))
    }
}

#[tool_handler]
impl ServerHandler for E2rServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("e2r", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Convert Electron desktop apps to Rust (Tauri 2). Analyse an app, plan its migration, look up Tauri replacements, simulate the payoff, validate a converted project, and read the decision log.",
            )
    }
}

/// Run the MCP server on stdio until the client disconnects (EOF on
/// stdin). This is what `e2r serve` runs.
pub async fn serve_stdio() -> Result<()> {
    let service = E2rServer::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A fixture app, relative to this crate's manifest dir.
    fn fixture_dir(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name)
    }

    /// A fresh scratch dir per test (no `tempfile` dependency: process id
    /// + timestamp is unique enough for a test). Under the crate target
    ///   dir, not `/tmp` (see the golden tests' note on the 512 MB tmpfs).
    fn scratch_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(format!("mcp-{tag}-{}-{nanos}", std::process::id()))
    }

    /// Point the calibration log at a nonexistent path so simulation
    /// results never depend on the developer's log. Tests in this module
    /// all set the same value, so parallel runs cannot interfere.
    fn disable_calibration() {
        std::env::set_var(
            crate::calibrate::CALIBRATION_LOG_ENV,
            scratch_dir("no-calibration").join("calibration.jsonl"),
        );
    }

    #[test]
    fn test_analyse_returns_scan_report() {
        let value = analyse_electron_app(&fixture_dir("minimal-electron-app"))
            .expect("analyse must succeed on the fixture");
        assert_eq!(value["app_name"], "minimal-electron-app");
        assert!(value["feasibility_score"].is_number());
        assert!(value["findings"].is_array());
        assert!(value["ipc_channels"].is_array());
    }

    #[test]
    fn test_analyse_missing_dir_is_error_not_panic() {
        let err = analyse_electron_app(Path::new("/definitely/not/a/real/dir"))
            .expect_err("missing dir must be an error");
        assert_eq!(err.code, "NOT_FOUND");
        assert!(err.to_json()["code"] == "NOT_FOUND");
    }

    #[test]
    fn test_migration_plan_shape() {
        disable_calibration();
        let value = get_migration_plan(&fixture_dir("full-apis-electron-app"))
            .expect("plan must succeed on the fixture");
        assert_eq!(value["app_name"], "full-apis-electron-app");
        let steps = value["steps"].as_array().expect("steps is an array");
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0]["name"], "analyse");
        assert_eq!(steps[1]["name"], "simulate");
        assert_eq!(steps[2]["name"], "convert");
        assert_eq!(steps[3]["name"], "validate");
        assert!(value["manual_first"].is_array());
        assert!(value["replacements"].is_array());
        assert!(!value["replacements"].as_array().unwrap().is_empty());
        assert!(value["effort"].is_string());
        assert!(value["effort_note"].is_string());
        assert!(value["breakdown"]["total"].is_number());
    }

    #[test]
    fn test_migration_plan_deterministic() {
        disable_calibration();
        let dir = fixture_dir("minimal-electron-app");
        let first = get_migration_plan(&dir).expect("first plan");
        let second = get_migration_plan(&dir).expect("second plan");
        assert_eq!(
            first, second,
            "same app must produce the same plan, byte for byte"
        );
    }

    #[test]
    fn test_replacement_known_api() {
        let value =
            get_rust_replacement("dialog.showOpenDialog", false).expect("known API must resolve");
        assert_eq!(value["electron_api"], "dialog.showOpenDialog");
        assert!(value["tauri_target"].is_string());
        assert_eq!(value["status"], "auto");
        assert!(value["notes"].is_string());
    }

    #[test]
    fn test_replacement_unknown_api_is_error_not_panic() {
        let err = get_rust_replacement("not.a.real.api", false)
            .expect_err("unknown API must be an error");
        assert_eq!(err.code, "API_NOT_FOUND");
    }

    #[test]
    fn test_replacement_empty_api_is_error() {
        let err = get_rust_replacement("", false).expect_err("empty API must be an error");
        assert_eq!(err.code, "INVALID_ARGUMENT");
    }

    #[test]
    fn test_replacement_list_all() {
        let value = get_rust_replacement("", true).expect("list_all must succeed");
        assert_eq!(value["count"], 48);
        assert_eq!(value["mappings"].as_array().unwrap().len(), 48);
    }

    #[test]
    fn test_simulate_migration_shape() {
        disable_calibration();
        let value = simulate_migration(&fixture_dir("minimal-electron-app"), Some("macos"))
            .expect("simulate must succeed on the fixture");
        assert_eq!(value["app_name"], "minimal-electron-app");
        assert!(value["breakdown"].is_object());
        assert!(value["ui_preservation"].is_object());
        assert!(value["predictions"].is_array());
        assert!(value["effort"].is_string());
        assert_eq!(value["model_version"], "0.1.0");
    }

    #[test]
    fn test_simulate_bad_platform_is_error() {
        let err = simulate_migration(&fixture_dir("minimal-electron-app"), Some("amiga"))
            .expect_err("unknown platform must be an error");
        assert_eq!(err.code, "INVALID_ARGUMENT");
    }

    #[test]
    fn test_validate_converted_fixture() {
        let out = scratch_dir("validate-ok");
        crate::convert::convert(&fixture_dir("minimal-electron-app"), &out)
            .expect("convert must succeed on the fixture");
        let value = validate_migration(&out, Some(&fixture_dir("minimal-electron-app")))
            .expect("validate must succeed on a converted fixture");
        assert_eq!(value["is_e2r_project"], true);
        assert!(value["missing"].as_array().unwrap().is_empty());
        assert!(!value["carried_over"].as_array().unwrap().is_empty());
        assert_eq!(value["renderer_carried_over"], true);
        assert!(value["todo_stubs"].is_number());
        assert!(value["manual_remaining"].is_array());
        let next_steps = value["next_steps"].as_array().unwrap();
        assert!(
            next_steps
                .iter()
                .any(|s| s.as_str().unwrap_or("").contains("cargo check")),
            "next_steps must mention cargo check"
        );
        fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn test_validate_not_a_project_is_error() {
        let dir = scratch_dir("validate-not-a-project");
        fs::create_dir_all(&dir).unwrap();
        let err = validate_migration(&dir, None).expect_err("plain dir must be an error");
        assert_eq!(err.code, "NOT_E2R_PROJECT");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_validate_tauri_but_not_e2r_is_error() {
        let dir = scratch_dir("validate-tauri-not-e2r");
        fs::create_dir_all(dir.join("src-tauri")).unwrap();
        fs::write(dir.join("src-tauri/tauri.conf.json"), "{}").unwrap();
        let err = validate_migration(&dir, None)
            .expect_err("Tauri project without an e2r marker must be an error");
        assert_eq!(err.code, "NOT_E2R_PROJECT");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_validate_counts_todo_stubs() {
        let out = scratch_dir("validate-stubs");
        crate::convert::convert(&fixture_dir("full-apis-electron-app"), &out)
            .expect("convert must succeed on the fixture");
        let value = validate_migration(&out, None).expect("validate must succeed");
        // The full-APIs fixture has ipcMain.handle channels, so the
        // generated commands.rs carries TODO(e2r) stubs.
        assert!(
            value["todo_stubs"].as_u64().unwrap() > 0,
            "expected TODO(e2r) stubs in the converted full-APIs fixture"
        );
        assert!(!value["todo_stub_files"].as_array().unwrap().is_empty());
        fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn test_decision_log_round_trip_through_convert() {
        let out = scratch_dir("decisions-convert");
        crate::convert::convert(&fixture_dir("minimal-electron-app"), &out)
            .expect("convert must succeed on the fixture");
        let value = get_decision_log(Some(&out)).expect("decision log must read back");
        assert!(value["count"].as_u64().unwrap() > 0);
        assert_eq!(value["skipped"], 0);
        let entries = value["entries"].as_array().unwrap();
        assert_eq!(entries.len() as u64, value["count"].as_u64().unwrap());
        assert_eq!(entries[0]["version"], 1);
        assert!(entries[0]["electron_api"].is_string());
        assert!(entries[0]["timestamp"].as_str().unwrap().ends_with('Z'));
        fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn test_decision_log_missing_project_dir_is_error() {
        let err = get_decision_log(Some(Path::new("/definitely/not/a/real/dir")))
            .expect_err("missing project dir must be an error");
        assert_eq!(err.code, "NOT_FOUND");
    }
}
