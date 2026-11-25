//! Shared data model for scan reports.
//!
//! Every type here is serde-serializable so a [`ScanReport`] can be dumped
//! verbatim as JSON (`e2r scan --json`) or consumed by other tools.

use serde::{Deserialize, Serialize};

/// How much of an Electron API usage the converter can handle on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Classification {
    /// Direct Tauri equivalent exists; conversion is mechanical.
    Auto,
    /// A Tauri equivalent exists but needs human decisions (a plugin,
    /// different semantics, or a small rewrite).
    Assisted,
    /// No direct Tauri equivalent; the feature needs a manual port.
    Manual,
}

/// One IPC channel between the main and renderer processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcChannel {
    /// Channel name, e.g. `"read-file"`.
    pub name: String,
    /// `"handle"` (`ipcMain.handle`), `"on"` (`ipcMain.on`),
    /// `"listen"` (`ipcRenderer.on`), `"send"` (`ipcRenderer.invoke` /
    /// `ipcRenderer.send`), or `"emit"` (`webContents.send`, main →
    /// renderer — those live in [`ScanReport::main_to_renderer`]).
    pub direction: String,
    /// Source file, relative to the scanned app directory.
    pub file: String,
    /// 1-based line number of the call.
    pub line: usize,
}

/// One Electron API usage found in the scanned source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Electron API name, e.g. `"ipcMain.handle"` or `"BrowserWindow"`.
    pub api: String,
    /// Coarse category, e.g. `"ipc"`, `"dialog"`, `"window"`.
    pub category: String,
    /// Source file, relative to the scanned app directory.
    pub file: String,
    /// 1-based line number of the first occurrence in this file.
    pub line: usize,
    /// How convertible this usage is (see [`Classification`]).
    pub classification: Classification,
    /// The Tauri target for this API, if the mapping table knows one.
    pub tauri_target: Option<String>,
    /// One honest line about what the conversion involves.
    pub note: String,
    /// How many occurrences of this API were found in this file.
    /// Findings still display once per (API, file), but the count is
    /// recorded so the report does not understate call sites.
    #[serde(default = "default_occurrences")]
    pub occurrences: usize,
}

fn default_occurrences() -> usize {
    1
}

/// One member of an object exposed via `contextBridge.exposeInMainWorld`,
/// classified by what its value is so the bridge can emit a typed wrapper
/// instead of a generic proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreloadMemberKind {
    /// A function (arrow function, `function` expression, or method).
    Function,
    /// A platform check: `process.platform === 'darwin'`.
    PlatformMac,
    /// A platform check: `process.platform === 'linux'`.
    PlatformLinux,
    /// A platform check: `process.platform === 'win32'`.
    PlatformWindows,
    /// Any other value (boolean, string, number, object). The bridge
    /// emits a `TODO` placeholder for it rather than guessing.
    Value,
}

/// One member of an exposed preload object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreloadMember {
    /// Member name, e.g. `"send"` or `"isMac"`.
    pub name: String,
    /// What kind of value the member holds.
    pub kind: PreloadMemberKind,
}

/// The shape of one object exposed via `contextBridge.exposeInMainWorld`,
/// recovered from the preload source (inline object literal, or a variable
/// the call references). Absent when the exposed value could not be
/// resolved statically — then the bridge emits a `TODO`, not a proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreloadShape {
    /// The exposed key, e.g. `"electron"`.
    pub key: String,
    /// The object's top-level members, in source order.
    pub members: Vec<PreloadMember>,
    /// Source file of the `exposeInMainWorld` call.
    pub file: String,
    /// 1-based line number of the `exposeInMainWorld` call.
    pub line: usize,
}

/// What the scanner learned about the app's preload scripts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreloadInfo {
    /// Preload script paths (files named `preload.*`, plus any path
    /// referenced from a `BrowserWindow` `webPreferences.preload`,
    /// resolved against the referencing file's directory and deduped).
    pub files: Vec<String>,
    /// Whether any file calls `contextBridge.exposeInMainWorld`.
    pub uses_context_bridge: bool,
    /// Keys exposed to the renderer via `contextBridge.exposeInMainWorld`.
    pub exposed_keys: Vec<String>,
    /// Recovered shapes of the exposed objects, when statically known.
    #[serde(default)]
    pub shapes: Vec<PreloadShape>,
}

