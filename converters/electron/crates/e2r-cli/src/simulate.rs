//! Pre-migration simulator: `e2r simulate`.
//!
//! Predicts the shape and payoff of migrating an Electron app to Tauri 2
//! *before* any code changes: the migratable breakdown, the UI
//! preservation estimate, performance predictions with confidence
//! ranges, warnings, and a rough effort hint.
//!
//! Inputs — the simulator never re-scans or re-classifies:
//!
//! * the scan report ([`crate::scan::scan`]) — the single source of
//!   truth for APIs, classifications, windows, native modules, and the
//!   feasibility score;
//! * the converter's renderer detection
//!   ([`crate::convert::detect_renderer`]) — the single source of truth
//!   for how the renderer carries over (built output vs. bundler source
//!   vs. static files);
//! * the model constants ([`crate::reference`]) — every number the
//!   prediction model uses, each with its citation and confidence;
//! * the target platform ([`TargetPlatform`]) — memory and process
//!   count are platform-dependent (macOS ≠ Windows ≠ Linux), so the
//!   memory and process-count predictions carry their platform in
//!   their metric id (e.g. `memory_macos`);
//! * the calibration log ([`crate::calibrate`]) — past
//!   predicted-vs-actual comparisons adjust the confidence intervals
//!   (widen when actuals fall outside, narrow when they stay inside).
//!
//! Honesty rules (see `docs/IMPLEMENTATION.md` §5.3):
//!
//! * Predictions are **ranges with confidence levels**, never single
//!   numbers — and never the planning chat's illustrative sample figures
//!   (86/100 readiness; 82%/18%/94%; startup 2.1s→1.7s; memory 380→265
//!   MB; CPU 22%→9%; binary 120→28 MB). Those exist in the repo only
//!   inside explicitly-labeled "NOT measurements" boxes, and this module
//!   must never emit them as measurements or model outputs.
//! * Every predicted number carries a `basis` string naming where it
//!   came from and an `assumptions` list naming what it assumes.
//! * Where the model has no basis, it emits `unknown` with **no number**
//!   (see the CPU prediction and the Linux memory prediction) rather
//!   than inventing one.
//! * The model **never predicts a startup improvement** — the published
//!   measurements disagree on which framework starts faster, so the
//!   startup prediction is "no meaningful change" (point exactly 1.0).
//! * Bundle size is modeled from **measured e2r outputs** (a 0-plugin
//!   floor plus ~2 MB per plugin), not from the third-party "3–15 MB"
//!   range — which the model never uses as an input.
//!
//! The report is deterministic: same app → byte-identical JSON. All
//! collections are sorted, there are no timestamps inside predictions,
//! and no `HashMap` iteration order leaks into the output. (The
//! predictions record — [`crate::model::PredictionsRecord`] — is the one
//! exception: it carries a `generated_at` timestamp by design, and it is
//! a separate file from the report.)

use crate::convert::{RendererKind, RendererPlan};
use crate::model::{
    Breakdown, Confidence, Effort, Finding, Prediction, PredictionsRecord, Range, RecordApp,
    RecordPrediction, RecordScanSummary, ScanReport, SimulationReport, UiFactor, UiPreservation,
    UiTouchPoint,
};
use crate::reference;
use crate::scan;
use anyhow::Result;
use regex::Regex;
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

/// Directories never descended into while measuring the app or scanning
/// renderer sources.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    "out",
    ".git",
    ".hg",
    ".svn",
];

/// Directories that may hold renderer source (same set the converter
/// checks, in the same order).
const RENDERER_SOURCE_DIRS: &[&str] = &["src", "renderer", "app", "web"];

/// Root-level asset directories that may hold renderer files in a static
/// app (same set the converter copies for a static renderer).
const STATIC_ASSET_DIRS: &[&str] = &[
    "assets", "static", "public", "css", "js", "img", "images", "fonts", "media", "styles",
    "scripts", "vendor",
];

/// File extensions scanned for renderer touch points.
const RENDERER_EXTS: &[&str] = &["js", "ts", "jsx", "tsx", "mjs", "cjs", "html"];

/// File extensions scanned for DRM markers.
const DRM_SCAN_EXTS: &[&str] = &["js", "ts", "jsx", "tsx", "mjs", "cjs", "html"];

/// Next.js config filenames, searched for `output: 'export'`.
const NEXT_CONFIG_FILES: &[&str] = &[
    "next.config.js",
    "next.config.mjs",
    "next.config.cjs",
    "next.config.ts",
];

/// Node.js built-in module names. A `require` of one of these inside a
/// renderer file is an Electron-only touch point (it only works because
/// Electron runs Node in the renderer).
const NODE_BUILTINS: &[&str] = &[
    "fs",
    "path",
    "os",
    "child_process",
    "net",
    "http",
    "https",
    "crypto",
    "util",
    "events",
    "stream",
    "url",
    "querystring",
    "readline",
    "zlib",
    "dgram",
    "dns",
    "tls",
    "cluster",
    "worker_threads",
    "perf_hooks",
    "assert",
    "buffer",
    "process",
];

/// Bytes in one megabyte, as this module counts them (decimal MB).
const BYTES_PER_MB: f64 = 1_000_000.0;

/// The scope note attached to every UI preservation estimate: full-native
/// UI mode is out of scope for the estimate.
const UI_SCOPE_NOTE: &str = "UI-preserving (system webview) mode only: the renderer is carried \
    over verbatim and the estimate counts only Electron-API touch points that need a bridge call \
    or a port. A full-native UI rewrite (egui / Iced / Dioxus / Slint) is out of scope for this \
    estimate — see docs/RND.md §8.";

/// Output format for `e2r simulate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    /// Plain-text summary for the terminal.
    Human,
    /// The [`SimulationReport`] as pretty JSON.
    Json,
    /// Condensed Markdown (headings + tables) for issues and PRs.
    Markdown,
}

/// Target platform for the per-platform predictions (memory and process
/// count). Memory is platform-dependent — macOS sees a real idle-memory
/// win, Windows sees about none, Linux is unmeasured — and the
/// process count goes UP on Windows, so the model never blends the
/// platforms into one number. The platform is part of the prediction's
/// metric id (`memory_macos`, `process_count_windows`, ...), so it is
/// explicit in every output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPlatform {
    /// macOS (WKWebView).
    Macos,
    /// Windows (WebView2 / Chromium).
    Windows,
    /// Linux (WebKitGTK).
    Linux,
}

impl TargetPlatform {
    /// The platform id used in metric names: `"macos"`, `"windows"`,
    /// or `"linux"`.
    pub fn id(self) -> &'static str {
        match self {
            TargetPlatform::Macos => "macos",
            TargetPlatform::Windows => "windows",
            TargetPlatform::Linux => "linux",
        }
    }

    /// Display name: `"macOS"`, `"Windows"`, or `"Linux"`.
    pub fn display_name(self) -> &'static str {
        match self {
            TargetPlatform::Macos => "macOS",
            TargetPlatform::Windows => "Windows",
            TargetPlatform::Linux => "Linux",
        }
    }

    /// The platform this binary is running on, from
    /// `std::env::consts::OS`. Anything that is not macOS or Windows
    /// is treated as Linux (the simulator only knows these three).
    pub fn host() -> Self {
        match std::env::consts::OS {
            "macos" => TargetPlatform::Macos,
            "windows" => TargetPlatform::Windows,
            _ => TargetPlatform::Linux,
        }
    }

    /// Parse a `--target-platform` value (`macos`, `windows`, `linux`,
    /// case-insensitive); `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "macos" | "mac" | "darwin" => Some(TargetPlatform::Macos),
            "windows" | "win32" | "win" => Some(TargetPlatform::Windows),
            "linux" => Some(TargetPlatform::Linux),
            _ => None,
        }
    }
}

/// Where `simulate` reads calibration history from (see
/// [`crate::calibrate`]).
#[derive(Debug, Clone)]
pub enum CalibrationInput {
    /// The default log (`~/.cache/e2r/calibration.jsonl`, or
    /// `E2R_CALIBRATION_LOG` when set).
    Default,
    /// A specific log file.
    Path(PathBuf),
    /// No calibration — the model's unadjusted intervals. Tests use
    /// this so results never depend on the developer's log.
    Disabled,
}

/// Options for [`simulate_with_options`].
#[derive(Debug, Clone)]
pub struct SimulateOptions {
    /// Target platform for the per-platform predictions.
    pub target_platform: TargetPlatform,
    /// Where calibration history comes from.
    pub calibration: CalibrationInput,
}

impl Default for SimulateOptions {
    fn default() -> Self {
        Self {
            target_platform: TargetPlatform::host(),
            calibration: CalibrationInput::Default,
        }
    }
}

/// Simulate the migration of the Electron app at `app_dir`.
///
/// Runs the scan (the single source of truth), reuses the converter's
/// renderer detection, and builds the full simulation report. Read-only:
/// nothing is written to the app directory.
///
/// Uses the default options: the host platform as the target platform,
/// and calibration from the default log (a missing log behaves exactly
/// like no calibration). For explicit control use
/// [`simulate_with_options`].
pub fn simulate(app_dir: &Path) -> Result<SimulationReport> {
    simulate_with_options(app_dir, &SimulateOptions::default())
}

/// Simulate with explicit options (target platform, calibration
/// source). See [`simulate`] for what a simulation does.
pub fn simulate_with_options(
    app_dir: &Path,
    options: &SimulateOptions,
) -> Result<SimulationReport> {
    let scan_report = scan::scan(app_dir)?;
    let pkg = read_package_json(app_dir);
    let renderer = crate::convert::detect_renderer(app_dir, &pkg, &scan_report);
    let breakdown = breakdown(&scan_report.findings);
    let ui = ui_preservation(app_dir, &pkg, &renderer, &scan_report);
    let payload_mb = measure_app_payload_mb(app_dir);
    let runtime_mb = measure_installed_electron_mb(app_dir);
    let (plugin_count, plugin_names) = count_plugins(&scan_report);
    let adjustments = match &options.calibration {
        CalibrationInput::Disabled => BTreeMap::new(),
        CalibrationInput::Default => {
            let path = crate::calibrate::calibration_log_path(None);
            crate::calibrate::interval_adjustments(&crate::calibrate::read_log(&path))
        }
        CalibrationInput::Path(path) => {
            crate::calibrate::interval_adjustments(&crate::calibrate::read_log(path))
        }
    };
    let predictions = predict(
        &scan_report,
        payload_mb,
        runtime_mb,
        plugin_count,
        &plugin_names,
        options.target_platform,
        &adjustments,
    );
    let warnings = collect_warnings(&scan_report, &renderer, app_dir);
    let (effort, effort_note) = effort_hint(&breakdown, scan_report.native_modules.len());
    Ok(SimulationReport {
        app_name: scan_report.app_name,
        app_version: scan_report.app_version,
        feasibility_score: scan_report.feasibility_score,
        verdict: scan_report.verdict,
        breakdown,
        ui_preservation: ui,
        predictions,
        warnings,
        effort,
        effort_note,
        model_version: reference::MODEL_VERSION.to_string(),
    })
}

/// Render a [`SimulationReport`] in the requested format.
pub fn format_report(report: &SimulationReport, format: ReportFormat) -> String {
    match format {
        ReportFormat::Human => format_human_report(report),
        ReportFormat::Json => {
            serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".to_string())
        }
        ReportFormat::Markdown => format_markdown_report(report),
    }
}

