//! Calibration loop: `e2r calibrate` and the interval adjustment
//! `e2r simulate` reads back.
//!
//! The loop, in one paragraph: `e2r simulate --record` writes a
//! [`PredictionsRecord`] (predicted *after* ranges, one per metric).
//! After the migration, the developer measures the real app and writes
//! the actuals into a `measurements.json` file (see
//! [`Measurements`]). `e2r calibrate` compares the two — per-metric
//! error (predicted vs actual, absolute + % error, inside/outside the
//! predicted interval) — and appends one [`CalibrationEntry`] per
//! compared metric to a local JSONL log. `e2r simulate` reads that log
//! on the next run and adjusts its confidence intervals for metrics
//! with enough history (see [`interval_adjustments`]).
//!
//! No ML, no network, no telemetry: the log lives on the developer's
//! machine (default `~/.cache/e2r/calibration.jsonl`, overridable via
//! `--log` / `E2R_CALIBRATION_LOG`), the adjustment rule is a small
//! deterministic function, and a missing, empty, or corrupt log makes
//! simulation behave exactly as if no calibration existed (a stderr
//! warning, never a crash).
//!
//! What `calibrate` can and cannot compare: bundle size and process
//! count are absolute values, so a single measured number compares
//! directly. Memory and startup predictions are *ratios* (fraction of
//! current memory / multiple of current startup time), so they need a
//! before AND an after measurement — the ratio is computed as
//! after / before. A lone after-value cannot be compared against a
//! ratio prediction and is reported as such, not forced. CPU has no
//! prediction at all (unknown), so it is never compared.

use crate::model::{CalibrationEntry, Measurements, PredictionsRecord, Range};
use crate::simulate::rfc3339_now;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Environment variable overriding the calibration log location.
pub const CALIBRATION_LOG_ENV: &str = "E2R_CALIBRATION_LOG";

/// Minimum logged comparisons for a metric before its interval is
/// adjusted. Below this, the sample is too small to learn from.
pub const MIN_COMPARISONS_FOR_ADJUSTMENT: usize = 3;

/// Interval multiplier when actuals repeatedly fall OUTSIDE the
/// predicted interval (fewer than half inside): widen by 50%.
pub const WIDEN_MULTIPLIER: f64 = 1.5;

/// Interval multiplier when actuals consistently fall INSIDE the
/// predicted interval (all inside): narrow by 20%, floored by
/// [`NARROW_FLOOR_FRACTION`].
pub const NARROW_MULTIPLIER: f64 = 0.8;

/// Floor on a narrowed interval: each half-width never goes below this
/// fraction of the point value (so a well-calibrated metric keeps a
/// ±10% interval at minimum — the model never claims more precision
/// than that from calibration alone).
pub const NARROW_FLOOR_FRACTION: f64 = 0.10;

/// Default predictions-record location, searched when `e2r calibrate`
/// is run without `--record`: `<app-dir>/.e2r/predictions.json`.
/// Read-only lookup — calibrate never writes into the app directory.
pub fn default_record_path(app_dir: &Path) -> PathBuf {
    app_dir.join(".e2r").join("predictions.json")
}

/// Resolve the calibration log path: an explicit `--log` path wins,
/// then the `E2R_CALIBRATION_LOG` environment variable, then the
/// default `~/.cache/e2r/calibration.jsonl`. When `HOME` is unset the
/// default falls back to `.e2r/calibration.jsonl` under the current
/// directory.
pub fn calibration_log_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Ok(path) = std::env::var(CALIBRATION_LOG_ENV) {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => PathBuf::from(home)
            .join(".cache")
            .join("e2r")
            .join("calibration.jsonl"),
        _ => PathBuf::from(".e2r").join("calibration.jsonl"),
    }
}

// --- Comparison ------------------------------------------------------------

/// One metric's comparison result. `predicted` is `None` when the model
/// declined to predict (unknown); `actual` is `None` when the
/// measurements carry no comparable actual; `abs_error`, `pct_error`,
/// and `inside` are `Some` only when both are present. `note` explains
/// any non-comparison in one line (empty when compared).
#[derive(Debug, Clone)]
pub struct MetricComparison {
    /// Metric id, as in the predictions record.
    pub metric: String,
    /// Unit of the prediction (and of the actual).
    pub unit: String,
    /// The predicted after-range, when the model predicted one.
    pub predicted: Option<Range>,
    /// The measured actual, when the measurements carry one.
    pub actual: Option<f64>,
    /// `actual - point`, when compared.
    pub abs_error: Option<f64>,
    /// `(actual - point) / point * 100`, when compared.
    pub pct_error: Option<f64>,
    /// Whether the actual fell inside the predicted interval, when
    /// compared (`low <= actual <= high`).
    pub inside: Option<bool>,
    /// Why this metric was not compared (empty when compared).
    pub note: String,
}