/// One `new BrowserWindow({...})` call site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Window width in pixels. When `width_known` is `false` this is
    /// Electron's default (800) as a placeholder — the real width is
    /// dynamic and was not resolvable, so it must not be reported as
    /// fact or emitted as a confident config value.
    pub width: u32,
    /// Window height in pixels. See [`WindowInfo::width`].
    pub height: u32,
    /// `true` when the width came from an integer literal in the options
    /// or from resolved `electron-window-state` defaults.
    #[serde(default)]
    pub width_known: bool,
    /// `true` when the height came from an integer literal in the options
    /// or from resolved `electron-window-state` defaults.
    #[serde(default)]
    pub height_known: bool,
    /// `true` when the size was resolved from `electron-window-state`
    /// `defaultWidth`/`defaultHeight` rather than from literals.
    #[serde(default)]
    pub from_window_state: bool,
    /// Minimum width, when the options set a literal `minWidth`.
    #[serde(default)]
    pub min_width: Option<u32>,
    /// Minimum height, when the options set a literal `minHeight`.
    #[serde(default)]
    pub min_height: Option<u32>,
    /// Background color, when the options set a literal `backgroundColor`.
    #[serde(default)]
    pub background_color: Option<String>,
    /// App icon path, when the options set an `icon`, resolved to a path
    /// relative to the app directory (e.g. `"assets/icon.png"`).
    #[serde(default)]
    pub icon: Option<String>,
    /// Window title, if the options set one.
    pub title: Option<String>,
    /// Whether the window is resizable (Electron default `true`).
    pub resizable: bool,
    /// Whether the window has a native frame (Electron default `true`).
    pub frame: bool,
    /// Source file, relative to the scanned app directory.
    pub file: String,
    /// 1-based line number of the `new BrowserWindow` call.
    pub line: usize,
}

/// The app's menu, from the first `Menu.buildFromTemplate(...)` call found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MenuInfo {
    /// `true` when the template argument is a static array literal
    /// (`[...]`), which the converter can translate directly; `false`
    /// when it is a variable or expression, which needs human review.
    pub is_static_literal: bool,
    /// The literal template source text, when static (capped at ~4 KiB).
    pub template_source: Option<String>,
    /// Source file, relative to the scanned app directory.
    pub file: String,
    /// 1-based line number of the `Menu.buildFromTemplate` call.
    pub line: usize,
}

/// A dev-server URL the main process loads (e.g. from `loadURL`), found
/// by scanning main-process sources for `http://localhost:<port>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevUrlInfo {
    /// The URL, e.g. `"http://localhost:4000"`.
    pub url: String,
    /// Source file, relative to the scanned app directory.
    pub file: String,
    /// 1-based line number of the URL.
    pub line: usize,
}

/// The full result of scanning one Electron app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanReport {
    /// App name from `package.json` (falls back to the directory name).
    pub app_name: String,
    /// App version from `package.json`, if declared.
    #[serde(default)]
    pub app_version: Option<String>,
    /// Display name from `package.json` `productName` (or the
    /// electron-builder config), if declared.
    #[serde(default)]
    pub product_name: Option<String>,
    /// Bundle id from `package.json` `build.appId` (or the
    /// electron-builder config), if declared.
    #[serde(default)]
    pub app_id: Option<String>,
    /// Electron version from `dependencies`/`devDependencies`, if declared.
    pub electron_version: Option<String>,
    /// Main-process entry point from `package.json` (`"main"`,
    /// default `"main.js"`).
    pub main_entry: Option<String>,
    /// Number of source files actually read and scanned.
    pub files_scanned: usize,
    /// IPC channels found, in scan order.
    pub ipc_channels: Vec<IpcChannel>,
    /// Main → renderer channels (`webContents.send` call sites), in scan
    /// order. Kept apart from [`ScanReport::ipc_channels`] because they
    /// flow the other way and become `listen()` wrappers, not `emit()`.
    #[serde(default)]
    pub main_to_renderer: Vec<IpcChannel>,
    /// Electron API usages, deduplicated per (API, file).
    pub findings: Vec<Finding>,
    /// Preload script information.
    pub preload: PreloadInfo,
    /// True native-addon dependencies from `package.json` (compiled
    /// code: `node-pty`, `better-sqlite3`, ...). Pure-JS deps that need
    /// a port live in [`ScanReport::port_needing_deps`] instead.
    pub native_modules: Vec<String>,
    /// Pure-JavaScript dependencies that still need a port to a Tauri
    /// plugin (e.g. `electron-updater`, `electron-store`). They are
    /// NOT native modules and cost no feasibility points when a 1:1
    /// plugin equivalent exists.
    #[serde(default)]
    pub port_needing_deps: Vec<String>,
    /// Dev-server URL from the main process, when the main process loads
    /// one (see [`DevUrlInfo`]).
    #[serde(default)]
    pub dev_url: Option<DevUrlInfo>,
    /// Feasibility score, 0–100. Higher means a smoother conversion.
    pub feasibility_score: u8,
    /// Human-readable verdict derived from the score.
    pub verdict: String,
    /// Windows found via `new BrowserWindow({...})`.
    pub windows: Vec<WindowInfo>,
    /// Menu info from the first `Menu.buildFromTemplate` call, if any.
    pub menu: Option<MenuInfo>,
}