/// Build the predictions record for the calibration loop
/// (see [`PredictionsRecord`] for the stable format).
pub fn build_record(report: &SimulationReport, app_dir: &Path) -> PredictionsRecord {
    PredictionsRecord {
        schema_version: 1,
        model_version: report.model_version.clone(),
        app: RecordApp {
            name: report.app_name.clone(),
            version: report.app_version.clone(),
            path: app_dir.display().to_string(),
        },
        generated_at: rfc3339_now(),
        scan_summary: RecordScanSummary {
            feasibility_score: report.feasibility_score,
            auto: report.breakdown.auto,
            assisted: report.breakdown.assisted,
            manual: report.breakdown.manual,
        },
        ui_preservation: report.ui_preservation.clone(),
        predictions: report
            .predictions
            .iter()
            .map(|p| RecordPrediction {
                metric: p.metric.clone(),
                low: p.after.map(|r| r.low),
                point: p.after.map(|r| r.point),
                high: p.after.map(|r| r.high),
                unit: p.unit.clone(),
                confidence: p.confidence,
                basis: p.basis.clone(),
            })
            .collect(),
    }
}

// --- Breakdown -------------------------------------------------------------

/// Count findings by classification, weighted by [`Finding::occurrences`],
/// with percentages that sum to exactly 100 (largest-remainder rounding
/// to one decimal). With no call sites at all, all shares are 0.
fn breakdown(findings: &[Finding]) -> Breakdown {
    let (mut auto, mut assisted, mut manual) = (0usize, 0usize, 0usize);
    for finding in findings {
        match finding.classification {
            crate::model::Classification::Auto => auto += finding.occurrences,
            crate::model::Classification::Assisted => assisted += finding.occurrences,
            crate::model::Classification::Manual => manual += finding.occurrences,
        }
    }
    let total = auto + assisted + manual;
    let (auto_pct, assisted_pct, manual_pct) = if total == 0 {
        (0.0, 0.0, 0.0)
    } else {
        percentages([auto, assisted, manual], total)
    };
    Breakdown {
        auto,
        assisted,
        manual,
        total,
        auto_pct,
        assisted_pct,
        manual_pct,
    }
}