/// The full result of one `e2r calibrate` run.
#[derive(Debug, Clone)]
pub struct CalibrationReport {
    /// App name from the predictions record.
    pub app_name: String,
    /// Path the predictions record was read from.
    pub record_path: PathBuf,
    /// Path the measurements were read from.
    pub actuals_path: PathBuf,
    /// Model version that produced the predictions.
    pub model_version: String,
    /// Platform the measurements were taken on.
    pub platform: String,
    /// Warnings (platform mismatch, app-name mismatch, ...).
    pub warnings: Vec<String>,
    /// Per-metric comparison results, in record order.
    pub comparisons: Vec<MetricComparison>,
    /// Path the log entries were appended to.
    pub log_path: PathBuf,
    /// How many entries were appended to the log.
    pub entries_written: usize,
}

/// The platform a platform-suffixed metric id names
/// (`memory_macos` → `"macos"`), or `None` for platform-independent
/// metrics (`bundle_size`, `startup_time`, `cpu`).
fn metric_platform(metric: &str) -> Option<&str> {
    metric
        .strip_prefix("memory_")
        .or_else(|| metric.strip_prefix("process_count_"))
}

/// The measured actual for one predicted metric, in the prediction's
/// unit — or a note explaining why there is no comparable actual.
///
/// Memory and startup predictions are ratios, so they need a before
/// AND an after measurement; a lone after-value is not comparable.
fn actual_for_metric(metric: &str, measurements: &Measurements) -> Result<Option<f64>, String> {
    let m = &measurements.metrics;
    match metric {
        "bundle_size" => Ok(m.bundle_size_mb),
        "process_count_macos" | "process_count_windows" | "process_count_linux" => {
            Ok(m.process_count)
        }
        "memory_macos" | "memory_windows" | "memory_linux" => {
            match (m.memory_before_mb, m.memory_after_mb) {
                (Some(before), Some(after)) => {
                    if before <= 0.0 {
                        Err("memory_before_mb must be > 0 to compute a ratio".to_string())
                    } else {
                        Ok(Some(after / before))
                    }
                }
                _ => Err(
                    "no comparable actual — memory is predicted as a ratio, so measurements need both memory_before_mb and memory_after_mb"
                        .to_string(),
                ),
            }
        }
        "startup_time" => match (m.startup_before_ms, m.startup_after_ms) {
            (Some(before), Some(after)) => {
                if before <= 0.0 {
                    Err("startup_before_ms must be > 0 to compute a ratio".to_string())
                } else {
                    Ok(Some(after / before))
                }
            }
            _ => Err(
                "no comparable actual — startup is predicted as a ratio, so measurements need both startup_before_ms and startup_after_ms"
                    .to_string(),
            ),
        },
        _ => Err(format!(
            "no actual mapping for metric {metric:?} — the measurements schema has no field for it"
        )),
    }
}

/// Compare one recorded prediction against the measurements.
fn compare_prediction(
    predicted: &crate::model::RecordPrediction,
    measurements: &Measurements,
) -> MetricComparison {
    let range = match (predicted.low, predicted.point, predicted.high) {
        (Some(low), Some(point), Some(high)) => Some(Range { low, point, high }),
        _ => None,
    };
    let Some(range) = range else {
        return MetricComparison {
            metric: predicted.metric.clone(),
            unit: predicted.unit.clone(),
            predicted: None,
            actual: None,
            abs_error: None,
            pct_error: None,
            inside: None,
            note: "not predicted — the model declines to predict this metric (unknown)".to_string(),
        };
    };
    match actual_for_metric(&predicted.metric, measurements) {
        Ok(Some(actual)) => {
            let abs_error = actual - range.point;
            let pct_error = if range.point != 0.0 {
                Some(abs_error / range.point * 100.0)
            } else {
                None
            };
            MetricComparison {
                metric: predicted.metric.clone(),
                unit: predicted.unit.clone(),
                predicted: Some(range),
                actual: Some(actual),
                abs_error: Some(abs_error),
                pct_error,
                inside: Some(actual >= range.low && actual <= range.high),
                note: String::new(),
            }
        }
        Ok(None) => MetricComparison {
            metric: predicted.metric.clone(),
            unit: predicted.unit.clone(),
            predicted: Some(range),
            actual: None,
            abs_error: None,
            pct_error: None,
            inside: None,
            note: "no actual measurement for this metric in measurements.json".to_string(),
        },
        Err(note) => MetricComparison {
            metric: predicted.metric.clone(),
            unit: predicted.unit.clone(),
            predicted: Some(range),
            actual: None,
            abs_error: None,
            pct_error: None,
            inside: None,
            note,
        },
    }
}