// ---------------------------------------------------------------------------
// Simulation report types (`e2r simulate`)
// ---------------------------------------------------------------------------

/// Confidence in a simulator prediction.
///
/// `Unknown` means the model has no basis for the metric at all — the
/// prediction carries no numbers (see [`Prediction::after`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Measured on real migrations, or structurally certain.
    High,
    /// Grounded in a cited model or the app's own measured data.
    Medium,
    /// Grounded in a single study, inconsistent evidence across
    /// studies, an unmeasured inference, or a model parameter with no
    /// measurement behind it.
    Low,
    /// No basis — the model declines to predict.
    Unknown,
}

/// A predicted value as a range: `low < point < high`, always.
///
/// The point is the central estimate; the low/high bounds are the
/// confidence interval the model is willing to defend. A prediction
/// whose range collapses to a single number is lying about its
/// precision — ranges are the honest unit here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Range {
    /// Low end of the predicted range.
    pub low: f64,
    /// Central (point) estimate.
    pub point: f64,
    /// High end of the predicted range.
    pub high: f64,
}

impl Range {
    /// Build a range. Panics when the bounds are not ordered — an
    /// invalid range is a model bug, not a value worth carrying.
    pub fn new(low: f64, point: f64, high: f64) -> Self {
        assert!(
            low < point && point < high,
            "range must satisfy low < point < high, got {low} < {point} < {high}"
        );
        Self { low, point, high }
    }

    /// Whether the range satisfies the ordering invariant.
    pub fn is_valid(&self) -> bool {
        self.low < self.point && self.point < self.high
    }
}

/// One performance prediction, before → after.
///
/// `before` is `None` when no baseline was measured and the model has no
/// defensible estimate for one; `after` is `None` only when
/// `confidence` is [`Confidence::Unknown`] — the model declines to
/// predict rather than invent a number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prediction {
    /// Metric id: `"bundle_size"`, `"memory"`, `"startup_time"`,
    /// `"process_count"`, or `"cpu"`.
    pub metric: String,
    /// The current (Electron) value, when known or model-estimated.
    pub before: Option<Range>,
    /// The predicted (Tauri) value. `None` for unknown predictions.
    pub after: Option<Range>,
    /// Unit of both ranges, e.g. `"MB"`, `"processes"`, or — for
    /// ratio predictions — `"fraction of current memory"`.
    pub unit: String,
    /// How much the model trusts this prediction.
    pub confidence: Confidence,
    /// Where the numbers came from, in one honest sentence.
    pub basis: String,
    /// What the prediction assumes (no measured baseline, comparable
    /// workload, ...). Never empty for a prediction with numbers.
    pub assumptions: Vec<String>,
}

/// One factor in the UI preservation estimate: what it is, how much of
/// the renderer it says carries over, and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiFactor {
    /// Factor name, e.g. `"Framework: react"`.
    pub name: String,
    /// Carry-over this factor alone implies, 0–100.
    pub carry_over_pct: f64,
    /// One line explaining the factor's value.
    pub detail: String,
}