/// Largest-remainder percentages to one decimal: each share is floored to
/// a tenth of a percent, then the remaining tenths go to the largest
/// fractional remainders (ties broken by order: auto, assisted, manual),
/// so the three percentages always sum to exactly 100.0.
fn percentages(counts: [usize; 3], total: usize) -> (f64, f64, f64) {
    let exact: [f64; 3] = counts.map(|c| c as f64 * 100.0 / total as f64);
    // Work in integer tenths of a percent to keep the sum exact.
    let mut tenths: [i64; 3] = exact.map(|e| (e * 10.0).floor() as i64);
    let remainders: [f64; 3] = exact.map(|e| e * 10.0 - (e * 10.0).floor());
    let mut left = 1000 - tenths.iter().sum::<i64>();
    let mut order = [0usize, 1, 2];
    // Largest remainder first; ties keep the original order (stable sort).
    order.sort_by(|&a, &b| {
        remainders[b]
            .partial_cmp(&remainders[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut i = 0;
    while left > 0 {
        tenths[order[i % 3]] += 1;
        left -= 1;
        i += 1;
    }
    (
        tenths[0] as f64 / 10.0,
        tenths[1] as f64 / 10.0,
        tenths[2] as f64 / 10.0,
    )
}

// --- UI preservation -------------------------------------------------------

/// Read `package.json` (if present and parsable) for framework/bundler
/// detection. A missing or broken file is not an error — detection just
/// has less to work with.
fn read_package_json(app_dir: &Path) -> Option<JsonValue> {
    let text = fs::read_to_string(app_dir.join("package.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Dependency names from `dependencies` + `devDependencies`.
fn dep_names(pkg: &Option<JsonValue>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(pkg) = pkg {
        for section in ["dependencies", "devDependencies"] {
            if let Some(deps) = pkg.get(section).and_then(|d| d.as_object()) {
                names.extend(deps.keys().cloned());
            }
        }
    }
    names
}

/// Script bodies from `package.json` `scripts`, for bundler detection.
fn script_texts(pkg: &Option<JsonValue>) -> Vec<String> {
    let mut scripts = Vec::new();
    if let Some(pkg) = pkg {
        if let Some(obj) = pkg.get("scripts").and_then(|s| s.as_object()) {
            for value in obj.values() {
                if let Some(text) = value.as_str() {
                    scripts.push(text.to_string());
                }
            }
        }
    }
    scripts.sort();
    scripts
}

/// Detect the renderer framework from `package.json` deps: `react`,
/// `vue`, `svelte`, `angular`, `vanilla` (renderer files exist but no
/// framework dependency), or `unknown` (no renderer at all).
fn detect_framework(pkg: &Option<JsonValue>, has_renderer: bool) -> &'static str {
    if !has_renderer {
        return "unknown";
    }
    let deps = dep_names(pkg);
    if deps.contains("react") {
        "react"
    } else if deps.contains("vue") {
        "vue"
    } else if deps.contains("svelte") {
        "svelte"
    } else if deps.contains("@angular/core") {
        "angular"
    } else {
        "vanilla"
    }
}

/// Detect the bundler from `package.json` deps and scripts: `vite`,
/// `webpack`, `electron-vite`, `next`, `other` (parcel, react-scripts,
/// esbuild, rollup), or `none`.
fn detect_bundler(pkg: &Option<JsonValue>) -> &'static str {
    let deps = dep_names(pkg);
    let scripts = script_texts(pkg);
    let in_scripts = |needle: &str| scripts.iter().any(|s| s.contains(needle));
    // electron-vite first: its scripts also contain "vite".
    if deps.contains("electron-vite") || in_scripts("electron-vite") {
        "electron-vite"
    } else if deps.contains("vite") || in_scripts("vite") {
        "vite"
    } else if deps.contains("webpack") || in_scripts("webpack") {
        "webpack"
    } else if deps.contains("next") || in_scripts("next") {
        "next"
    } else if deps.contains("parcel")
        || deps.contains("react-scripts")
        || deps.contains("esbuild")
        || deps.contains("rollup")
        || in_scripts("parcel")
        || in_scripts("react-scripts")
        || in_scripts("esbuild")
        || in_scripts("rollup")
    {
        "other"
    } else {
        "none"
    }
}

/// Whether the Next.js config enables static export (`output: 'export'`).
/// Tauri serves static files, so a Next app needs the export to run.
fn next_has_static_export(app_dir: &Path) -> bool {
    for name in NEXT_CONFIG_FILES {
        if let Ok(text) = fs::read_to_string(app_dir.join(name)) {
            if text.contains("output") && (text.contains("'export'") || text.contains("\"export\""))
            {
                return true;
            }
        }
    }
    false
}

/// Path relative to the app dir, with `/` separators, for reports.
fn relative_path(app_dir: &Path, path: &Path) -> String {
    path.strip_prefix(app_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Strip a leading `./` from a relative path for comparison.
fn normalize_rel(path: &str) -> &str {
    path.strip_prefix("./").unwrap_or(path)
}

/// Renderer source files to scan for Electron-API touch points:
/// the renderer source dirs (when the converter carried source), the
/// root `index.html` / root scripts (Vite root layout and static apps),
/// and the static asset dirs (static apps). The main-process entry and
/// preload scripts are excluded — they are not UI, and preload is
/// replaced by the bridge rather than carried over. Built output is
/// never scanned: it is generated code, and its line numbers would be
/// meaningless.
fn renderer_source_files(app_dir: &Path, plan: &RendererPlan, report: &ScanReport) -> Vec<PathBuf> {
    if matches!(plan.kind, RendererKind::Placeholder) {
        return Vec::new();
    }
    if matches!(plan.kind, RendererKind::BuiltOutput) && !plan.source_carried {
        // Built output only, no source: nothing to scan for touch points.
        return Vec::new();
    }
    let main_entry = report.main_entry.as_deref().map(normalize_rel);
    let preloads: BTreeSet<&str> = report
        .preload
        .files
        .iter()
        .map(|p| normalize_rel(p))
        .collect();
    let excluded = |rel: &str| -> bool { main_entry == Some(rel) || preloads.contains(rel) };
    let is_renderer_file = |path: &Path| -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| RENDERER_EXTS.contains(&e))
            .unwrap_or(false)
    };
    let mut files: BTreeSet<PathBuf> = BTreeSet::new();

    // Renderer source dirs, when the converter carried source.
    if plan.source_carried {
        for dir in RENDERER_SOURCE_DIRS {
            let base = app_dir.join(dir);
            if !base.is_dir() {
                continue;
            }
            for entry in WalkDir::new(&base)
                .into_iter()
                .filter_entry(|e| {
                    !(e.file_type().is_dir()
                        && e.file_name()
                            .to_str()
                            .map(|n| SKIP_DIRS.contains(&n))
                            .unwrap_or(false))
                })
                .flatten()
            {
                if !entry.file_type().is_file() || !is_renderer_file(entry.path()) {
                    continue;
                }
                let rel = relative_path(app_dir, entry.path());
                if excluded(&rel) {
                    continue;
                }
                files.insert(entry.path().to_path_buf());
            }
        }
    }

    // Root renderer files: `index.html` and any root script that is not
    // the main entry or a preload script.
    if let Ok(entries) = fs::read_dir(app_dir) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            if !is_renderer_file(&path) {
                continue;
            }
            let rel = relative_path(app_dir, &path);
            if excluded(&rel) {
                continue;
            }
            files.insert(path);
        }
    }

    // Static asset dirs (static apps only).
    if matches!(plan.kind, RendererKind::Static) {
        for dir in STATIC_ASSET_DIRS {
            let base = app_dir.join(dir);
            if !base.is_dir() {
                continue;
            }
            for entry in WalkDir::new(&base).into_iter().flatten() {
                if !entry.file_type().is_file() || !is_renderer_file(entry.path()) {
                    continue;
                }
                let rel = relative_path(app_dir, entry.path());
                if excluded(&rel) {
                    continue;
                }
                files.insert(entry.path().to_path_buf());
            }
        }
    }

    files.into_iter().collect()
}

/// One regex-detectable Electron-only pattern in renderer sources.
struct TouchPattern {
    /// Regex matched against a single source line.
    regex: Regex,
    /// Label for the touch point, e.g. `"window.require('electron')"`.
    /// `{name}` is replaced with the first capture group when present.
    label: &'static str,
    /// One line on what the call site needs instead.
    detail: &'static str,
}

/// The touch-point patterns. `window.require('electron')` is checked
/// before bare `require('electron')` — a line matching the first must
/// not also produce the second (the bare pattern is a substring of it).
fn touch_patterns() -> Vec<TouchPattern> {
    let builtins = NODE_BUILTINS.join("|");
    let require_builtin = format!(r#"require\s*\(\s*['"](?:node:)?({builtins})\b['"]"#);
    let import_builtin = format!(r#"from\s+['"](?:node:)?({builtins})\b['"]"#);
    let patterns: Vec<(&str, &str, &str)> = vec![
        (
            r#"window\.require\s*\(\s*['"]electron['"]"#,
            "window.require('electron')",
            "loads the Electron module in the renderer — replace with a bridge call",
        ),
        (
            r#"require\s*\(\s*['"]electron['"]"#,
            "require('electron')",
            "loads the Electron module in the renderer — replace with a bridge call",
        ),
        (
            r#"from\s+['"]electron['"]"#,
            "import from 'electron'",
            "imports the Electron module in the renderer — replace with a bridge call",
        ),
        (
            r"\bipcRenderer\b",
            "ipcRenderer direct use",
            "direct ipcRenderer use in the renderer — rebind to the Tauri invoke bridge",
        ),
        (
            r"\bremote\.",
            "remote",
            "the remote module has no Tauri equivalent — port the call to a command",
        ),
        (
            &require_builtin,
            "Node builtin '{name}' in renderer",
            "a Node builtin in renderer code — only works with nodeIntegration; port to a command",
        ),
        (
            &import_builtin,
            "Node builtin '{name}' in renderer",
            "a Node builtin in renderer code — only works with nodeIntegration; port to a command",
        ),
        (
            r"\bprocess\.versions\b",
            "process.versions in renderer",
            "reads the Node/Electron version from the renderer — expose it via the bridge instead",
        ),
        (
            r"\b(__dirname|__filename)\b",
            "'{name}' in renderer",
            "a Node path global in renderer code — only works with nodeIntegration",
        ),
    ];
    patterns
        .into_iter()
        .map(|(pattern, label, detail)| TouchPattern {
            regex: Regex::new(pattern).expect("touch-point regex"),
            label,
            detail,
        })
        .collect()
}

/// Whether a source line is a comment line (JS `//`, block-comment
/// continuation, or HTML `<!--`) — skipped so a sentence in a comment
/// cannot produce a touch point.
fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
        || trimmed.starts_with('*')
        || trimmed.starts_with("<!--")
        || trimmed.starts_with("-->")
}

/// Scan renderer sources for Electron-only call sites. Each match is one
/// touch point with its file and line; results are sorted by
/// (file, line, api) and deduplicated.
fn scan_touch_points(app_dir: &Path, files: &[PathBuf]) -> Vec<UiTouchPoint> {
    let patterns = touch_patterns();
    let mut found: BTreeSet<(String, usize, String, String)> = BTreeSet::new();
    for path in files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = relative_path(app_dir, path);
        for (idx, line) in content.lines().enumerate() {
            if is_comment_line(line) {
                continue;
            }
            let window_require = line.contains("window.require");
            for pattern in &patterns {
                // A `window.require('electron')` line also matches the
                // bare `require('electron')` pattern — count it once.
                if window_require && pattern.label == "require('electron')" {
                    continue;
                }
                if let Some(cap) = pattern.regex.captures(line) {
                    let api = match cap.get(1) {
                        Some(name) => pattern.label.replace("{name}", name.as_str()),
                        None => pattern.label.to_string(),
                    };
                    found.insert((rel.clone(), idx + 1, api, pattern.detail.to_string()));
                }
            }
        }
    }
    found
        .into_iter()
        .map(|(file, line, api, detail)| UiTouchPoint {
            file,
            line,
            api,
            detail,
        })
        .collect()
}

/// Build the UI preservation estimate from the converter's renderer plan,
/// the detected framework/bundler, and the touch-point scan.
///
/// The estimate is multiplicative over the factors: each factor is a
/// carry-over percentage, and `estimated_pct` is their product (scaled
/// back to percent). A renderer with no touch points, a known framework,
/// and a known bundler estimates 100% — UI-preserving mode carries the
/// renderer verbatim by construction, and the estimate only discounts
/// for what provably needs changing.
fn ui_preservation(
    app_dir: &Path,
    pkg: &Option<JsonValue>,
    plan: &RendererPlan,
    report: &ScanReport,
) -> UiPreservation {
    let has_renderer = !matches!(plan.kind, RendererKind::Placeholder);
    let framework = detect_framework(pkg, has_renderer);
    let bundler = if has_renderer {
        detect_bundler(pkg)
    } else {
        "none"
    };
    let renderer_kind = match plan.kind {
        RendererKind::BuiltOutput => "built-output",
        RendererKind::Source => "source",
        RendererKind::Static => "static",
        RendererKind::Placeholder => "none",
    };

    let files = renderer_source_files(app_dir, plan, report);
    let touch_points = scan_touch_points(app_dir, &files);

    // Renderer factor: what the converter found to carry over.
    let (renderer_pct, renderer_detail) = match plan.kind {
        RendererKind::BuiltOutput => (
            100.0,
            format!(
                "Built output in `{}/` is the runnable renderer — carried over as-is",
                plan.frontend_dir
            ),
        ),
        RendererKind::Source => (
            100.0,
            "Renderer source rebuilds with the preserved build script".to_string(),
        ),
        RendererKind::Static => (
            100.0,
            "Static files are copied into dist/ unchanged".to_string(),
        ),
        RendererKind::Placeholder => (
            0.0,
            "No renderer files were found — there is nothing to preserve".to_string(),
        ),
    };

    // Framework factor: known frameworks run unchanged in the webview.
    let (framework_pct, framework_detail) = if framework == "unknown" {
        (
            reference::UI_UNKNOWN_FRAMEWORK_FACTOR_PCT.value,
            "No renderer found — the framework cannot be identified".to_string(),
        )
    } else if framework == "vanilla" {
        (
            100.0,
            "No framework dependency — plain web code runs unchanged in the webview".to_string(),
        )
    } else {
        (
            100.0,
            format!("{framework} code runs unchanged in the system webview"),
        )
    };

    // Bundler factor: known bundlers rebuild as-is; electron-vite and
    // non-export Next.js need build changes first.
    let (bundler_pct, bundler_detail) = match bundler {
        "electron-vite" => (
            reference::UI_ELECTRON_VITE_FACTOR_PCT.value,
            "electron-vite bundles for Electron — the build script needs adapting to a plain vite build".to_string(),
        ),
        "next" => {
            if next_has_static_export(app_dir) {
                (
                    reference::UI_NEXT_EXPORT_FACTOR_PCT.value,
                    "Next.js with static export — the export step is a change from the current setup".to_string(),
                )
            } else {
                (
                    reference::UI_NEXT_NO_EXPORT_FACTOR_PCT.value,
                    "Next.js without static export — Tauri serves static files, so server-side rendering must be replaced".to_string(),
                )
            }
        }
        "other" => (
            reference::UI_OTHER_BUNDLER_FACTOR_PCT.value,
            "Unrecognized bundler — the build probably works in Tauri, but it is unverified".to_string(),
        ),
        "none" if !has_renderer => (100.0, "No renderer found".to_string()),
        "none" => (
            100.0,
            "No bundler — nothing to adapt".to_string(),
        ),
        _ => (
            100.0,
            format!("{bundler} rebuilds as-is inside the Tauri project"),
        ),
    };

    // Touch-point factor: each Electron-API call site in renderer sources
    // is a place the UI does not carry over unchanged.
    let deduction = (touch_points.len() as f64 * reference::UI_TOUCH_POINT_DEDUCTION_PCT.value)
        .min(reference::UI_TOUCH_POINT_MAX_DEDUCTION_PCT.value);
    let touch_pct = 100.0 - deduction;
    let touch_detail = if touch_points.is_empty() {
        "No Electron APIs in renderer sources — nothing to rewire".to_string()
    } else {
        let noun = if touch_points.len() == 1 {
            "touch point"
        } else {
            "touch points"
        };
        format!(
            "{} Electron-API {noun} in renderer sources need a bridge call or a port",
            touch_points.len()
        )
    };

    let factors = vec![
        UiFactor {
            name: format!("Renderer: {renderer_kind}"),
            carry_over_pct: renderer_pct,
            detail: renderer_detail,
        },
        UiFactor {
            name: format!("Framework: {framework}"),
            carry_over_pct: framework_pct,
            detail: framework_detail,
        },
        UiFactor {
            name: format!("Bundler: {bundler}"),
            carry_over_pct: bundler_pct,
            detail: bundler_detail,
        },
        UiFactor {
            name: "Electron API touch points".to_string(),
            carry_over_pct: touch_pct,
            detail: touch_detail,
        },
    ];
    let estimated_pct = round1(
        100.0
            * factors
                .iter()
                .map(|f| f.carry_over_pct / 100.0)
                .product::<f64>(),
    );

    UiPreservation {
        estimated_pct,
        mode: "ui-preserving (system webview)".to_string(),
        framework: framework.to_string(),
        bundler: bundler.to_string(),
        renderer_kind: renderer_kind.to_string(),
        factors,
        touch_points,
        note: UI_SCOPE_NOTE.to_string(),
    }
}

// --- Predictions -----------------------------------------------------------

/// Round to 3 decimal places (the precision predictions are reported at).
fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Round to 1 decimal place (the precision percentages are reported at).
fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Build a [`Range`] from unrounded bounds, rounding each bound to 3
/// decimals. Panics (via [`Range::new`]) if rounding ever collapses the
/// ordering — a model bug, caught by the interval-sanity tests.
fn range3(low: f64, point: f64, high: f64) -> Range {
    Range::new(round3(low), round3(point), round3(high))
}

/// Total size in bytes of `dir`, skipping directories in `skip` by name.
fn dir_size_bytes(dir: &Path, skip: &[&str]) -> u64 {
    let mut total = 0u64;
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            !(e.depth() > 0
                && e.file_type().is_dir()
                && e.file_name()
                    .to_str()
                    .map(|n| skip.contains(&n))
                    .unwrap_or(false))
        })
        .flatten()
    {
        if entry.file_type().is_file() {
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

/// The app's own payload in MB: every file under the app dir except
/// `node_modules` and version control. Built output counts — it ships in
/// both bundles.
fn measure_app_payload_mb(app_dir: &Path) -> f64 {
    round3(dir_size_bytes(app_dir, &["node_modules", ".git", ".hg", ".svn"]) as f64 / BYTES_PER_MB)
}

/// The installed Electron runtime in MB, measured from
/// `node_modules/electron/dist` when it exists and is big enough to be a
/// real install. `None` when the app has no installed runtime (the
/// common case for a source checkout) — the model then falls back to the
/// cited bundle range.
fn measure_installed_electron_mb(app_dir: &Path) -> Option<f64> {
    let dist = app_dir.join("node_modules").join("electron").join("dist");
    if !dist.is_dir() {
        return None;
    }
    let mb = round3(dir_size_bytes(&dist, &[]) as f64 / BYTES_PER_MB);
    if mb < reference::MEASURED_RUNTIME_MIN_MB.value {
        return None;
    }
    Some(mb)
}

/// Which Tauri plugins the generated project will include, decided
/// from the scan findings and the dependency lists — the count the
/// bundle-size model scales with (~2 MB per plugin).
///
/// This mirrors the converter's `detect_plugins` (same API list, same
/// rules, including the deliberate Tray exclusion: the mapping table
/// names the Tauri v1 `tauri-plugin-system-tray`, which does not exist
/// for Tauri 2). It is duplicated here rather than shared because
/// `detect_plugins` is private to `convert.rs`; if the converter's
/// plugin set ever changes, this function must change with it — the
/// fixture tests pin the counts (minimal 2, full-apis 7, built-output
/// 0), which are also the counts behind the measured binaries in
/// `docs/RND.md` §11.
///
/// Returns the count and the sorted plugin crate names (for the
/// prediction's basis string).
fn count_plugins(report: &ScanReport) -> (usize, Vec<&'static str>) {
    let mut plugins: BTreeSet<&'static str> = BTreeSet::new();
    for f in &report.findings {
        let plugin = match f.api.as_str() {
            "shell.openExternal" | "shell.openPath" | "shell" => Some("tauri-plugin-opener"),
            "dialog.showOpenDialog"
            | "dialog.showSaveDialog"
            | "dialog.showMessageBox"
            | "dialog" => Some("tauri-plugin-dialog"),
            "electron-store" => Some("tauri-plugin-store"),
            "autoUpdater" | "electron-updater" => Some("tauri-plugin-updater"),
            "Notification" => Some("tauri-plugin-notification"),
            "globalShortcut" => Some("tauri-plugin-global-shortcut"),
            "clipboard" => Some("tauri-plugin-clipboard-manager"),
            "app.requestSingleInstanceLock" => Some("tauri-plugin-single-instance"),
            "app.setAsDefaultProtocolClient" => Some("tauri-plugin-deep-link"),
            "electron-window-state" => Some("tauri-plugin-window-state"),
            _ => None,
        };
        if let Some(plugin) = plugin {
            plugins.insert(plugin);
        }
    }
    for m in &report.port_needing_deps {
        let plugin = match m.as_str() {
            "electron-store" => Some("tauri-plugin-store"),
            "electron-updater" => Some("tauri-plugin-updater"),
            "electron-window-state" => Some("tauri-plugin-window-state"),
            _ => None,
        };
        if let Some(plugin) = plugin {
            plugins.insert(plugin);
        }
    }
    let names: Vec<&'static str> = plugins.into_iter().collect();
    (names.len(), names)
}

/// Bundle/installer size prediction, before → after, in MB.
///
/// The after value is modeled from **measured e2r outputs**: the
/// zero-plugin release binary (11.0–17.4 MB, point 11.4 MB — the
/// high end absorbs the unexplained ~6 MB anomaly between two
/// near-identical e2r outputs) plus ~2 MB per plugin plus the measured
/// app payload (frontend assets embed into the binary ~1:1). This is
/// the default-profile binary e2r produces today — no
/// `[profile.release]`, unstripped — not the cited 1–15 MB
/// optimised-profile range, which the model mentions only as a
/// labeled estimate in the assumptions.
///
/// The before value is measured when an installed Electron runtime is
/// available (runtime on disk + app payload, ± the measurement
/// spread); otherwise it is the measured Electron installer range
/// (90–160 MB, point 106 MB — Dawloom's measured 106.1 MB NSIS plus
/// our measured 118.2 MiB compressed-runtime floor), clearly labeled
/// as a model estimate.
fn bundle_prediction(
    payload_mb: f64,
    runtime_mb: Option<f64>,
    plugin_count: usize,
    plugin_names: &[&str],
) -> Prediction {
    let floor = reference::TAURI_BINARY_ZERO_PLUGIN_MB;
    let per_plugin = reference::TAURI_PER_PLUGIN_MB;
    let optimised = reference::TAURI_OPTIMISED_INSTALLER_MB;
    let plugins = plugin_count as f64;
    let after = range3(
        floor.low + per_plugin.low * plugins + payload_mb,
        floor.point + per_plugin.point * plugins + payload_mb,
        floor.high + per_plugin.high * plugins + payload_mb,
    );
    let plugin_note = if plugin_names.is_empty() {
        "no plugins".to_string()
    } else {
        format!("{} plugin(s): {}", plugin_count, plugin_names.join(", "))
    };
    let after_basis = format!(
        "measured e2r output: 0-plugin release binary 11.0–17.4 MB (point 11.4 MB, Tauri 2.12.7, Linux, unstripped) + ~2 MB per plugin ({plugin_note}) + measured app payload ({payload_mb} MB) — RND.md §11"
    );
    let optimised_note = format!(
        "With the docs' size-optimised release profile (strip + LTO + opt-level=\"s\" + installer compression), a typical small app's installer is a MODEL ESTIMATE of ~{} MB ({}–{} MB) — cited benchmarks, not a measurement of e2r output, which emits no size profile",
        fmt_num(optimised.point),
        fmt_num(optimised.low),
        fmt_num(optimised.high)
    );
    match runtime_mb {
        Some(runtime) => {
            let spread = reference::MEASURED_RUNTIME_SPREAD.value;
            let before = range3(
                runtime * (1.0 - spread) + payload_mb,
                runtime + payload_mb,
                runtime * (1.0 + spread) + payload_mb,
            );
            Prediction {
                metric: "bundle_size".to_string(),
                before: Some(before),
                after: Some(after),
                unit: "MB".to_string(),
                confidence: Confidence::High,
                basis: format!(
                    "measured installed Electron runtime ({runtime} MB) + measured app payload ({payload_mb} MB); after is {after_basis}"
                ),
                assumptions: vec![
                    "The runtime on disk is not the installer size — compression and packaging change it, hence the ±5% range".to_string(),
                    "The app payload ships in both bundles, so it adds to both sides; on the Tauri side only frontend assets actually embed (~1:1), so the after figure slightly overestimates apps with large main processes".to_string(),
                    "The after range is the default-profile release binary e2r produces today (no [profile.release], unstripped); its high end absorbs the unexplained ~6 MB variance between two near-identical e2r outputs (RND.md §11)".to_string(),
                    optimised_note,
                ],
            }
        }
        None => {
            let electron = reference::ELECTRON_INSTALLER_MB;
            let before = range3(electron.low, electron.point, electron.high);
            Prediction {
                metric: "bundle_size".to_string(),
                before: Some(before),
                after: Some(after),
                unit: "MB".to_string(),
                confidence: Confidence::Medium,
                basis: format!(
                    "model estimate from the measured Electron installer range 90–160 MB (Dawloom measured 106.1 MB NSIS; our measured compressed-runtime floor is 118.2 MiB); after is {after_basis}"
                ),
                assumptions: vec![
                    "No installed Electron runtime was found (node_modules/electron/dist is missing) — the before size is a measured range for typical apps, not a measurement of this app".to_string(),
                    "The cited range already includes typical app code; very large apps exceed it".to_string(),
                    "The app payload ships in both bundles, so it adds to the Tauri side; on the Tauri side only frontend assets actually embed (~1:1), so the after figure slightly overestimates apps with large main processes".to_string(),
                    "The after range is the default-profile release binary e2r produces today (no [profile.release], unstripped); its high end absorbs the unexplained ~6 MB variance between two near-identical e2r outputs (RND.md §11)".to_string(),
                    optimised_note,
                ],
            }
        }
    }
}

/// Memory prediction for one target platform, as a fraction of current
/// memory usage (idle, one window).
///
/// Memory is platform-dependent, so there is no single number: macOS
/// sees a real win (WKWebView is lighter than bundled Chromium),
/// Windows sees about none (WebView2 IS Chromium), and Linux is
/// unmeasured — the model refuses to predict Linux memory rather than
/// invent a number. No baseline is measured (memory depends on
/// workload and the app is not running), so `before` is unknown and
/// the prediction is a ratio.
fn memory_prediction(platform: TargetPlatform) -> Prediction {
    let metric = format!("memory_{}", platform.id());
    let common_assumptions = vec![
        "No runtime memory baseline was measured for this app".to_string(),
        "The prediction is a ratio of current usage, not an absolute MB figure".to_string(),
        "Idle, one window only — loaded/active memory for a real app is unmeasured beyond a handful of anecdotes".to_string(),
        "Memory depends on workload; the ratio assumes a comparable workload after migration"
            .to_string(),
    ];
    match platform {
        TargetPlatform::Macos => {
            let ratio = reference::MEMORY_AFTER_RATIO_MACOS;
            Prediction {
                metric,
                before: None,
                after: Some(range3(ratio.low, ratio.point, ratio.high)),
                unit: "fraction of current memory".to_string(),
                confidence: Confidence::Medium,
                basis: "macOS idle, one window: wolinski measured 86.0 → 59.5 MiB (N=10 medians, macOS footprint); Elanis 364 → 96 MB; Hopp 409 → 172 MB (6 windows, N=1) — Tauri uses WKWebView on macOS".to_string(),
                assumptions: common_assumptions,
            }
        }
        TargetPlatform::Windows => {
            let ratio = reference::MEMORY_AFTER_RATIO_WINDOWS;
            Prediction {
                metric,
                before: None,
                after: Some(range3(ratio.low, ratio.point, ratio.high)),
                unit: "fraction of current memory".to_string(),
                confidence: Confidence::Low,
                basis: "Windows idle, one window: Dawloom 149 → 142 MB, lleg 103.3 → 108 MB (Tauri +4.5%), skathi 88.8 → 95.2 MiB (Tauri +7%) — 2 of 3 studies measured Tauri higher; WebView2 is Chromium, so the browser engine (the bulk of the memory) remains".to_string(),
                assumptions: common_assumptions,
            }
        }
        TargetPlatform::Linux => Prediction {
            metric,
            before: None,
            after: None,
            unit: "fraction of current memory".to_string(),
            confidence: Confidence::Unknown,
            basis: "unknown — no published Linux (WebKitGTK) idle-memory measurement exists; the model refuses to predict Linux memory".to_string(),
            assumptions: vec![
                "No Linux memory baseline exists in the published literature".to_string(),
                "The model declines to predict rather than invent a number".to_string(),
            ],
        },
    }
}

/// Startup-time prediction, as a multiple of current startup time
/// (1.0 = no change).
///
/// **The model never predicts a startup improvement.** Wolinski found
/// a tie on macOS (309/377 ms vs 321/326 ms to first frame, median of
/// 30 launches) and skathi found Electron FASTER on Windows first
/// frame (388 vs 522 ms, median of 5); Elanis and Better Stack also
/// found Tauri slower. The sources agree on the order of magnitude
/// (~0.3–0.6 s) but disagree on the sign, so the prediction is "no
/// meaningful change" — point exactly 1.0, interval 0.8–1.3×.
/// No baseline is measured, so `before` is unknown.
fn startup_prediction() -> Prediction {
    let ratio = reference::STARTUP_TIME_RATIO;
    Prediction {
        metric: "startup_time".to_string(),
        before: None,
        after: Some(range3(ratio.low, ratio.point, ratio.high)),
        unit: "multiple of current startup time".to_string(),
        confidence: Confidence::Low,
        basis: "no improvement predicted: wolinski found a startup tie on macOS (median of 30 launches) and skathi found Electron faster on Windows first frame (388 vs 522 ms) — published measurements disagree on which is faster, so the model predicts no meaningful change (0.8–1.3x)".to_string(),
        assumptions: vec![
            "No startup baseline was measured for this app".to_string(),
            "The prediction is a ratio of current startup time (1.0 = no change), not an absolute time".to_string(),
            "A cold-start advantage from the smaller binary on slow disks is plausible but unmeasured — not modeled".to_string(),
        ],
    }
}

/// Process-count prediction for one target platform, before → after.
///
/// Structural and measured, from the window count the scan found:
/// Electron runs 4 processes for one window (main + renderer + GPU +
/// network/utility — Dawloom, skathi, wolinski) and adds one renderer
/// per extra window (Electron docs). Tauri runs 4 on macOS (wolinski),
/// **7 on Windows** (Dawloom and skathi agree exactly — the count goes
/// UP), and ~4 on Linux (unmeasured, inferred from the WebKitGTK
/// process model). Multi-window Tauri scaling is unmeasured, so the
/// after high end widens by an extra process per additional window.
fn process_prediction(
    window_count: usize,
    windows_found: bool,
    platform: TargetPlatform,
) -> Prediction {
    let windows = window_count.max(1) as f64;
    let extra_windows = windows - 1.0;
    let electron = reference::ELECTRON_PROCESSES_ONE_WINDOW;
    let before = range3(
        electron.low + extra_windows,
        electron.point + extra_windows,
        electron.high + extra_windows,
    );
    let (tauri, confidence, basis) = match platform {
        TargetPlatform::Macos => (
            reference::TAURI_PROCESSES_ONE_WINDOW_MACOS,
            Confidence::Medium,
            "structural + measured: Electron 4 processes (main + renderer + GPU + network, wolinski) vs Tauri 4 (Rust app + WebKit page + GPU + networking, wolinski) — no process-count change on macOS",
        ),
        TargetPlatform::Windows => (
            reference::TAURI_PROCESSES_ONE_WINDOW_WINDOWS,
            Confidence::High,
            "structural + measured: Electron 4 processes vs Tauri 7 (Rust host + WebView2 browser + renderer + GPU + helpers; Dawloom and skathi agree exactly) — the count RISES on Windows, 4 → 7",
        ),
        TargetPlatform::Linux => (
            reference::TAURI_PROCESSES_ONE_WINDOW_LINUX,
            Confidence::Low,
            "structural + inferred: Electron ~4 processes vs Tauri ~4 (host + WebKitGTK web/network/GPU processes) — UNMEASURED, inferred from the WebKitGTK process model; no published Linux count exists",
        ),
    };
    let after = range3(
        tauri.low + extra_windows,
        tauri.point + extra_windows,
        tauri.high + 2.0 * extra_windows,
    );
    let window_assumption = if windows_found {
        format!("{} window(s) found by the scan", window_count)
    } else {
        "No BrowserWindow found — assumed 1 window".to_string()
    };
    Prediction {
        metric: format!("process_count_{}", platform.id()),
        before: Some(before),
        after: Some(after),
        unit: "processes".to_string(),
        confidence,
        basis: basis.to_string(),
        assumptions: vec![
            window_assumption,
            "Electron adds one renderer process per extra window (Electron docs)".to_string(),
            "Multi-window Tauri process scaling is unmeasured — the after high end widens by an extra process per additional window".to_string(),
        ],
    }
}

/// CPU prediction: unknown, with no numbers.
///
/// There is no reliable CPU baseline (the app is not running) and no
/// usable CPU model in the reference data: the only rigorous
/// measurement (skathi) found idle CPU within ~1–4 Windows accounting
/// ticks for both frameworks — measurement noise — and no published
/// measurement of CPU under load exists at all. The model declines to
/// predict rather than invent a number.
fn cpu_prediction() -> Prediction {
    Prediction {
        metric: "cpu".to_string(),
        before: None,
        after: None,
        unit: "percent CPU".to_string(),
        confidence: Confidence::Unknown,
        basis: "unknown — no reliable baseline: idle CPU is noise-level (<1% of one core) in both frameworks in the one rigorous measurement (skathi), and CPU under load is unmeasured anywhere".to_string(),
        assumptions: vec![
            "CPU use depends on workload; no baseline was measured".to_string(),
            "The model declines to predict rather than invent a number".to_string(),
        ],
    }
}

/// All performance predictions, in fixed metric order. Calibration
/// adjustments (from the calibration log) are applied to the *after*
/// ranges only — the before baselines are measurements or cited
/// ranges, not predictions. An adjusted prediction says so in its
/// assumptions, with the multiplier and the comparison count.
fn predict(
    report: &ScanReport,
    payload_mb: f64,
    runtime_mb: Option<f64>,
    plugin_count: usize,
    plugin_names: &[&str],
    platform: TargetPlatform,
    adjustments: &BTreeMap<String, crate::calibrate::IntervalAdjustment>,
) -> Vec<Prediction> {
    let mut predictions = vec![
        bundle_prediction(payload_mb, runtime_mb, plugin_count, plugin_names),
        memory_prediction(platform),
        startup_prediction(),
        process_prediction(report.windows.len(), !report.windows.is_empty(), platform),
        cpu_prediction(),
    ];
    for prediction in &mut predictions {
        if let (Some(after), Some(adjustment)) =
            (prediction.after, adjustments.get(&prediction.metric))
        {
            let adjusted = crate::calibrate::adjust_range(&after, adjustment);
            if adjusted != after {
                prediction.after = Some(adjusted);
                prediction.assumptions.push(format!(
                    "Confidence interval adjusted by calibration: x{} ({} logged comparisons, {} inside their predicted interval)",
                    adjustment.multiplier, adjustment.comparisons, adjustment.inside
                ));
            }
        }
    }
    predictions
}

// --- Warnings ---------------------------------------------------------------

/// Find DRM (Widevine) references in app sources: file + line of each
/// match, sorted and deduplicated.
fn find_drm_references(app_dir: &Path) -> Vec<(String, usize, String)> {
    let mut found: BTreeSet<(String, usize, String)> = BTreeSet::new();
    for entry in WalkDir::new(app_dir)
        .into_iter()
        .filter_entry(|e| {
            !(e.depth() > 0
                && e.file_type().is_dir()
                && e.file_name()
                    .to_str()
                    .map(|n| SKIP_DIRS.contains(&n))
                    .unwrap_or(false))
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let is_scannable = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| DRM_SCAN_EXTS.contains(&e))
            .unwrap_or(false);
        if !is_scannable {
            continue;
        }
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = relative_path(app_dir, path);
        for (idx, line) in content.lines().enumerate() {
            if is_comment_line(line) {
                continue;
            }
            let lower = line.to_lowercase();
            for marker in reference::DRM_MARKERS {
                if lower.contains(marker) {
                    found.insert((rel.clone(), idx + 1, (*marker).to_string()));
                }
            }
        }
    }
    found.into_iter().collect()
}

/// Warnings for the report, built from the scan findings, the renderer's
/// caveats, and the DRM scan. Sorted and deduplicated.
fn collect_warnings(report: &ScanReport, plan: &RendererPlan, app_dir: &Path) -> Vec<String> {
    let mut warnings: BTreeSet<String> = BTreeSet::new();

    for module in &report.native_modules {
        warnings.insert(format!(
            "Native module `{module}` needs a Rust port — there is no automatic conversion."
        ));
    }

    // MANUAL findings, grouped by API: total call sites across files,
    // first occurrence's location and note.
    let mut manual: BTreeMap<&str, (usize, &str, usize, &str)> = BTreeMap::new();
    for finding in &report.findings {
        if finding.classification != crate::model::Classification::Manual {
            continue;
        }
        let entry = manual.entry(finding.api.as_str()).or_insert((
            0,
            finding.file.as_str(),
            finding.line,
            finding.note.as_str(),
        ));
        entry.0 += finding.occurrences;
    }
    for (api, (sites, file, line, note)) in &manual {
        let noun = if *sites == 1 {
            "call site"
        } else {
            "call sites"
        };
        warnings.insert(format!(
            "MANUAL API `{api}` ({sites} {noun}) — {note} (first seen at {file}:{line})"
        ));
    }

    if report.findings.iter().any(|f| f.api == "nodeIntegration") {
        warnings.insert(
            "nodeIntegration is enabled — the renderer runs Node APIs directly; every Node call site in renderer code needs a bridge call or a port."
                .to_string(),
        );
    }
    if report.findings.iter().any(|f| f.api == "contextIsolation") {
        warnings.insert(
            "contextIsolation is disabled — the preload bridge assumes isolation; review the security model before converting."
                .to_string(),
        );
    }

    for api in reference::CHROMIUM_ONLY_APIS {
        if report.findings.iter().any(|f| f.api == *api) {
            warnings.insert(format!(
                "Chromium-only API `{api}` — the system webview has no equivalent; it needs a manual design."
            ));
        }
    }

    for (file, line, marker) in find_drm_references(app_dir) {
        warnings.insert(format!(
            "DRM ({marker}) reference found at {file}:{line} — Widevine is Chromium-only and has no Tauri equivalent."
        ));
    }

    let uses_ipc_renderer = report
        .findings
        .iter()
        .any(|f| f.api.starts_with("ipcRenderer"));
    if report.preload.files.is_empty() && uses_ipc_renderer {
        warnings.insert(
            "ipcRenderer is used but no preload script was found — the bridge has to be invented, not translated."
                .to_string(),
        );
    }

    // The renderer's own caveats (stale build output, electron-vite /
    // Next.js build changes, ...) are warnings too — reuse them verbatim.
    for caveat in &plan.caveats {
        warnings.insert(caveat.clone());
    }

    warnings.into_iter().collect()
}

// --- Effort hint -----------------------------------------------------------

/// Rough effort hint from the MANUAL share and the native-module count.
/// A guide, not a quote — the note says what it does and does not
/// account for.
fn effort_hint(breakdown: &Breakdown, native_modules: usize) -> (Effort, String) {
    let manual_share = if breakdown.total > 0 {
        breakdown.manual as f64 / breakdown.total as f64
    } else {
        0.0
    };
    let effort = if manual_share >= reference::EFFORT_MANUAL_SHARE_LARGE.value
        || native_modules as f64 >= reference::EFFORT_NATIVE_MODULES_LARGE.value
    {
        Effort::Large
    } else if breakdown.manual > 0 || native_modules == 1 {
        Effort::Medium
    } else if breakdown.assisted > 0 {
        Effort::Small
    } else {
        Effort::Trivial
    };
    let note = format!(
        "Rough guide only: derived from the MANUAL share ({:.1}% of {} call sites) and {} native module(s). It does not account for code size, test coverage, or the review effort behind ASSISTED items.",
        manual_share * 100.0,
        breakdown.total,
        native_modules
    );
    (effort, note)
}

// --- Timestamps ------------------------------------------------------------

/// Format a Unix timestamp (seconds since the epoch) as RFC 3339 UTC
/// (`2026-11-25T04:00:00Z`). Civil-date conversion is Howard Hinnant's
/// days-from-civil algorithm, inverted; no date crate is needed for a
/// UTC timestamp with no sub-second precision.
fn rfc3339_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// The current time as RFC 3339 UTC, for the predictions record, the
/// calibration log, and the decision registry.
pub fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_from_unix(secs)
}

// --- Report formatting -----------------------------------------------------

/// Format a number without trailing zeros: `122.5`, `85`, `0.35`.
fn fmt_num(value: f64) -> String {
    let text = format!("{value:.3}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-0" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Format a percentage with one decimal: `90.9`, `100.0`.
fn fmt_pct(value: f64) -> String {
    format!("{value:.1}")
}

/// Format a range as `low–high (point p)`.
fn fmt_range(range: &Range) -> String {
    format!(
        "{}–{} (point {})",
        fmt_num(range.low),
        fmt_num(range.high),
        fmt_num(range.point)
    )
}

/// Confidence as a lowercase word.
fn fmt_confidence(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
        Confidence::Unknown => "unknown",
    }
}

/// Effort as a lowercase word.
fn fmt_effort(effort: Effort) -> &'static str {
    match effort {
        Effort::Trivial => "trivial",
        Effort::Small => "small",
        Effort::Medium => "medium",
        Effort::Large => "large",
    }
}

/// One prediction as a human-readable line: `metric: before → after —
/// confidence`, with the basis on the next line. Unknown predictions
/// print no numbers at all.
fn fmt_prediction_human(prediction: &Prediction) -> String {
    let mut out = String::new();
    let name = prediction.metric.replace('_', " ");
    match (&prediction.before, &prediction.after) {
        (_, None) => {
            out.push_str(&format!(
                "  {name}: unknown — no reliable baseline\n    Basis: {}\n",
                prediction.basis
            ));
        }
        (None, Some(after)) => {
            out.push_str(&format!(
                "  {name}: unknown baseline → {} {} — {} confidence\n    Basis: {}\n",
                fmt_range(after),
                prediction.unit,
                fmt_confidence(prediction.confidence),
                prediction.basis
            ));
        }
        (Some(before), Some(after)) => {
            out.push_str(&format!(
                "  {name}: {} {} → {} {} — {} confidence\n    Basis: {}\n",
                fmt_range(before),
                prediction.unit,
                fmt_range(after),
                prediction.unit,
                fmt_confidence(prediction.confidence),
                prediction.basis
            ));
        }
    }
    if !prediction.assumptions.is_empty() {
        out.push_str("    Assumes: ");
        out.push_str(&prediction.assumptions.join("; "));
        out.push('\n');
    }
    out
}

/// Render a [`SimulationReport`] as a human-readable summary for the
/// terminal. Plain text, no ANSI escapes.
pub fn format_human_report(report: &SimulationReport) -> String {
    let mut out = String::new();

    let title = format!("Migration simulation: {}", report.app_name);
    out.push_str(&title);
    out.push('\n');
    out.push_str(&"=".repeat(title.chars().count()));
    out.push_str("\n\n");
    if let Some(version) = &report.app_version {
        out.push_str(&format!("App version: {version}\n"));
    }
    out.push_str(&format!(
        "Feasibility: {}/100 — {}\n",
        report.feasibility_score, report.verdict
    ));
    out.push_str(&format!(
        "Model: {} (research-backed — predictions are ranges with confidence levels, not measurements)\n",
        report.model_version
    ));

    let breakdown = &report.breakdown;
    out.push_str(&format!(
        "\nMigratable breakdown ({} API call sites)\n",
        breakdown.total
    ));
    out.push_str(&format!(
        "  AUTO      {} ({}%)\n",
        breakdown.auto,
        fmt_pct(breakdown.auto_pct)
    ));
    out.push_str(&format!(
        "  ASSISTED  {} ({}%)\n",
        breakdown.assisted,
        fmt_pct(breakdown.assisted_pct)
    ));
    out.push_str(&format!(
        "  MANUAL    {} ({}%)\n",
        breakdown.manual,
        fmt_pct(breakdown.manual_pct)
    ));

    let ui = &report.ui_preservation;
    out.push_str("\nUI preservation (ui-preserving / system webview mode)\n");
    out.push_str(&format!(
        "  Estimated carry-over: {}% unchanged\n",
        fmt_pct(ui.estimated_pct)
    ));
    out.push_str(&format!(
        "  Framework: {} | Bundler: {} | Renderer: {}\n",
        ui.framework, ui.bundler, ui.renderer_kind
    ));
    out.push_str("  Factors:\n");
    for factor in &ui.factors {
        out.push_str(&format!(
            "    - {} — {}% — {}\n",
            factor.name,
            fmt_pct(factor.carry_over_pct),
            factor.detail
        ));
    }
    out.push_str(&format!(
        "  Electron-API touch points ({})\n",
        ui.touch_points.len()
    ));
    if ui.touch_points.is_empty() {
        out.push_str("    (none)\n");
    }
    for point in &ui.touch_points {
        out.push_str(&format!(
            "    - {} — {}:{} — {}\n",
            point.api, point.file, point.line, point.detail
        ));
    }
    out.push_str(&format!("  Note: {}\n", ui.note));

    out.push_str("\nPredicted gains (before → after)\n");
    for prediction in &report.predictions {
        out.push_str(&fmt_prediction_human(prediction));
    }

    out.push_str(&format!("\nWarnings ({})\n", report.warnings.len()));
    if report.warnings.is_empty() {
        out.push_str("  (none)\n");
    }
    for warning in &report.warnings {
        out.push_str(&format!("  - {warning}\n"));
    }

    out.push_str(&format!(
        "\nEffort hint: {} (rough guide)\n  {}\n",
        fmt_effort(report.effort),
        report.effort_note
    ));
    out
}

/// One prediction as a Markdown table row. Unknown predictions carry no
/// numbers.
fn fmt_prediction_markdown(prediction: &Prediction) -> String {
    let name = prediction.metric.replace('_', " ");
    let (before, after) = match (&prediction.before, &prediction.after) {
        (_, None) => (
            "unknown".to_string(),
            "unknown — no reliable baseline".to_string(),
        ),
        (None, Some(after)) => (
            "unknown baseline".to_string(),
            format!("{} {}", fmt_range(after), prediction.unit),
        ),
        (Some(before), Some(after)) => (
            format!("{} {}", fmt_range(before), prediction.unit),
            format!("{} {}", fmt_range(after), prediction.unit),
        ),
    };
    format!(
        "| {} | {} | {} | {} | {} |\n",
        name,
        before,
        after,
        fmt_confidence(prediction.confidence),
        prediction.basis
    )
}

/// Render a [`SimulationReport`] as Markdown: the same content as the
/// human report, condensed into headings and tables for pasting into
/// issues and PRs. No ANSI escapes.
pub fn format_markdown_report(report: &SimulationReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Migration simulation: {}\n\n", report.app_name));
    if let Some(version) = &report.app_version {
        out.push_str(&format!("- App version: `{version}`\n"));
    }
    out.push_str(&format!(
        "- Feasibility: **{}/100** — {}\n",
        report.feasibility_score, report.verdict
    ));
    out.push_str(&format!(
        "- Model: `{}` (research-backed — predictions are ranges with confidence levels, not measurements)\n",
        report.model_version
    ));

    let breakdown = &report.breakdown;
    out.push_str(&format!(
        "\n## Migratable breakdown ({} API call sites)\n\n",
        breakdown.total
    ));
    out.push_str("| Class | Call sites | Share |\n|---|---:|---:|\n");
    out.push_str(&format!(
        "| AUTO | {} | {}% |\n",
        breakdown.auto,
        fmt_pct(breakdown.auto_pct)
    ));
    out.push_str(&format!(
        "| ASSISTED | {} | {}% |\n",
        breakdown.assisted,
        fmt_pct(breakdown.assisted_pct)
    ));
    out.push_str(&format!(
        "| MANUAL | {} | {}% |\n",
        breakdown.manual,
        fmt_pct(breakdown.manual_pct)
    ));

    let ui = &report.ui_preservation;
    out.push_str("\n## UI preservation\n\n");
    out.push_str(&format!(
        "Estimated carry-over: **{}%** unchanged (ui-preserving / system webview mode).\n\n",
        fmt_pct(ui.estimated_pct)
    ));
    out.push_str(&format!(
        "Framework: `{}` | Bundler: `{}` | Renderer: `{}`\n\n",
        ui.framework, ui.bundler, ui.renderer_kind
    ));
    out.push_str("| Factor | Carry-over | Detail |\n|---|---:|---|\n");
    for factor in &ui.factors {
        out.push_str(&format!(
            "| {} | {}% | {} |\n",
            factor.name,
            fmt_pct(factor.carry_over_pct),
            factor.detail
        ));
    }
    out.push_str(&format!(
        "\nElectron-API touch points ({}):\n\n",
        ui.touch_points.len()
    ));
    if ui.touch_points.is_empty() {
        out.push_str("(none)\n");
    }
    for point in &ui.touch_points {
        out.push_str(&format!(
            "- `{}` — `{}:{}` — {}\n",
            point.api, point.file, point.line, point.detail
        ));
    }
    out.push_str(&format!("\n> {}\n", ui.note));

    out.push_str("\n## Predicted gains\n\n");
    out.push_str("| Metric | Before | After | Confidence | Basis |\n|---|---|---|---|---|\n");
    for prediction in &report.predictions {
        out.push_str(&fmt_prediction_markdown(prediction));
    }

    out.push_str(&format!("\n## Warnings ({})\n\n", report.warnings.len()));
    if report.warnings.is_empty() {
        out.push_str("(none)\n");
    }
    for warning in &report.warnings {
        out.push_str(&format!("- {warning}\n"));
    }

    out.push_str(&format!(
        "\n## Effort hint\n\n**{}** (rough guide) — {}\n",
        fmt_effort(report.effort),
        report.effort_note
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The fixture app dir, relative to this crate's manifest dir.
    fn fixture_dir(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/{name}"))
    }

    fn all_fixtures() -> Vec<PathBuf> {
        vec![
            fixture_dir("minimal-electron-app"),
            fixture_dir("full-apis-electron-app"),
            fixture_dir("built-output-electron-app"),
        ]
    }

    /// Simulate with calibration disabled, so unit tests never depend
    /// on the developer's `~/.cache/e2r/calibration.jsonl`. The host
    /// platform is the target platform (this VM is Linux).
    fn simulate_no_calibration(app_dir: &Path) -> Result<SimulationReport> {
        simulate_with_options(
            app_dir,
            &SimulateOptions {
                target_platform: TargetPlatform::host(),
                calibration: CalibrationInput::Disabled,
            },
        )
    }

    /// Simulate for an explicit target platform, calibration disabled.
    fn simulate_for_platform(app_dir: &Path, platform: TargetPlatform) -> Result<SimulationReport> {
        simulate_with_options(
            app_dir,
            &SimulateOptions {
                target_platform: platform,
                calibration: CalibrationInput::Disabled,
            },
        )
    }

    static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A fresh empty directory under the system temp dir, unique per call.
    fn fresh_test_dir(tag: &str) -> PathBuf {
        let n = TEST_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("e2r-sim-test-{tag}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn write_test_file(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create test parent dir");
        }
        fs::write(&path, contents).expect("write test file");
    }

    // ---- Determinism ------------------------------------------------------

    #[test]
    fn same_input_twice_gives_identical_json() {
        for fixture in all_fixtures() {
            let first = simulate_no_calibration(&fixture).expect("simulate");
            let second = simulate_no_calibration(&fixture).expect("simulate again");
            let first_json = serde_json::to_string_pretty(&first).expect("serialize");
            let second_json = serde_json::to_string_pretty(&second).expect("serialize");
            assert_eq!(
                first_json,
                second_json,
                "simulate must be deterministic for {}",
                fixture.display()
            );
            // And the rendered formats are deterministic too.
            assert_eq!(
                format_human_report(&first),
                format_human_report(&second),
                "human report must be deterministic for {}",
                fixture.display()
            );
            assert_eq!(
                format_markdown_report(&first),
                format_markdown_report(&second),
                "markdown report must be deterministic for {}",
                fixture.display()
            );
        }
    }

    // ---- Interval sanity --------------------------------------------------

    #[test]
    fn every_prediction_range_is_ordered_on_all_fixtures() {
        for fixture in all_fixtures() {
            let report = simulate_no_calibration(&fixture).expect("simulate");
            for prediction in &report.predictions {
                for (side, range) in [("before", &prediction.before), ("after", &prediction.after)]
                {
                    if let Some(range) = range {
                        assert!(
                            range.is_valid(),
                            "{} {} range must satisfy low < point < high, got {:?}",
                            prediction.metric,
                            side,
                            range
                        );
                    }
                }
            }
        }
    }

    // ---- Breakdown math ---------------------------------------------------

    #[test]
    fn breakdown_matches_scan_findings_weighted_by_occurrences() {
        for fixture in all_fixtures() {
            let scanned = scan::scan(&fixture).expect("scan");
            let report = simulate_no_calibration(&fixture).expect("simulate");
            let mut expected = [0usize; 3];
            for finding in &scanned.findings {
                match finding.classification {
                    crate::model::Classification::Auto => expected[0] += finding.occurrences,
                    crate::model::Classification::Assisted => expected[1] += finding.occurrences,
                    crate::model::Classification::Manual => expected[2] += finding.occurrences,
                }
            }
            assert_eq!(
                (
                    report.breakdown.auto,
                    report.breakdown.assisted,
                    report.breakdown.manual
                ),
                (expected[0], expected[1], expected[2]),
                "breakdown counts must match the scan, weighted by occurrences, for {}",
                fixture.display()
            );
            assert_eq!(
                report.breakdown.total,
                expected.iter().sum::<usize>(),
                "total must be the sum of the classes for {}",
                fixture.display()
            );
            // Percentages sum to 100 (within float error) when there are
            // call sites at all.
            let sum = report.breakdown.auto_pct
                + report.breakdown.assisted_pct
                + report.breakdown.manual_pct;
            assert!(
                (sum - 100.0).abs() < 1e-6,
                "percentages must sum to 100, got {sum} for {}",
                fixture.display()
            );
            // The feasibility score and verdict come straight from the scan.
            assert_eq!(report.feasibility_score, scanned.feasibility_score);
            assert_eq!(report.verdict, scanned.verdict);
        }
    }

    #[test]
    fn percentages_sum_to_100_for_awkward_splits() {
        // 1/3 each: largest-remainder must still land on exactly 100.0.
        let (a, b, c) = percentages([1, 1, 1], 3);
        assert!((a + b + c - 100.0).abs() < 1e-6);
        assert!((a - 33.4).abs() < 1e-9 || (a - 33.3).abs() < 1e-9);
        // 1/7-ish splits.
        let (a, b, c) = percentages([1, 2, 4], 7);
        assert!((a + b + c - 100.0).abs() < 1e-6);
        // One class empty.
        let (a, b, c) = percentages([0, 3, 7], 10);
        assert_eq!((a, b, c), (0.0, 30.0, 70.0));
    }

    // ---- UI preservation --------------------------------------------------

    #[test]
    fn built_output_fixture_carries_over_more_than_heavy_electron_renderer() {
        let built =
            simulate_no_calibration(&fixture_dir("built-output-electron-app")).expect("simulate");
        assert_eq!(built.ui_preservation.renderer_kind, "built-output");
        assert_eq!(built.ui_preservation.bundler, "vite");
        assert!(
            built.ui_preservation.touch_points.is_empty(),
            "the built-output fixture's renderer uses the bridge only; touch points: {:?}",
            built.ui_preservation.touch_points
        );

        // A renderer that leans on Electron directly: nodeIntegration plus
        // window.require, direct ipcRenderer, remote, and Node builtins.
        let app = fresh_test_dir("heavy-renderer");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"heavy","main":"main.js","version":"1.0.0","dependencies":{"electron":"^30.0.0"}}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             new BrowserWindow({ width: 800, height: 600, webPreferences: { nodeIntegration: true } });\n",
        );
        write_test_file(
            &app,
            "index.html",
            "<!doctype html><html><body><script src=\"renderer.js\"></script></body></html>\n",
        );
        write_test_file(
            &app,
            "renderer.js",
            "const { ipcRenderer, remote } = window.require('electron');\n\
             const fs = require('fs');\n\
             const path = require('node:path');\n\
             ipcRenderer.send('ping');\n\
             remote.dialog.showMessageBoxSync({});\n\
             console.log(process.versions.node, fs.readFileSync('x'), path.join('a', 'b'));\n",
        );
        let heavy = simulate_no_calibration(&app).expect("simulate heavy app");
        assert!(
            !heavy.ui_preservation.touch_points.is_empty(),
            "the heavy renderer must produce touch points"
        );
        // Every touch point carries its file and line.
        for point in &heavy.ui_preservation.touch_points {
            assert_eq!(point.file, "renderer.js");
            assert!(point.line >= 1 && point.line <= 6);
        }
        assert!(
            heavy
                .ui_preservation
                .touch_points
                .iter()
                .any(|p| p.api == "window.require('electron')"),
            "window.require must be a touch point; got {:?}",
            heavy.ui_preservation.touch_points
        );
        assert!(
            heavy
                .ui_preservation
                .touch_points
                .iter()
                .any(|p| p.api == "Node builtin 'fs' in renderer"),
            "Node builtins must be touch points; got {:?}",
            heavy.ui_preservation.touch_points
        );
        assert!(
            heavy.ui_preservation.estimated_pct < built.ui_preservation.estimated_pct,
            "heavy Electron renderer ({}%) must carry over less than built output ({}%)",
            heavy.ui_preservation.estimated_pct,
            built.ui_preservation.estimated_pct
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn ui_preservation_is_zero_with_no_renderer() {
        let app = fresh_test_dir("no-renderer");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"norender","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             new BrowserWindow({ width: 800, height: 600 });\n",
        );
        let report = simulate_no_calibration(&app).expect("simulate");
        assert_eq!(report.ui_preservation.renderer_kind, "none");
        assert_eq!(report.ui_preservation.framework, "unknown");
        assert_eq!(report.ui_preservation.estimated_pct, 0.0);
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn framework_and_bundler_are_detected_from_package_json() {
        let app = fresh_test_dir("react-vite");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"rv","main":"main.js","version":"1.0.0","dependencies":{"react":"^18.0.0"},"devDependencies":{"vite":"^5.0.0"},"scripts":{"build":"vite build","dev":"vite"}}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             new BrowserWindow({ width: 800, height: 600 });\n",
        );
        write_test_file(&app, "src/main.jsx", "import React from 'react';\n");
        write_test_file(
            &app,
            "index.html",
            "<!doctype html><html><body></body></html>\n",
        );
        let report = simulate_no_calibration(&app).expect("simulate");
        assert_eq!(report.ui_preservation.framework, "react");
        assert_eq!(report.ui_preservation.bundler, "vite");
        assert_eq!(report.ui_preservation.renderer_kind, "source");
        fs::remove_dir_all(&app).ok();
    }

    // ---- Honesty: no planning-chat sample figures --------------------------

    /// The planning chat's illustrative sample figures must never appear
    /// in simulator output — as measurements, as model outputs, or as
    /// predictions. The percentages (82%/94%) are checked against the
    /// predictions specifically: a computed breakdown share could
    /// legitimately equal one of them on some other app, but a prediction
    /// must never be one.
    #[test]
    fn simulator_output_contains_no_planning_chat_sample_figures() {
        const FORBIDDEN_ANYWHERE: &[&str] = &[
            "2.1s",
            "380 MB",
            "265 MB",
            "120 MB",
            "28 MB",
            "120 MB → 28 MB",
        ];
        const FORBIDDEN_AS_PREDICTIONS: &[&str] = &["82%", "94%", "22%", "9%"];
        for fixture in all_fixtures() {
            let report = simulate_no_calibration(&fixture).expect("simulate");
            let json = serde_json::to_string_pretty(&report).expect("serialize");
            let human = format_human_report(&report);
            let markdown = format_markdown_report(&report);
            let predictions_json =
                serde_json::to_string_pretty(&report.predictions).expect("serialize");
            for needle in FORBIDDEN_ANYWHERE {
                for (format, text) in [("JSON", &json), ("human", &human), ("markdown", &markdown)]
                {
                    assert!(
                        !text.contains(needle),
                        "{format} output for {} must not contain the planning-chat sample figure {needle:?}",
                        fixture.display()
                    );
                }
            }
            for needle in FORBIDDEN_AS_PREDICTIONS {
                assert!(
                    !predictions_json.contains(needle),
                    "predictions for {} must not contain the planning-chat sample figure {needle:?}",
                    fixture.display()
                );
            }
        }
    }

    // ---- CPU: unknown with no numbers --------------------------------------

    #[test]
    fn measured_runtime_branch_uses_disk_size_and_rejects_stubs() {
        use std::fs::File;
        // A stub dist (2 MB) is not a real installed runtime: the model
        // falls back to the measured 90–160 MB installer range.
        let stub = fresh_test_dir("runtime-stub");
        write_test_file(
            &stub,
            "package.json",
            r#"{"name":"stub","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &stub,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\nnew BrowserWindow({});\n",
        );
        write_test_file(&stub, "index.html", "<!doctype html><html></html>\n");
        let stub_dist = stub.join("node_modules/electron/dist");
        fs::create_dir_all(&stub_dist).expect("create stub dist");
        File::create(stub_dist.join("electron"))
            .expect("create stub binary")
            .set_len(2_000_000)
            .expect("size stub binary");
        let stub_report = simulate_no_calibration(&stub).expect("simulate stub");
        let stub_bundle = stub_report
            .predictions
            .iter()
            .find(|p| p.metric == "bundle_size")
            .expect("bundle prediction");
        assert_eq!(stub_bundle.confidence, Confidence::Medium);
        assert_eq!(
            stub_bundle.before.expect("before range").low,
            90.0,
            "a 2 MB stub dist must fall back to the measured installer range"
        );

        // A real-sized dist (60 MB sparse file — metadata length counts,
        // no blocks written) is measured: the before point is the disk
        // size plus the payload, with high confidence (both sides are
        // measured: the runtime on disk and the e2r outputs).
        let real = fresh_test_dir("runtime-real");
        write_test_file(
            &real,
            "package.json",
            r#"{"name":"real","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &real,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\nnew BrowserWindow({});\n",
        );
        write_test_file(&real, "index.html", "<!doctype html><html></html>\n");
        let real_dist = real.join("node_modules/electron/dist");
        fs::create_dir_all(&real_dist).expect("create real dist");
        File::create(real_dist.join("electron"))
            .expect("create real binary")
            .set_len(60_000_000)
            .expect("size real binary");
        let real_report = simulate_no_calibration(&real).expect("simulate real");
        let real_bundle = real_report
            .predictions
            .iter()
            .find(|p| p.metric == "bundle_size")
            .expect("bundle prediction");
        assert_eq!(real_bundle.confidence, Confidence::High);
        let before = real_bundle.before.expect("before range");
        assert!(
            before.point >= 60.0 && before.point < 61.0,
            "measured before point must be ~60 MB + payload, got {}",
            before.point
        );
        assert!(before.is_valid());
        fs::remove_dir_all(&stub).ok();
        fs::remove_dir_all(&real).ok();
    }

    #[test]
    fn drm_scan_ignores_comments_but_flags_code() {
        let app = fresh_test_dir("drm-comment");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"drm","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             // Widevine DRM was considered here and rejected.\n\
             const drm = 'widevine';\n\
             new BrowserWindow({ width: 800, height: 600 });\n",
        );
        write_test_file(&app, "index.html", "<!doctype html><html></html>\n");
        let report = simulate_no_calibration(&app).expect("simulate");
        let drm_warnings: Vec<&String> = report
            .warnings
            .iter()
            .filter(|w| w.contains("widevine"))
            .collect();
        assert_eq!(
            drm_warnings.len(),
            1,
            "only the code reference (not the comment) must warn; warnings: {:?}",
            report.warnings
        );
        assert!(
            drm_warnings[0].contains("main.js:3"),
            "the warning must point at the code line; got {}",
            drm_warnings[0]
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn cpu_prediction_is_unknown_with_no_numbers() {
        for fixture in all_fixtures() {
            let report = simulate_no_calibration(&fixture).expect("simulate");
            let cpu = report
                .predictions
                .iter()
                .find(|p| p.metric == "cpu")
                .expect("cpu prediction present");
            assert_eq!(cpu.confidence, Confidence::Unknown);
            assert_eq!(cpu.before, None, "cpu must carry no before number");
            assert_eq!(cpu.after, None, "cpu must carry no after number");
            assert!(cpu.basis.contains("unknown"));
        }
    }

    // ---- Warnings and effort ----------------------------------------------

    #[test]
    fn warnings_cover_native_modules_manual_apis_and_node_integration() {
        let app = fresh_test_dir("warnings");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"warn","main":"main.js","version":"1.0.0","dependencies":{"node-pty":"^1.0.0"}}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow, session } = require('electron');\n\
             session.defaultSession.cookies.get({});\n\
             new BrowserWindow({ width: 800, height: 600, webPreferences: { nodeIntegration: true } });\n",
        );
        write_test_file(
            &app,
            "index.html",
            "<!doctype html><html><body></body></html>\n",
        );
        let report = simulate_no_calibration(&app).expect("simulate");
        let joined = report.warnings.join("\n");
        assert!(
            joined.contains("node-pty"),
            "native module must be warned about; warnings: {joined}"
        );
        assert!(
            joined.contains("session"),
            "MANUAL/Chromium-only session API must be warned about; warnings: {joined}"
        );
        assert!(
            joined.contains("nodeIntegration"),
            "nodeIntegration must be warned about; warnings: {joined}"
        );
        assert!(
            matches!(report.effort, Effort::Medium | Effort::Large),
            "a native module plus a MANUAL API is at least medium effort; got {:?}",
            report.effort
        );
        // Warnings are sorted and deduplicated.
        let mut sorted = report.warnings.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(report.warnings, sorted, "warnings must be sorted + deduped");
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn effort_hint_thresholds() {
        let breakdown = |auto: usize, assisted: usize, manual: usize| Breakdown {
            auto,
            assisted,
            manual,
            total: auto + assisted + manual,
            auto_pct: 0.0,
            assisted_pct: 0.0,
            manual_pct: 0.0,
        };
        // All AUTO, no native modules: trivial.
        assert_eq!(effort_hint(&breakdown(10, 0, 0), 0).0, Effort::Trivial);
        // Only ASSISTED items: small.
        assert_eq!(effort_hint(&breakdown(10, 3, 0), 0).0, Effort::Small);
        // One MANUAL call site: medium.
        assert_eq!(effort_hint(&breakdown(10, 0, 1), 0).0, Effort::Medium);
        // One native module, no MANUAL: medium.
        assert_eq!(effort_hint(&breakdown(10, 0, 0), 1).0, Effort::Medium);
        // MANUAL share at 25%: large.
        assert_eq!(effort_hint(&breakdown(6, 0, 2), 0).0, Effort::Large);
        // Two native modules: large.
        assert_eq!(effort_hint(&breakdown(10, 0, 0), 2).0, Effort::Large);
        // The note always says it is a rough guide.
        assert!(effort_hint(&breakdown(10, 0, 0), 0)
            .1
            .contains("Rough guide"));
    }

    // ---- Predictions record -------------------------------------------------

    #[test]
    fn predictions_record_format_is_stable() {
        let fixture = fixture_dir("minimal-electron-app");
        let report = simulate_no_calibration(&fixture).expect("simulate");
        let record = build_record(&report, &fixture);
        assert_eq!(record.schema_version, 1);
        assert_eq!(record.model_version, reference::MODEL_VERSION);
        assert_eq!(record.app.name, report.app_name);
        assert_eq!(record.app.version, report.app_version);
        assert_eq!(record.app.path, fixture.display().to_string());
        assert_eq!(
            record.scan_summary.feasibility_score,
            report.feasibility_score
        );
        assert_eq!(record.scan_summary.auto, report.breakdown.auto);
        assert_eq!(record.scan_summary.assisted, report.breakdown.assisted);
        assert_eq!(record.scan_summary.manual, report.breakdown.manual);
        assert_eq!(record.predictions.len(), report.predictions.len());
        for (recorded, predicted) in record.predictions.iter().zip(report.predictions.iter()) {
            assert_eq!(recorded.metric, predicted.metric);
            assert_eq!(recorded.unit, predicted.unit);
            assert_eq!(recorded.confidence, predicted.confidence);
            assert_eq!(recorded.basis, predicted.basis);
            match predicted.after {
                Some(range) => {
                    assert_eq!(recorded.low, Some(range.low));
                    assert_eq!(recorded.point, Some(range.point));
                    assert_eq!(recorded.high, Some(range.high));
                }
                None => {
                    assert_eq!(recorded.low, None);
                    assert_eq!(recorded.point, None);
                    assert_eq!(recorded.high, None);
                }
            }
        }
        // generated_at is RFC 3339 UTC.
        let timestamp =
            Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$").expect("timestamp regex");
        assert!(
            timestamp.is_match(&record.generated_at),
            "generated_at must be RFC 3339 UTC, got {}",
            record.generated_at
        );
    }

    // ---- Timestamps ---------------------------------------------------------

    #[test]
    fn rfc3339_conversion_matches_known_dates() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(86_400), "1970-01-02T00:00:00Z");
        // Leap day 2000.
        assert_eq!(rfc3339_from_unix(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_from_unix(1_793_040_000), "2026-10-26T18:40:00Z");
    }

    // ---- Formatting ---------------------------------------------------------

    #[test]
    fn reports_contain_no_ansi_escapes() {
        for fixture in all_fixtures() {
            let report = simulate_no_calibration(&fixture).expect("simulate");
            assert!(
                !format_human_report(&report).contains('\x1b'),
                "human report must not contain ANSI escapes"
            );
            assert!(
                !format_markdown_report(&report).contains('\x1b'),
                "markdown report must not contain ANSI escapes"
            );
        }
    }

    #[test]
    fn number_formatting_trims_trailing_zeros() {
        assert_eq!(fmt_num(122.5), "122.5");
        assert_eq!(fmt_num(85.0), "85");
        assert_eq!(fmt_num(0.35), "0.35");
        assert_eq!(fmt_num(0.0), "0");
        assert_eq!(fmt_pct(90.9), "90.9");
        assert_eq!(fmt_pct(100.0), "100.0");
    }

    // ---- Reconciled model: bundle size -------------------------------------

    #[test]
    fn plugin_counts_and_bundle_points_match_measured_binaries() {
        // (fixture, expected plugin count, expected after point without
        // payload, measured release binary in MB)
        let cases = [
            ("minimal-electron-app", 2, 15.4, 12.826),
            ("full-apis-electron-app", 7, 25.4, 26.760),
            ("built-output-electron-app", 0, 11.4, 11.420),
        ];
        for (name, plugins, point, measured_mb) in cases {
            let fixture = fixture_dir(name);
            let scanned = scan::scan(&fixture).expect("scan");
            let (count, names) = count_plugins(&scanned);
            assert_eq!(
                count, plugins,
                "{name} must use {plugins} plugins; found {names:?}"
            );
            let report = simulate_no_calibration(&fixture).expect("simulate");
            let bundle = report
                .predictions
                .iter()
                .find(|p| p.metric == "bundle_size")
                .expect("bundle prediction");
            let after = bundle.after.expect("after range");
            // The point is the model value plus the fixture's small
            // payload (tens of KB at most).
            assert!(
                after.point >= point && after.point < point + 0.5,
                "{name}: after point must be ~{point} MB + payload, got {}",
                after.point
            );
            assert!(
                after.low <= measured_mb && measured_mb <= after.high,
                "{name}: the measured release binary ({measured_mb} MB) must fall inside the predicted after range {after:?}"
            );
        }
    }

    // ---- Reconciled model: per-platform memory -----------------------------

    #[test]
    fn memory_is_per_platform_and_linux_is_unknown() {
        let fixture = fixture_dir("minimal-electron-app");

        let macos = simulate_for_platform(&fixture, TargetPlatform::Macos).expect("simulate");
        let memory = macos
            .predictions
            .iter()
            .find(|p| p.metric == "memory_macos")
            .expect("macOS memory prediction");
        assert_eq!(memory.confidence, Confidence::Medium);
        let after = memory.after.expect("macOS memory range");
        assert_eq!((after.low, after.point, after.high), (0.4, 0.6, 0.7));

        let windows = simulate_for_platform(&fixture, TargetPlatform::Windows).expect("simulate");
        let memory = windows
            .predictions
            .iter()
            .find(|p| p.metric == "memory_windows")
            .expect("Windows memory prediction");
        assert_eq!(memory.confidence, Confidence::Low);
        let after = memory.after.expect("Windows memory range");
        assert_eq!((after.low, after.point, after.high), (0.9, 1.0, 1.1));

        let linux = simulate_for_platform(&fixture, TargetPlatform::Linux).expect("simulate");
        let memory = linux
            .predictions
            .iter()
            .find(|p| p.metric == "memory_linux")
            .expect("Linux memory prediction");
        assert_eq!(memory.confidence, Confidence::Unknown);
        assert_eq!(memory.after, None, "Linux memory must carry no numbers");
        assert!(memory.basis.contains("unknown"));
    }

    // ---- Reconciled model: startup -----------------------------------------

    #[test]
    fn startup_never_predicts_an_improvement() {
        for fixture in all_fixtures() {
            let report = simulate_no_calibration(&fixture).expect("simulate");
            let startup = report
                .predictions
                .iter()
                .find(|p| p.metric == "startup_time")
                .expect("startup prediction");
            let after = startup.after.expect("startup range");
            assert_eq!(
                after.point,
                1.0,
                "startup point must be exactly 1.0 (no change) for {}",
                fixture.display()
            );
            assert!(
                after.low < 1.0 && after.high > 1.0,
                "startup interval must straddle 1.0, got {after:?}"
            );
            assert_eq!(startup.confidence, Confidence::Low);
            assert_eq!(startup.unit, "multiple of current startup time");
        }
    }

    // ---- Reconciled model: process count -----------------------------------

    #[test]
    fn process_count_is_per_platform_and_rises_on_windows() {
        // The built-output fixture has exactly one window.
        let fixture = fixture_dir("built-output-electron-app");
        let scanned = scan::scan(&fixture).expect("scan");
        assert_eq!(scanned.windows.len(), 1);

        let macos = simulate_for_platform(&fixture, TargetPlatform::Macos).expect("simulate");
        let procs = macos
            .predictions
            .iter()
            .find(|p| p.metric == "process_count_macos")
            .expect("macOS process prediction");
        assert_eq!(procs.before.expect("before").point, 4.0);
        assert_eq!(procs.after.expect("after").point, 4.0);
        assert_eq!(procs.confidence, Confidence::Medium);

        let windows = simulate_for_platform(&fixture, TargetPlatform::Windows).expect("simulate");
        let procs = windows
            .predictions
            .iter()
            .find(|p| p.metric == "process_count_windows")
            .expect("Windows process prediction");
        assert_eq!(procs.before.expect("before").point, 4.0);
        assert_eq!(
            procs.after.expect("after").point,
            7.0,
            "the process count must RISE on Windows (4 → 7)"
        );
        assert_eq!(procs.confidence, Confidence::High);

        let linux = simulate_for_platform(&fixture, TargetPlatform::Linux).expect("simulate");
        let procs = linux
            .predictions
            .iter()
            .find(|p| p.metric == "process_count_linux")
            .expect("Linux process prediction");
        assert_eq!(procs.after.expect("after").point, 4.0);
        assert_eq!(procs.confidence, Confidence::Low);
        assert!(
            procs.basis.contains("UNMEASURED"),
            "the Linux process basis must say it is unmeasured; got {}",
            procs.basis
        );
    }

    // ---- Target platform ---------------------------------------------------

    #[test]
    fn target_platform_parsing() {
        assert_eq!(TargetPlatform::parse("macos"), Some(TargetPlatform::Macos));
        assert_eq!(TargetPlatform::parse("MacOS"), Some(TargetPlatform::Macos));
        assert_eq!(
            TargetPlatform::parse("windows"),
            Some(TargetPlatform::Windows)
        );
        assert_eq!(TargetPlatform::parse("linux"), Some(TargetPlatform::Linux));
        assert_eq!(TargetPlatform::parse("freebsd"), None);
        assert_eq!(TargetPlatform::parse(""), None);
        // This VM is Linux.
        assert_eq!(TargetPlatform::host(), TargetPlatform::Linux);
    }

    // ---- Calibration adjustment through simulate ---------------------------

    #[test]
    fn calibration_log_widens_intervals_when_actuals_fall_outside() {
        use crate::model::CalibrationEntry;
        let dir = fresh_test_dir("sim-calibration");
        let log_path = dir.join("calibration.jsonl");
        // Three logged bundle_size comparisons, all outside their
        // predicted intervals → the model widens 1.5×.
        let mut log_text = String::new();
        for actual in [30.0, 5.0, 28.0] {
            let entry = CalibrationEntry {
                metric: "bundle_size".to_string(),
                model_version: reference::MODEL_VERSION.to_string(),
                low: 11.4,
                point: 15.4,
                high: 23.4,
                actual,
                inside: false,
                app: "test-app".to_string(),
                platform: "linux".to_string(),
                recorded_at: "2026-11-25T00:00:00Z".to_string(),
            };
            log_text.push_str(&serde_json::to_string(&entry).expect("serialize"));
            log_text.push('\n');
        }
        fs::write(&log_path, log_text).expect("write synthetic log");

        let fixture = fixture_dir("minimal-electron-app");
        let base = simulate_no_calibration(&fixture).expect("simulate");
        let base_bundle = base
            .predictions
            .iter()
            .find(|p| p.metric == "bundle_size")
            .expect("bundle prediction");
        let base_after = base_bundle.after.expect("base after range");

        let adjusted = simulate_with_options(
            &fixture,
            &SimulateOptions {
                target_platform: TargetPlatform::Linux,
                calibration: CalibrationInput::Path(log_path),
            },
        )
        .expect("simulate with calibration");
        let adj_bundle = adjusted
            .predictions
            .iter()
            .find(|p| p.metric == "bundle_size")
            .expect("bundle prediction");
        let adj_after = adj_bundle.after.expect("adjusted after range");
        assert_eq!(
            adj_after.point, base_after.point,
            "calibration moves the interval, never the point"
        );
        assert!(
            adj_after.low < base_after.low && adj_after.high > base_after.high,
            "3 outside actuals must widen the interval: base {base_after:?} vs adjusted {adj_after:?}"
        );
        assert!(
            adj_bundle
                .assumptions
                .iter()
                .any(|a| a.contains("calibration")),
            "an adjusted prediction must say so in its assumptions; got {:?}",
            adj_bundle.assumptions
        );
        // A missing log behaves exactly like no calibration.
        let missing = simulate_with_options(
            &fixture,
            &SimulateOptions {
                target_platform: TargetPlatform::Linux,
                calibration: CalibrationInput::Path(dir.join("does-not-exist.jsonl")),
            },
        )
        .expect("simulate with missing log");
        assert_eq!(
            serde_json::to_string(&missing).expect("serialize"),
            serde_json::to_string(&base).expect("serialize"),
            "a missing calibration log must not change the report"
        );
        fs::remove_dir_all(&dir).ok();
    }
}