/// Run the calibration: read the predictions record and the
/// measurements, compare per metric, append one log entry per
/// compared metric, and return the report.
///
/// `record_path` defaults to `<app-dir>/.e2r/predictions.json` when
/// `None`; when the record is missing the error tells the user to run
/// `e2r simulate --record` first.
pub fn calibrate(
    app_dir: &Path,
    record_path: Option<&Path>,
    actuals_path: &Path,
    log_path: Option<&Path>,
) -> Result<CalibrationReport> {
    let record_path = match record_path {
        Some(path) => path.to_path_buf(),
        None => default_record_path(app_dir),
    };
    if !record_path.is_file() {
        anyhow::bail!(
            "no predictions record found at {} — run `e2r simulate {} --record <path>` first, then pass the record with --record",
            record_path.display(),
            app_dir.display()
        );
    }
    let record_text = fs::read_to_string(&record_path)
        .with_context(|| format!("failed to read {}", record_path.display()))?;
    let record: PredictionsRecord = serde_json::from_str(&record_text).with_context(|| {
        format!(
            "failed to parse {} as a predictions record",
            record_path.display()
        )
    })?;
    if record.schema_version != 1 {
        anyhow::bail!(
            "unsupported predictions record schema_version {} (expected 1) in {}",
            record.schema_version,
            record_path.display()
        );
    }

    let actuals_text = fs::read_to_string(actuals_path)
        .with_context(|| format!("failed to read {}", actuals_path.display()))?;
    let measurements: Measurements = serde_json::from_str(&actuals_text)
        .with_context(|| format!("failed to parse {} as measurements", actuals_path.display()))?;
    if measurements.schema_version != 1 {
        anyhow::bail!(
            "unsupported measurements schema_version {} (expected 1) in {}",
            measurements.schema_version,
            actuals_path.display()
        );
    }

    let mut warnings = Vec::new();
    if measurements.app.name != record.app.name {
        warnings.push(format!(
            "app name mismatch: measurements are for {:?} but the record is for {:?} — comparing anyway",
            measurements.app.name, record.app.name
        ));
    }
    // Memory and process-count predictions are per-platform: flag a
    // measurements file taken on a different platform than the record.
    let record_platform = record
        .predictions
        .iter()
        .find_map(|p| metric_platform(&p.metric));
    if let Some(platform) = record_platform {
        if platform != measurements.platform {
            warnings.push(format!(
                "platform mismatch: predictions are for {platform} but measurements were taken on {} — memory and process-count comparisons mix platforms",
                measurements.platform
            ));
        }
    }

    let comparisons: Vec<MetricComparison> = record
        .predictions
        .iter()
        .map(|p| compare_prediction(p, &measurements))
        .collect();

    let now = rfc3339_now();
    let entries: Vec<CalibrationEntry> = comparisons
        .iter()
        .filter_map(|c| match (c.predicted, c.actual, c.inside) {
            (Some(range), Some(actual), Some(inside)) => Some(CalibrationEntry {
                metric: c.metric.clone(),
                model_version: record.model_version.clone(),
                low: range.low,
                point: range.point,
                high: range.high,
                actual,
                inside,
                app: record.app.name.clone(),
                platform: measurements.platform.clone(),
                recorded_at: now.clone(),
            }),
            _ => None,
        })
        .collect();

    let log_path = calibration_log_path(log_path);
    let entries_written = append_entries(&log_path, &entries)?;

    Ok(CalibrationReport {
        app_name: record.app.name.clone(),
        record_path,
        actuals_path: actuals_path.to_path_buf(),
        model_version: record.model_version.clone(),
        platform: measurements.platform.clone(),
        warnings,
        comparisons,
        log_path,
        entries_written,
    })
}

/// Append entries to the calibration log (JSONL, one object per line),
/// creating parent directories as needed. Returns the number of entries
/// written. An empty slice writes nothing and creates nothing.
pub fn append_entries(path: &Path, entries: &[CalibrationEntry]) -> Result<usize> {
    if entries.is_empty() {
        return Ok(0);
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    for entry in entries {
        let line = serde_json::to_string(entry).context("failed to serialize log entry")?;
        writeln!(file, "{line}").with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(entries.len())
}

/// Read the calibration log. A missing file is normal (no calibration
/// yet) and yields an empty vec, silently. A corrupt line is skipped
/// with a stderr warning — a damaged log must never break simulation.
/// Any other read failure also warns and yields an empty vec.
pub fn read_log(path: &Path) -> Vec<CalibrationEntry> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            eprintln!(
                "warning: cannot read calibration log {} ({err}) — simulating without calibration",
                path.display()
            );
            return Vec::new();
        }
    };
    let mut entries = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<CalibrationEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(err) => {
                eprintln!(
                    "warning: skipping corrupt line {} of calibration log {} ({err})",
                    idx + 1,
                    path.display()
                );
            }
        }
    }
    entries
}

// --- Interval adjustment ---------------------------------------------------