/// One Electron-only call site in renderer sources: a place the UI does
/// not carry over unchanged, with its file and line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiTouchPoint {
    /// Source file, relative to the scanned app directory.
    pub file: String,
    /// 1-based line number of the call site.
    pub line: usize,
    /// What was found, e.g. `"window.require('electron')"`.
    pub api: String,
    /// One line on what the call site needs instead.
    pub detail: String,
}

/// The UI preservation estimate: how much of the renderer carries over
/// unchanged in UI-preserving (system webview) mode.
///
/// The estimate is multiplicative over the factors: each factor is a
/// carry-over percentage, and `estimated_pct` is their product (so a
/// renderer with no touch points, a known framework, and a known
/// bundler estimates 100%). A full-native UI rewrite is out of scope
/// for this estimate — see the `note`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiPreservation {
    /// Estimated percentage of the renderer that carries over unchanged.
    pub estimated_pct: f64,
    /// The mode this estimate applies to: `"ui-preserving (system webview)"`.
    pub mode: String,
    /// Detected framework: `"react"`, `"vue"`, `"svelte"`, `"angular"`,
    /// `"vanilla"`, or `"unknown"`.
    pub framework: String,
    /// Detected bundler: `"vite"`, `"webpack"`, `"electron-vite"`,
    /// `"next"`, `"other"`, or `"none"`.
    pub bundler: String,
    /// Renderer kind from the converter's detection: `"built-output"`,
    /// `"source"`, `"static"`, or `"none"`.
    pub renderer_kind: String,
    /// Per-factor breakdown (renderer, framework, bundler, touch points).
    pub factors: Vec<UiFactor>,
    /// Electron-API touch points in renderer sources, with file:line.
    pub touch_points: Vec<UiTouchPoint>,
    /// Scope note: full-native UI mode is out of scope for this estimate.
    pub note: String,
}

/// Migratable breakdown: API call sites by classification, weighted by
/// [`Finding::occurrences`]. Percentages are rounded to one decimal and
/// always sum to 100 (largest-remainder rounding), except when there are
/// no call sites at all — then all three are 0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breakdown {
    /// AUTO call sites.
    pub auto: usize,
    /// ASSISTED call sites.
    pub assisted: usize,
    /// MANUAL call sites.
    pub manual: usize,
    /// Total call sites (`auto + assisted + manual`).
    pub total: usize,
    /// AUTO share, percent.
    pub auto_pct: f64,
    /// ASSISTED share, percent.
    pub assisted_pct: f64,
    /// MANUAL share, percent.
    pub manual_pct: f64,
}

/// Rough effort hint for the migration, derived from the MANUAL share
/// and the native-module count. A guide, not a quote — see
/// [`SimulationReport::effort_note`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// No MANUAL call sites, no native modules, no ASSISTED items.
    Trivial,
    /// Only ASSISTED items to review.
    Small,
    /// Some MANUAL call sites or one native module.
    Medium,
    /// A large MANUAL share or several native modules.
    Large,
}

/// The full result of simulating a migration: the scan's feasibility
/// verdict plus the migratable breakdown, the UI preservation estimate,
/// performance predictions with confidence ranges, warnings, and an
/// effort hint.
///
/// The report is deterministic: same app → byte-identical JSON. It
/// contains no timestamps and no unordered collections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationReport {
    /// App name from the scan report.
    pub app_name: String,
    /// App version from the scan report, if declared.
    pub app_version: Option<String>,
    /// Feasibility score, 0–100, from the scan report.
    pub feasibility_score: u8,
    /// Human-readable verdict from the scan report.
    pub verdict: String,
    /// Migratable breakdown (counts + percentages).
    pub breakdown: Breakdown,
    /// UI preservation estimate.
    pub ui_preservation: UiPreservation,
    /// Performance predictions, in fixed metric order
    /// (bundle size, memory, startup time, process count, CPU).
    pub predictions: Vec<Prediction>,
    /// Warnings, sorted and deduplicated.
    pub warnings: Vec<String>,
    /// Rough effort hint.
    pub effort: Effort,
    /// What the effort hint is based on, and its limits.
    pub effort_note: String,
    /// Simulator model version that produced this report
    /// (see [`crate::reference::MODEL_VERSION`]).
    pub model_version: String,
}