/// The adjustment for one metric's confidence interval, derived from
/// its logged comparisons.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IntervalAdjustment {
    /// Multiplier applied to each half-width around the point.
    pub multiplier: f64,
    /// Logged comparisons behind this adjustment.
    pub comparisons: usize,
    /// Of those, how many actuals fell inside their predicted interval.
    pub inside: usize,
}

/// Derive per-metric interval adjustments from logged comparisons.
///
/// The rule, per metric with at least
/// [`MIN_COMPARISONS_FOR_ADJUSTMENT`] logged comparisons:
///
/// * fewer than half of the actuals inside their predicted intervals
///   → the model is overconfident: **widen** (`WIDEN_MULTIPLIER`, 1.5×
///   each half-width);
/// * all actuals inside → the model is conservative: **narrow**
///   (`NARROW_MULTIPLIER`, 0.8× each half-width, floored by
///   [`adjust_range`]);
/// * otherwise (majority inside, but not all) → no adjustment.
///
/// Metrics with fewer than 3 comparisons, and metrics whose rule gives
/// no change, are absent from the returned map. Deterministic: same
/// log → same adjustments. `inside` is recomputed from the stored
/// low/high/actual rather than trusting the stored flag.
pub fn interval_adjustments(entries: &[CalibrationEntry]) -> BTreeMap<String, IntervalAdjustment> {
    let mut by_metric: BTreeMap<&str, Vec<&CalibrationEntry>> = BTreeMap::new();
    for entry in entries {
        by_metric
            .entry(entry.metric.as_str())
            .or_default()
            .push(entry);
    }
    let mut adjustments = BTreeMap::new();
    for (metric, comparisons) in by_metric {
        if comparisons.len() < MIN_COMPARISONS_FOR_ADJUSTMENT {
            continue;
        }
        let inside = comparisons
            .iter()
            .filter(|e| e.actual >= e.low && e.actual <= e.high)
            .count();
        let multiplier = if inside * 2 < comparisons.len() {
            WIDEN_MULTIPLIER
        } else if inside == comparisons.len() {
            NARROW_MULTIPLIER
        } else {
            continue;
        };
        adjustments.insert(
            metric.to_string(),
            IntervalAdjustment {
                multiplier,
                comparisons: comparisons.len(),
                inside,
            },
        );
    }
    adjustments
}

/// Round to 3 decimal places (the precision predictions are reported at).
fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Apply an interval adjustment to a predicted range: scale each
/// half-width around the point by the multiplier, then enforce the
/// narrow floor (each half-width at least [`NARROW_FLOOR_FRACTION`] of
/// the point, so a well-calibrated metric keeps a ±10% interval at
/// minimum). The point never moves — calibration adjusts confidence,
/// not the central estimate.
///
/// If rounding ever collapsed the ordering (it should not — the floor
/// keeps the half-widths well above the rounding granularity), the
/// original range is returned unchanged rather than panicking.
pub fn adjust_range(range: &Range, adjustment: &IntervalAdjustment) -> Range {
    let floor = (range.point.abs() * NARROW_FLOOR_FRACTION).max(0.001);
    let low =
        (range.point - (range.point - range.low) * adjustment.multiplier).min(range.point - floor);
    let high =
        (range.point + (range.high - range.point) * adjustment.multiplier).max(range.point + floor);
    let (low, point, high) = (round3(low), round3(range.point), round3(high));
    if low < point && point < high {
        Range { low, point, high }
    } else {
        *range
    }
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

/// Format a signed number with an explicit sign: `+2.5`, `-0.4`.
fn fmt_signed(value: f64) -> String {
    if value >= 0.0 {
        format!("+{}", fmt_num(value))
    } else {
        fmt_num(value)
    }
}

/// Render a [`CalibrationReport`] as a human-readable summary for the
/// terminal. Plain text, no ANSI escapes.
pub fn format_human_report(report: &CalibrationReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("Calibration: {}\n", report.app_name));
    out.push_str(&format!(
        "  Predictions record: {} (model {})\n",
        report.record_path.display(),
        report.model_version
    ));
    out.push_str(&format!(
        "  Measurements: {} (platform {})\n",
        report.actuals_path.display(),
        report.platform
    ));
    for warning in &report.warnings {
        out.push_str(&format!("  Warning: {warning}\n"));
    }
    out.push('\n');
    let mut compared = 0usize;
    let mut inside_count = 0usize;
    for comparison in &report.comparisons {
        let name = comparison.metric.replace('_', " ");
        match (comparison.predicted, comparison.actual) {
            (Some(range), Some(actual)) => {
                compared += 1;
                let inside = comparison.inside.unwrap_or(false);
                if inside {
                    inside_count += 1;
                }
                let abs = comparison.abs_error.unwrap_or(0.0);
                let pct = comparison
                    .pct_error
                    .map(|p| format!(" ({}%)", fmt_signed(p)))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "  {name}: predicted {}–{} (point {}) {} — actual {} {} — error {}{} — {}\n",
                    fmt_num(range.low),
                    fmt_num(range.high),
                    fmt_num(range.point),
                    comparison.unit,
                    fmt_num(actual),
                    comparison.unit,
                    fmt_signed(abs),
                    pct,
                    if inside {
                        "inside interval"
                    } else {
                        "OUTSIDE interval"
                    }
                ));
            }
            _ => {
                out.push_str(&format!("  {name}: not compared — {}\n", comparison.note));
            }
        }
    }
    out.push_str(&format!(
        "\nCompared {compared} metric(s), {inside_count} inside their predicted interval.\n"
    ));
    if report.entries_written > 0 {
        out.push_str(&format!(
            "Appended {} entr{} to {}\n",
            report.entries_written,
            if report.entries_written == 1 {
                "y"
            } else {
                "ies"
            },
            report.log_path.display()
        ));
    } else {
        out.push_str("No entries appended to the calibration log (nothing was comparable).\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Confidence, MeasurementsApp, MeasurementsMetrics, RecordApp, RecordPrediction,
        RecordScanSummary, UiPreservation,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A fresh empty directory under the system temp dir, unique per call.
    /// Tests NEVER touch the real `~/.cache` — every log path here is
    /// under a temp dir.
    fn fresh_test_dir(tag: &str) -> PathBuf {
        let n = TEST_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("e2r-cal-test-{tag}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn empty_ui() -> UiPreservation {
        UiPreservation {
            estimated_pct: 100.0,
            mode: "ui-preserving (system webview)".to_string(),
            framework: "vanilla".to_string(),
            bundler: "none".to_string(),
            renderer_kind: "static".to_string(),
            factors: Vec::new(),
            touch_points: Vec::new(),
            note: String::new(),
        }
    }

    fn test_record() -> PredictionsRecord {
        PredictionsRecord {
            schema_version: 1,
            model_version: "0.1.0".to_string(),
            app: RecordApp {
                name: "test-app".to_string(),
                version: Some("1.0.0".to_string()),
                path: "/tmp/test-app".to_string(),
            },
            generated_at: "2026-11-25T00:00:00Z".to_string(),
            scan_summary: RecordScanSummary {
                feasibility_score: 90,
                auto: 10,
                assisted: 0,
                manual: 0,
            },
            ui_preservation: empty_ui(),
            predictions: vec![
                RecordPrediction {
                    metric: "bundle_size".to_string(),
                    low: Some(11.4),
                    point: Some(15.4),
                    high: Some(23.4),
                    unit: "MB".to_string(),
                    confidence: Confidence::High,
                    basis: "test".to_string(),
                },
                RecordPrediction {
                    metric: "memory_linux".to_string(),
                    low: None,
                    point: None,
                    high: None,
                    unit: "fraction of current memory".to_string(),
                    confidence: Confidence::Unknown,
                    basis: "unknown — no Linux memory measurement".to_string(),
                },
                RecordPrediction {
                    metric: "startup_time".to_string(),
                    low: Some(0.8),
                    point: Some(1.0),
                    high: Some(1.3),
                    unit: "multiple of current startup time".to_string(),
                    confidence: Confidence::Low,
                    basis: "test".to_string(),
                },
                RecordPrediction {
                    metric: "process_count_linux".to_string(),
                    low: Some(3.0),
                    point: Some(4.0),
                    high: Some(5.0),
                    unit: "processes".to_string(),
                    confidence: Confidence::Low,
                    basis: "test".to_string(),
                },
                RecordPrediction {
                    metric: "cpu".to_string(),
                    low: None,
                    point: None,
                    high: None,
                    unit: "percent CPU".to_string(),
                    confidence: Confidence::Unknown,
                    basis: "unknown".to_string(),
                },
            ],
        }
    }

    fn test_measurements() -> Measurements {
        Measurements {
            schema_version: 1,
            app: MeasurementsApp {
                name: "test-app".to_string(),
                version: Some("1.0.0".to_string()),
            },
            measured_at: "2026-11-25T05:00:00Z".to_string(),
            platform: "linux".to_string(),
            metrics: MeasurementsMetrics {
                bundle_size_mb: Some(14.2),
                memory_before_mb: Some(200.0),
                memory_after_mb: Some(120.0),
                startup_before_ms: Some(400.0),
                startup_after_ms: Some(520.0),
                process_count: Some(4.0),
            },
        }
    }

    fn write_json(dir: &Path, name: &str, value: &impl serde::Serialize) -> PathBuf {
        let path = dir.join(name);
        fs::write(
            &path,
            serde_json::to_string_pretty(value).expect("serialize"),
        )
        .expect("write test json");
        path
    }

    fn log_entry(metric: &str, low: f64, point: f64, high: f64, actual: f64) -> CalibrationEntry {
        CalibrationEntry {
            metric: metric.to_string(),
            model_version: "0.1.0".to_string(),
            low,
            point,
            high,
            actual,
            inside: actual >= low && actual <= high,
            app: "test-app".to_string(),
            platform: "linux".to_string(),
            recorded_at: "2026-11-25T00:00:00Z".to_string(),
        }
    }

    // ---- Comparison math --------------------------------------------------

    #[test]
    fn error_computation_and_inside_outside_classification() {
        let measurements = test_measurements();
        let record = test_record();

        // Bundle: predicted 11.4–23.4 (point 15.4), actual 14.2 — inside.
        let bundle = compare_prediction(&record.predictions[0], &measurements);
        assert_eq!(bundle.actual, Some(14.2));
        assert!((bundle.abs_error.unwrap() - (14.2 - 15.4)).abs() < 1e-9);
        assert!((bundle.pct_error.unwrap() - ((14.2 - 15.4) / 15.4 * 100.0)).abs() < 1e-9);
        assert_eq!(bundle.inside, Some(true));

        // Startup: ratio 520/400 = 1.3 — exactly ON the high bound, still inside.
        let startup = compare_prediction(&record.predictions[2], &measurements);
        assert!((startup.actual.unwrap() - 1.3).abs() < 1e-9);
        assert_eq!(startup.inside, Some(true));

        // Process count: actual 4 == point — zero error, inside.
        let procs = compare_prediction(&record.predictions[3], &measurements);
        assert_eq!(procs.abs_error, Some(0.0));
        assert_eq!(procs.pct_error, Some(0.0));
        assert_eq!(procs.inside, Some(true));

        // An actual above the high bound is outside.
        let mut high_measurements = test_measurements();
        high_measurements.metrics.bundle_size_mb = Some(30.0);
        let outside = compare_prediction(&record.predictions[0], &high_measurements);
        assert_eq!(outside.inside, Some(false));
        assert!(outside.abs_error.unwrap() > 0.0);

        // Unknown predictions (memory_linux, cpu) are never compared.
        let memory = compare_prediction(&record.predictions[1], &measurements);
        assert_eq!(memory.predicted, None);
        assert_eq!(memory.actual, None);
        assert!(memory.note.contains("not predicted"));
        let cpu = compare_prediction(&record.predictions[4], &measurements);
        assert_eq!(cpu.predicted, None);
        assert!(cpu.note.contains("not predicted"));
    }

    #[test]
    fn ratio_predictions_need_both_before_and_after() {
        let mut measurements = test_measurements();
        measurements.metrics.memory_after_mb = None;
        // memory_linux is unknown, so use a macOS record prediction instead.
        let macos_memory = RecordPrediction {
            metric: "memory_macos".to_string(),
            low: Some(0.4),
            point: Some(0.6),
            high: Some(0.7),
            unit: "fraction of current memory".to_string(),
            confidence: Confidence::Medium,
            basis: "test".to_string(),
        };
        let compared = compare_prediction(&macos_memory, &measurements);
        assert_eq!(compared.actual, None);
        assert!(compared
            .note
            .contains("both memory_before_mb and memory_after_mb"));

        // With both present, the ratio compares.
        measurements.metrics.memory_after_mb = Some(120.0);
        let compared = compare_prediction(&macos_memory, &measurements);
        assert!((compared.actual.unwrap() - 0.6).abs() < 1e-9);
        assert_eq!(compared.inside, Some(true));
    }

    #[test]
    fn metrics_missing_from_measurements_are_reported_not_compared() {
        let record = test_record();
        let mut measurements = test_measurements();
        measurements.metrics.bundle_size_mb = None;
        measurements.metrics.process_count = None;
        let bundle = compare_prediction(&record.predictions[0], &measurements);
        assert_eq!(bundle.actual, None);
        assert!(bundle.note.contains("no actual measurement"));
        let procs = compare_prediction(&record.predictions[3], &measurements);
        assert_eq!(procs.actual, None);
        assert!(procs.note.contains("no actual measurement"));
    }

    // ---- Full round-trip --------------------------------------------------

    #[test]
    fn calibrate_round_trip_appends_log_entries() {
        let dir = fresh_test_dir("round-trip");
        let record_path = write_json(&dir, "predictions.json", &test_record());
        let actuals_path = write_json(&dir, "measurements.json", &test_measurements());
        let log_path = dir.join("nested").join("calibration.jsonl");

        let report =
            calibrate(&dir, Some(&record_path), &actuals_path, Some(&log_path)).expect("calibrate");
        assert_eq!(report.app_name, "test-app");
        assert_eq!(report.comparisons.len(), 5);
        // bundle_size, startup_time, process_count_linux compared;
        // memory_linux and cpu unknown.
        assert_eq!(report.entries_written, 3);
        assert_eq!(report.log_path, log_path);

        let entries = read_log(&log_path);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].metric, "bundle_size");
        assert!((entries[0].actual - 14.2).abs() < 1e-9);
        assert!(entries[0].inside);
        // Appending again adds more lines (append-only).
        let report2 = calibrate(&dir, Some(&record_path), &actuals_path, Some(&log_path))
            .expect("calibrate again");
        assert_eq!(report2.entries_written, 3);
        assert_eq!(read_log(&log_path).len(), 6);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn calibrate_without_record_errors_clearly() {
        let dir = fresh_test_dir("no-record");
        let actuals_path = write_json(&dir, "measurements.json", &test_measurements());
        let log_path = dir.join("calibration.jsonl");
        let err = calibrate(&dir, None, &actuals_path, Some(&log_path)).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains(".e2r/predictions.json"),
            "error must name the default record location; got {message}"
        );
        assert!(
            message.contains("e2r simulate"),
            "error must tell the user to run simulate --record first; got {message}"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn calibrate_warns_on_platform_mismatch() {
        let dir = fresh_test_dir("platform-mismatch");
        let record_path = write_json(&dir, "predictions.json", &test_record());
        let mut measurements = test_measurements();
        measurements.platform = "windows".to_string();
        let actuals_path = write_json(&dir, "measurements.json", &measurements);
        let log_path = dir.join("calibration.jsonl");
        let report =
            calibrate(&dir, Some(&record_path), &actuals_path, Some(&log_path)).expect("calibrate");
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("platform mismatch")),
            "expected a platform-mismatch warning; got {:?}",
            report.warnings
        );
        fs::remove_dir_all(&dir).ok();
    }

    // ---- Log reading ------------------------------------------------------

    #[test]
    fn log_append_and_read_back_round_trip() {
        let dir = fresh_test_dir("log-io");
        let log_path = dir.join("calibration.jsonl");
        // Missing file reads as empty, silently.
        assert!(read_log(&log_path).is_empty());
        let entries = vec![
            log_entry("bundle_size", 11.4, 15.4, 23.4, 14.2),
            log_entry("startup_time", 0.8, 1.0, 1.3, 1.1),
        ];
        assert_eq!(append_entries(&log_path, &entries).expect("append"), 2);
        let read_back = read_log(&log_path);
        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[0].metric, "bundle_size");
        assert_eq!(read_back[1].metric, "startup_time");
        assert!(read_back[1].inside);
        // Appending nothing writes nothing.
        assert_eq!(append_entries(&log_path, &[]).expect("append empty"), 0);
        assert_eq!(read_log(&log_path).len(), 2);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_log_lines_are_skipped_not_fatal() {
        let dir = fresh_test_dir("log-corrupt");
        let log_path = dir.join("calibration.jsonl");
        let good = serde_json::to_string(&log_entry("bundle_size", 11.4, 15.4, 23.4, 14.2))
            .expect("serialize");
        fs::write(
            &log_path,
            format!("{good}\nthis is not json\n{{\"metric\": 42}}\n"),
        )
        .expect("write corrupt log");
        let entries = read_log(&log_path);
        assert_eq!(entries.len(), 1, "only the good line survives");
        assert_eq!(entries[0].metric, "bundle_size");
        fs::remove_dir_all(&dir).ok();
    }

    // ---- Interval adjustment ----------------------------------------------

    #[test]
    fn adjustment_needs_three_comparisons() {
        let entries = vec![
            log_entry("bundle_size", 11.4, 15.4, 23.4, 99.0),
            log_entry("bundle_size", 11.4, 15.4, 23.4, 99.0),
        ];
        assert!(
            interval_adjustments(&entries).is_empty(),
            "2 comparisons must not adjust anything"
        );
    }

    #[test]
    fn adjustment_widens_when_actuals_fall_outside() {
        // 3 comparisons, all outside → widen 1.5×.
        let entries = vec![
            log_entry("bundle_size", 11.4, 15.4, 23.4, 30.0),
            log_entry("bundle_size", 11.4, 15.4, 23.4, 5.0),
            log_entry("bundle_size", 11.4, 15.4, 23.4, 28.0),
        ];
        let adjustments = interval_adjustments(&entries);
        let adj = adjustments.get("bundle_size").expect("widen adjustment");
        assert_eq!(adj.multiplier, WIDEN_MULTIPLIER);
        assert_eq!(adj.comparisons, 3);
        assert_eq!(adj.inside, 0);

        // 1 of 3 inside is still fewer than half → widen.
        let entries = vec![
            log_entry("bundle_size", 11.4, 15.4, 23.4, 14.0),
            log_entry("bundle_size", 11.4, 15.4, 23.4, 30.0),
            log_entry("bundle_size", 11.4, 15.4, 23.4, 28.0),
        ];
        let adj = interval_adjustments(&entries)
            .get("bundle_size")
            .expect("widen adjustment")
            .to_owned();
        assert_eq!(adj.multiplier, WIDEN_MULTIPLIER);
        assert_eq!(adj.inside, 1);
    }

    #[test]
    fn adjustment_narrows_when_actuals_stay_inside() {
        // 3 comparisons, all inside → narrow 0.8×.
        let entries = vec![
            log_entry("bundle_size", 11.4, 15.4, 23.4, 14.0),
            log_entry("bundle_size", 11.4, 15.4, 23.4, 16.0),
            log_entry("bundle_size", 11.4, 15.4, 23.4, 15.0),
        ];
        let adj = interval_adjustments(&entries)
            .get("bundle_size")
            .expect("narrow adjustment")
            .to_owned();
        assert_eq!(adj.multiplier, NARROW_MULTIPLIER);
        assert_eq!(adj.inside, 3);

        // Majority inside but not all (2 of 3) → no adjustment.
        let entries = vec![
            log_entry("bundle_size", 11.4, 15.4, 23.4, 14.0),
            log_entry("bundle_size", 11.4, 15.4, 23.4, 16.0),
            log_entry("bundle_size", 11.4, 15.4, 23.4, 30.0),
        ];
        assert!(
            interval_adjustments(&entries).is_empty(),
            "2 of 3 inside must not adjust"
        );
    }

    #[test]
    fn adjustment_is_per_metric() {
        let entries = vec![
            log_entry("bundle_size", 11.4, 15.4, 23.4, 30.0),
            log_entry("bundle_size", 11.4, 15.4, 23.4, 31.0),
            log_entry("bundle_size", 11.4, 15.4, 23.4, 32.0),
            log_entry("startup_time", 0.8, 1.0, 1.3, 1.0),
            log_entry("startup_time", 0.8, 1.0, 1.3, 1.1),
            log_entry("startup_time", 0.8, 1.0, 1.3, 0.9),
        ];
        let adjustments = interval_adjustments(&entries);
        assert_eq!(adjustments.len(), 2);
        assert_eq!(
            adjustments["bundle_size"].multiplier, WIDEN_MULTIPLIER,
            "bundle_size widens (all outside)"
        );
        assert_eq!(
            adjustments["startup_time"].multiplier, NARROW_MULTIPLIER,
            "startup_time narrows (all inside)"
        );
    }

    #[test]
    fn adjust_range_scales_half_widths_and_keeps_the_point() {
        let range = Range::new(10.0, 15.0, 20.0);
        let widened = adjust_range(
            &range,
            &IntervalAdjustment {
                multiplier: 1.5,
                comparisons: 3,
                inside: 0,
            },
        );
        assert_eq!(widened.point, 15.0, "the point never moves");
        assert!((widened.low - 7.5).abs() < 1e-9);
        assert!((widened.high - 22.5).abs() < 1e-9);
        assert!(widened.is_valid());

        let narrowed = adjust_range(
            &range,
            &IntervalAdjustment {
                multiplier: 0.8,
                comparisons: 3,
                inside: 3,
            },
        );
        assert_eq!(narrowed.point, 15.0);
        assert!((narrowed.low - 11.0).abs() < 1e-9);
        assert!((narrowed.high - 19.0).abs() < 1e-9);
        assert!(narrowed.is_valid());
    }

    #[test]
    fn adjust_range_never_narrows_below_the_floor() {
        // A tight range narrowed hard: the floor is ±10% of the point.
        let range = Range::new(9.9, 10.0, 10.1);
        let narrowed = adjust_range(
            &range,
            &IntervalAdjustment {
                multiplier: 0.1,
                comparisons: 3,
                inside: 3,
            },
        );
        assert_eq!(narrowed.point, 10.0);
        assert!(
            (narrowed.low - 9.0).abs() < 1e-9,
            "low must stop at the ±10% floor, got {}",
            narrowed.low
        );
        assert!(
            (narrowed.high - 11.0).abs() < 1e-9,
            "high must stop at the ±10% floor, got {}",
            narrowed.high
        );
        assert!(narrowed.is_valid());
    }

    // ---- Report formatting ------------------------------------------------

    #[test]
    fn human_report_shows_errors_and_inside_outside() {
        let dir = fresh_test_dir("report");
        let record_path = write_json(&dir, "predictions.json", &test_record());
        let actuals_path = write_json(&dir, "measurements.json", &test_measurements());
        let log_path = dir.join("calibration.jsonl");
        let report =
            calibrate(&dir, Some(&record_path), &actuals_path, Some(&log_path)).expect("calibrate");
        let text = format_human_report(&report);
        assert!(text.contains("Calibration: test-app"));
        assert!(text.contains("bundle size"));
        assert!(text.contains("inside interval"));
        assert!(text.contains("not compared"));
        assert!(text.contains("Appended 3 entries"));
        assert!(!text.contains('\x1b'), "no ANSI escapes");
        fs::remove_dir_all(&dir).ok();
    }
}