/// The app identity block of a [`PredictionsRecord`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordApp {
    /// App name.
    pub name: String,
    /// App version, if declared.
    pub version: Option<String>,
    /// App directory, as passed on the command line.
    pub path: String,
}

/// The scan summary block of a [`PredictionsRecord`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordScanSummary {
    /// Feasibility score, 0–100.
    pub feasibility_score: u8,
    /// AUTO call sites (weighted by occurrences).
    pub auto: usize,
    /// ASSISTED call sites (weighted by occurrences).
    pub assisted: usize,
    /// MANUAL call sites (weighted by occurrences).
    pub manual: usize,
}

/// One metric's predicted *after* range in a [`PredictionsRecord`].
/// `low`/`point`/`high` are all `None` for unknown predictions (no
/// number was invented); otherwise all three are present and satisfy
/// low < point < high.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordPrediction {
    /// Metric id (same ids as [`Prediction::metric`]).
    pub metric: String,
    /// Low end of the predicted after-range, or `None` if unknown.
    pub low: Option<f64>,
    /// Central estimate of the predicted after-range, or `None` if unknown.
    pub point: Option<f64>,
    /// High end of the predicted after-range, or `None` if unknown.
    pub high: Option<f64>,
    /// Unit of the range (same units as [`Prediction::unit`]).
    pub unit: String,
    /// Confidence in this prediction.
    pub confidence: Confidence,
    /// Where the numbers came from, in one honest sentence.
    pub basis: String,
}

/// A predictions record, written by `e2r simulate --record <path>` for
/// the calibration loop (see `docs/IMPLEMENTATION.md` §9).
///
/// **Stable format.** The calibration worker reads this file; field
/// names, the `schema_version`, and the meaning of each field must not
/// change without bumping `schema_version` and updating the calibration
/// reader. Shape:
///
/// ```json
/// {
///   "schema_version": 1,
///   "model_version": "0.1.0",
///   "app": { "name": "...", "version": "1.0.0", "path": "..." },
///   "generated_at": "2026-11-25T04:00:00Z",
///   "scan_summary": { "feasibility_score": 100, "auto": 20, "assisted": 2, "manual": 0 },
///   "ui_preservation": { "...": "same shape as SimulationReport.ui_preservation" },
///   "predictions": [
///     { "metric": "bundle_size", "low": 11.4, "point": 15.4, "high": 23.4,
///       "unit": "MB", "confidence": "high", "basis": "..." }
///   ]
/// }
/// ```
///
/// `generated_at` is an RFC 3339 UTC timestamp (`...Z`); it is the only
/// non-deterministic field in the record, and it is deliberately absent
/// from [`SimulationReport`] so reports stay byte-identical across runs.
/// `predictions` carries the *after* ranges (the predictions the loop
/// calibrates against measured actuals); the before baselines live in
/// the full simulation report, not here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionsRecord {
    /// Record format version. Always `1` in this release.
    pub schema_version: u32,
    /// Simulator model version that produced the predictions.
    pub model_version: String,
    /// The app that was simulated.
    pub app: RecordApp,
    /// When the record was generated (RFC 3339 UTC).
    pub generated_at: String,
    /// Scan summary (feasibility + weighted breakdown counts).
    pub scan_summary: RecordScanSummary,
    /// UI preservation estimate (same shape as the report's).
    pub ui_preservation: UiPreservation,
    /// Per-metric predicted after-ranges.
    pub predictions: Vec<RecordPrediction>,
}

// ---------------------------------------------------------------------------
// Calibration types (`e2r calibrate`)
// ---------------------------------------------------------------------------

/// The app identity block of a [`Measurements`] file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementsApp {
    /// App name (should match the predictions record's app name).
    pub name: String,
    /// App version, if declared.
    #[serde(default)]
    pub version: Option<String>,
}

/// The measured metrics in a [`Measurements`] file. Every field is
/// optional: `e2r calibrate` compares only the metrics present in both
/// the predictions record and the measurements, and says so for the
/// rest.
///
/// Memory and startup predictions are *ratios* (fraction of current
/// memory / multiple of current startup time), so they need a before
/// AND an after measurement to compare: the ratio is computed as
/// after / before. A lone after-value cannot be compared against a
/// ratio prediction and is reported as such.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeasurementsMetrics {
    /// Measured Tauri release binary size in MB (decimal MB).
    #[serde(default)]
    pub bundle_size_mb: Option<f64>,
    /// Measured Electron-app idle memory in MB (the before baseline).
    #[serde(default)]
    pub memory_before_mb: Option<f64>,
    /// Measured Tauri-app idle memory in MB (the after value).
    #[serde(default)]
    pub memory_after_mb: Option<f64>,
    /// Measured Electron-app startup time in ms (the before baseline).
    #[serde(default)]
    pub startup_before_ms: Option<f64>,
    /// Measured Tauri-app startup time in ms (the after value).
    #[serde(default)]
    pub startup_after_ms: Option<f64>,
    /// Measured Tauri-app process count (one window, idle).
    #[serde(default)]
    pub process_count: Option<f64>,
}

/// Measured post-migration actuals, read by `e2r calibrate` from a
/// `measurements.json` file. Written by hand (or by a benchmark
/// harness) after the migration has been measured — this is the
/// "actual" half of the calibration loop; the "predicted" half is a
/// [`PredictionsRecord`] from `e2r simulate --record`.
///
/// **Stable format.** Field names, the `schema_version`, and the
/// meaning of each field must not change without bumping
/// `schema_version` and updating the calibration reader. Shape:
///
/// ```json
/// {
///   "schema_version": 1,
///   "app": { "name": "my-app", "version": "1.0.0" },
///   "measured_at": "2026-11-25T04:00:00Z",
///   "platform": "linux",
///   "metrics": {
///     "bundle_size_mb": 14.2,
///     "memory_before_mb": 210.0,
///     "memory_after_mb": 130.0,
///     "startup_before_ms": 420.0,
///     "startup_after_ms": 390.0,
///     "process_count": 4
///   }
/// }
/// ```
///
/// `platform` is `"macos"`, `"windows"`, or `"linux"` — the platform
/// the measurements were taken on. Memory and process-count
/// predictions are per-platform, so a measurements file whose platform
/// does not match the predictions record's platform produces a warning
/// (the comparison still runs; the mismatch is recorded in the log).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Measurements {
    /// Measurements format version. Always `1` in this release.
    pub schema_version: u32,
    /// The app that was measured.
    pub app: MeasurementsApp,
    /// When the measurements were taken (RFC 3339 UTC).
    pub measured_at: String,
    /// Platform the measurements were taken on:
    /// `"macos"`, `"windows"`, or `"linux"`.
    pub platform: String,
    /// The measured metrics (all optional).
    pub metrics: MeasurementsMetrics,
}

/// One line of the calibration log (`~/.cache/e2r/calibration.jsonl`,
/// overridable via `--log` / `E2R_CALIBRATION_LOG`): a single
/// predicted-vs-actual comparison for one metric, appended by
/// `e2r calibrate` and read back by `e2r simulate` to adjust
/// confidence intervals.
///
/// **Append-only, JSONL.** One JSON object per line; a corrupt line is
/// skipped (with a stderr warning) rather than failing the read, so a
/// damaged log never breaks simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationEntry {
    /// Metric id, as in [`RecordPrediction::metric`] (memory and
    /// process-count ids carry their platform, e.g. `"memory_macos"`).
    pub metric: String,
    /// Simulator model version that produced the prediction.
    pub model_version: String,
    /// Low end of the predicted after-range.
    pub low: f64,
    /// Central estimate of the predicted after-range.
    pub point: f64,
    /// High end of the predicted after-range.
    pub high: f64,
    /// The measured actual, in the same unit as the prediction.
    pub actual: f64,
    /// Whether the actual fell inside the predicted interval
    /// (`low <= actual <= high`).
    pub inside: bool,
    /// App name the comparison came from.
    pub app: String,
    /// Platform the actual was measured on.
    pub platform: String,
    /// When the comparison was recorded (RFC 3339 UTC).
    pub recorded_at: String,
}
