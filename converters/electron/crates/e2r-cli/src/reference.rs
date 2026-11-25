//! Model constants for the pre-migration simulator ([`crate::simulate`]).
//!
//! Every number the simulator's prediction model uses lives here as data:
//! value, unit, source citation (name + URL), and confidence. The model
//! logic in `simulate.rs` must read constants from this module only —
//! never hardcode a model number there.
//!
//! **Research-backed (model 0.1.0).** The performance constants below
//! were reconciled against the simulator research report (measured
//! e2r-generated Tauri binaries, a measured Electron 45.5.0 baseline,
//! and cited third-party benchmarks — summarized in `docs/RND.md`
//! §11). Each constant's citation names the source, what was measured,
//! and when; each note states the constant's limits.
//!
//! What is still unresearched is marked as a **model parameter** (the
//! [`MODEL_PARAMETER`] citation): the UI-preservation factors, the
//! effort-hint thresholds, and the measurement tolerances. Those have
//! no external measurement behind them — the calibration loop
//! ([`crate::calibrate`]) is how they get replaced by data. They are
//! honest about it: every one carries low confidence and says so in
//! its note.
//!
//! Honesty rule: a constant with no defensible source does not get a
//! plausible-looking value — the model declines to predict instead (see
//! the CPU prediction, which has no constant here at all, and the Linux
//! memory prediction, which has no constant because no published Linux
//! memory measurement exists).

use crate::model::Confidence;

/// Where a model constant came from: a human-readable source name and a
/// URL (the repo docs themselves for values taken from `docs/RND.md`,
/// the upstream docs for values taken from a vendor).
#[derive(Debug, Clone, Copy)]
pub struct Citation {
    /// Source name, e.g. `"RND.md §2.4 — Tauri 2 vs Electron bundle ranges"`.
    pub name: &'static str,
    /// Source URL.
    pub url: &'static str,
}

/// One model parameter: a single value with its citation and confidence.
#[derive(Debug, Clone, Copy)]
pub struct ModelConstant {
    /// The value.
    pub value: f64,
    /// What the value measures, e.g. `"MB"` or `"percentage points"`.
    pub unit: &'static str,
    /// Where the value came from.
    pub citation: Citation,
    /// How much the model trusts this value.
    pub confidence: Confidence,
    /// One line on what the value means and why it is (only) this good.
    pub note: &'static str,
}

/// A model parameter expressed as a range: low < point < high, always.
#[derive(Debug, Clone, Copy)]
pub struct ModelRange {
    /// Low end of the range.
    pub low: f64,
    /// Central (point) estimate.
    pub point: f64,
    /// High end of the range.
    pub high: f64,
    /// What the range measures, e.g. `"MB"`.
    pub unit: &'static str,
    /// Where the range came from.
    pub citation: Citation,
    /// How much the model trusts this range.
    pub confidence: Confidence,
    /// One line on what the range means and why it is (only) this good.
    pub note: &'static str,
}

/// The simulator model version, stamped on every report and predictions
/// record so the calibration loop can tell which model produced a
/// prediction. Bump it whenever a constant in this file changes.
pub const MODEL_VERSION: &str = "0.1.0";

/// Citation for values measured by this project: `cargo build
/// --release` of five e2r-generated Tauri 2.12.7 projects (Linux x86_64,
/// Rust 1.100.0) and a measured Electron 45.5.0 baseline, 2026-11-25.
/// Full method and numbers: `docs/RND.md` §11.
const E2R_MEASURED: Citation = Citation {
    name: "e2r project measurements — release builds of 5 e2r-generated Tauri 2.12.7 projects + Electron 45.5.0 baseline, 2026-11-25 (docs/RND.md §11)",
    url: "https://github.com/mr-vishwet/rust-made-easy/blob/main/docs/RND.md#11-simulator-research-and-calibration",
};

/// Citation for values that are e2r model parameters with no external
/// measurement behind them — chosen by the model authors, to be replaced
/// by calibration data. The URL points at the simulator design doc.
const MODEL_PARAMETER: Citation = Citation {
    name: "e2r model parameter — no external measurement (docs/IMPLEMENTATION.md §5)",
    url: "https://github.com/mr-vishwet/rust-made-easy/blob/main/docs/IMPLEMENTATION.md",
};

/// Dawloom's Windows benchmark: Tauri 2.12.3 (size-optimised profile)
/// vs Electron 44.4.5, same counter app, Windows 11. Installer 1.05 MB
/// vs 106.1 MB NSIS; on disk 3.00 vs 367.40 MB; idle private memory 142
/// vs 149 MB (Tauri median of 5, Electron N=1); processes 7 vs 4.
/// Measured 2026-10-13/14, published 2026-10-19.
const DAWLOOM: Citation = Citation {
    name: "Dawloom, \"Tauri vs Electron vs WPF benchmark\" (Oct 2026) — Windows 11, Tauri 2.12.3 vs Electron 44.4.5, same counter app",
    url: "https://dawloom.com/blog/tauri-vs-electron-vs-wpf-benchmark/",
};

/// Wolinski's macOS benchmark: Tauri 2.12.5 vs Electron 45.2.3, same
/// HTML page, MacBook Pro M1 Pro 32 GB, macOS 27.2. Memory = macOS
/// footprint, median of 10 launches; startup = median of 30 launches.
/// Bundle 7.3 MiB vs 268.7 MiB; idle 59.5 vs 86.0 MiB; startup a tie.
/// November 2026.
const WOLINSKI: Citation = Citation {
    name: "Wolinski, \"Tauri vs Electron: Memory, IPC and MCP, Measured\" (Nov 2026) — macOS M1 Pro, medians of 10/30 launches",
    url: "https://wolinski.com/blog/tauri-vs-electron-ipc-memory.html",
};

/// Skathi's Windows benchmark: same Vue app (1,000 list rows),
/// Windows 11, Ryzen 9 9950X. Electron 45.3.3 vs Tauri 2.12.6.
/// Median of 5. First frame 522 vs 388 ms (Electron faster); private
/// working set idle 95.2 vs 88.8 MiB (Tauri higher); processes 7 vs 4.
/// Measured 2026-11-22.
const SKATHI: Citation = Citation {
    name: "Skathi, \"Skathi vs Electron vs Tauri\" (2026-11-22) — Windows 11, median of 5, same Vue app",
    url: "https://skathi.land/comparison/",
};

/// Lleg's Windows benchmark: Electron 45.2.3 vs Tauri 2.12.5, same
/// static page (500 rows), Windows 11, median of 5 runs, 20 s after
/// launch. Private working set 103.3 vs 108 MB (Tauri +4.5%); Tauri
/// exe 5.5 MiB; electron.exe 238.3 MiB. November 2026.
const LLEG: Citation = Citation {
    name: "Lleg, \"Swapping Electron for Tauri Saved No Memory\" (Nov 2026) — Windows 11, median of 5, same static page",
    url: "https://lleg.dev/posts/electron-vs-tauri-empty-window-memory/",
};

// --- Bundle size -----------------------------------------------------------

/// Electron installer size in MB, typical app.
///
/// Dawloom measured a 106.1 MB NSIS installer for an Electron 44.4.5
/// counter app on Windows; our measured compressed-runtime floor is
/// 118.2 MiB (Linux) / 151.2 MiB (Windows) for Electron 45.5.0, and
/// WatchMe's real macOS app was 289 MB installed. Used as the *before*
/// estimate only when no installed Electron runtime is available to
/// measure — the range already includes typical app code.
pub const ELECTRON_INSTALLER_MB: ModelRange = ModelRange {
    low: 90.0,
    point: 106.0,
    high: 160.0,
    unit: "MB",
    citation: DAWLOOM,
    confidence: Confidence::High,
    note: "Dawloom measured 106.1 MB NSIS (Electron 44.4); our measured compressed runtime floor is 118.2 MiB Linux / 151.2 MiB Windows (Electron 45.5.0).",
};

/// Tauri release binary size in MB for a zero-plugin app, e2r's
/// default profile (the generated Cargo.toml has NO
/// `[profile.release]`, so no strip / LTO / `opt-level="s"`),
/// unstripped, trivial frontend.
///
/// Measured by us: the built-output fixture's generated project builds
/// to 11,419,952 B (11.42 MB) unstripped, 7,222,960 B stripped. The
/// point is that measurement; the low end rounds it down slightly;
/// the high end adds the unexplained ~6 MB anomaly — electro-dash-v2
/// (zero plugins, identical deps and versions) measured 17.10 MB, and
/// the entire 5.68 MB gap is unnamed `.rodata`, cause unknown
/// (RND.md §11). The simulator adds [`TAURI_PER_PLUGIN_MB`] per
/// plugin and the measured app payload on top (frontend assets embed
/// into the binary ~1:1).
pub const TAURI_BINARY_ZERO_PLUGIN_MB: ModelRange = ModelRange {
    low: 11.0,
    point: 11.4,
    high: 17.4,
    unit: "MB",
    citation: E2R_MEASURED,
    confidence: Confidence::High,
    note: "Measured 11.42 MB (built-output); high end = point + the unexplained ~6 MB anomaly (electro-dash-v2, 17.10 MB, identical deps).",
};

/// Per-plugin binary cost in MB, on top of
/// [`TAURI_BINARY_ZERO_PLUGIN_MB`].
///
/// Measured from three data points: minimal (dialog + store,
/// +1,405,912 B over the zero-plugin baseline = 0.67 MiB/plugin),
/// Simplenote (5 plugins, +10,182,464 B = 1.94 MiB/plugin), and
/// full-apis (7 plugins, +15,340,080 B = 2.19 MiB/plugin). The cost is
/// not evenly spread — light plugins (dialog, store, opener) cost
/// well under 1 MiB each while the updater pulls in an HTTP/TLS stack
/// — so the low end covers light plugin sets and the point is the
/// average. No per-plugin breakdown exists (RND.md §11).
pub const TAURI_PER_PLUGIN_MB: ModelRange = ModelRange {
    low: 0.5,
    point: 2.0,
    high: 2.5,
    unit: "MB per plugin",
    citation: E2R_MEASURED,
    confidence: Confidence::High,
    note: "Three data points: 0.67 (dialog+store), 1.94 (Simplenote), 2.19 MiB/plugin (full-apis); light plugins cost far less than the updater.",
};

/// Tauri installer size in MB **with the docs' size-optimised release
/// profile applied** (`strip` + `lto` + `opt-level=\"s\"` +
/// `panic=\"abort\"` + installer compression).
///
/// **MODEL ESTIMATE ONLY — never the predicted after size.** Cited
/// benchmarks with the optimised profile: Dawloom 1.05 MB NSIS (empty
/// app), lleg 5.5 MiB exe (plain release build), wolinski 7.3 MiB
/// macOS bundle (LTO), WatchMe 3.7 MB `.dmg` / 15 MB `.app` (real
/// migration). e2r currently emits NO `[profile.release]`, so what it
/// produces out of the box is the measured default-profile binary
/// ([`TAURI_BINARY_ZERO_PLUGIN_MB`] + plugins), ~2–5× larger than this
/// range. The simulator mentions this figure only as a labeled
/// "with the optimised profile" estimate, never as a measurement of
/// e2r output. The often-quoted "3–15 MB" range is third-party
/// (digitalapplied.com, techxcelerate.ntxm.org), NOT an official Tauri
/// docs claim — the docs claim only that a minimal app "can be less
/// than 600KB", a floor, not a typical outcome.
pub const TAURI_OPTIMISED_INSTALLER_MB: ModelRange = ModelRange {
    low: 1.0,
    point: 5.0,
    high: 15.0,
    unit: "MB",
    citation: DAWLOOM,
    confidence: Confidence::High,
    note: "MODEL ESTIMATE: cited optimised-profile benchmarks (Dawloom 1.05 MB, lleg 5.5 MiB, wolinski 7.3 MiB, WatchMe 3.7–15 MB). NOT what e2r produces out of the box.",
};

// --- Memory ----------------------------------------------------------------

/// Memory after migration on **macOS**, as a fraction of current memory
/// usage (idle, one window).
///
/// −40% point (−30% to −60%): wolinski measured 86.0 → 59.5 MiB
/// (N=10 medians, macOS footprint); Elanis via senific 364 → 96 MB;
/// Hopp 409 → 172 MB (6 windows, N=1). On macOS Tauri uses WKWebView
/// (WebKit), genuinely lighter than a bundled Chromium in these
/// measurements. Real-app / loaded memory is unmeasured beyond a
/// handful of anecdotes — the prediction is for idle, one window only.
pub const MEMORY_AFTER_RATIO_MACOS: ModelRange = ModelRange {
    low: 0.4,
    point: 0.6,
    high: 0.7,
    unit: "fraction of current memory",
    citation: WOLINSKI,
    confidence: Confidence::Medium,
    note: "macOS idle, one window: wolinski 86.0 → 59.5 MiB; Elanis 364 → 96 MB; Hopp 409 → 172 MB (N=1).",
};

/// Memory after migration on **Windows**, as a fraction of current
/// memory usage (idle, one window).
///
/// ≈0% (−10% to +10%): Dawloom 149 → 142 MB private (Electron N=1),
/// lleg 103.3 → 108 MB private working set (Tauri +4.5%, median of 5),
/// skathi 88.8 → 95.2 MiB private working set (Tauri +7%, median of
/// 5). Two of three studies measured Tauri HIGHER — the sign of the
/// delta is not even consistent, hence low confidence. On Windows
/// Tauri renders through WebView2 (Chromium), so the browser engine —
/// the bulk of the memory — remains.
///
/// There is deliberately NO Linux constant: no published Linux
/// (WebKitGTK) idle-memory measurement exists, so the model refuses
/// to predict Linux memory.
pub const MEMORY_AFTER_RATIO_WINDOWS: ModelRange = ModelRange {
    low: 0.9,
    point: 1.0,
    high: 1.1,
    unit: "fraction of current memory",
    citation: LLEG,
    confidence: Confidence::Low,
    note: "Windows idle, one window: Dawloom 149 → 142 MB, lleg 103.3 → 108 MB, skathi 88.8 → 95.2 MiB — sign inconsistent across studies.",
};

// --- Startup time ----------------------------------------------------------

/// Startup time after migration, as a multiple of current startup time
/// (1.0 = no change).
///
/// **No improvement is predicted.** Wolinski found a tie on macOS
/// (309/377 ms vs 321/326 ms to first frame, median of 30 launches);
/// skathi found Electron FASTER on Windows first frame (388 vs 522 ms,
/// median of 5); Elanis (153/576 ms Windows, 582/1,412 ms macOS) and
/// Better Stack (273 vs 311 ms, real app) also found Tauri slower.
/// The one source finding a large Tauri win (buildr.sh, 2.4 s → 0.42 s)
/// publishes no run counts or harness and contradicts the
/// methodologically strongest source. The interval 0.8–1.3× says "no
/// meaningful change, either way within ~30%"; the point is exactly
/// 1.0. A cold-start advantage from the smaller binary on slow disks
/// is plausible but unmeasured — the model does not include it.
pub const STARTUP_TIME_RATIO: ModelRange = ModelRange {
    low: 0.8,
    point: 1.0,
    high: 1.3,
    unit: "multiple of current startup time",
    citation: WOLINSKI,
    confidence: Confidence::Low,
    note: "No improvement: wolinski tie (macOS), skathi Electron faster (Windows first frame); sources disagree on the sign.",
};

// --- Process count ---------------------------------------------------------

/// Electron process count, one window: main + renderer + GPU +
/// network/utility. Measured 4 on Windows (Dawloom and skathi agree
/// exactly) and macOS (wolinski). Electron starts a separate renderer
/// process per `BrowserWindow` (Electron docs), so the count grows by
/// one per extra window.
pub const ELECTRON_PROCESSES_ONE_WINDOW: ModelRange = ModelRange {
    low: 3.0,
    point: 4.0,
    high: 5.0,
    unit: "processes",
    citation: DAWLOOM,
    confidence: Confidence::High,
    note: "Measured 4 on Windows (Dawloom, skathi) and macOS (wolinski); +1 renderer per extra window (Electron docs).",
};

/// Tauri process count, one window, **macOS**: the Rust app plus the
/// WebKit page, GPU, and networking processes. Measured 4 by wolinski
/// (a single rigorous source). Same count as Electron — no
/// process-count win on macOS.
pub const TAURI_PROCESSES_ONE_WINDOW_MACOS: ModelRange = ModelRange {
    low: 3.0,
    point: 4.0,
    high: 5.0,
    unit: "processes",
    citation: WOLINSKI,
    confidence: Confidence::Medium,
    note: "Measured 4 (wolinski, macOS) — same as Electron; no process-count win on macOS.",
};

/// Tauri process count, one window, **Windows**: Rust host + WebView2
/// browser + renderer + GPU + helpers. Measured 7 by Dawloom and
/// skathi, who agree exactly. **The count goes UP on Windows** (4 →
/// 7) — the simulator must never claim "fewer processes" as a
/// migration benefit.
pub const TAURI_PROCESSES_ONE_WINDOW_WINDOWS: ModelRange = ModelRange {
    low: 6.0,
    point: 7.0,
    high: 8.0,
    unit: "processes",
    citation: SKATHI,
    confidence: Confidence::High,
    note: "Measured 7 (Dawloom and skathi agree exactly) — the count RISES on Windows, 4 → 7.",
};

/// Tauri process count, one window, **Linux**: ~4 expected (host +
/// WebKitGTK web/network/GPU processes).
///
/// **Unmeasured inference, low confidence.** No published Linux
/// process count was found; the value is inferred from the WebKitGTK
/// process model, not measured. The prediction says so in its basis.
pub const TAURI_PROCESSES_ONE_WINDOW_LINUX: ModelRange = ModelRange {
    low: 3.0,
    point: 4.0,
    high: 5.0,
    unit: "processes",
    citation: Citation {
        name: "WebKitGTK process model (inferred — no published Linux process count found)",
        url: "https://webkitgtk.org/",
    },
    confidence: Confidence::Low,
    note: "UNMEASURED: inferred from the WebKitGTK process model (host + web/network/GPU); no published count.",
};

// --- Measurement tolerance -------------------------------------------------

/// Tolerance applied around a *measured* installed Electron runtime size
/// (`node_modules/electron/dist` on disk): the before-range is
/// measured × (1 ± this fraction). The runtime on disk is not the
/// installer size (compression, signing, and packaging change it), so a
/// measured baseline still gets a range — just a narrower one than the
/// cited 90–160 MB range.
pub const MEASURED_RUNTIME_SPREAD: ModelConstant = ModelConstant {
    value: 0.05,
    unit: "fraction",
    citation: MODEL_PARAMETER,
    confidence: Confidence::Low,
    note: "±5% around a measured runtime: disk size is not installer size.",
};

/// Minimum size (MB) for an installed Electron runtime to count as
/// measured. A real `electron/dist` is well over 100 MB unpacked
/// (measured: 273.7 MiB for Electron 45.5.0); below 50 MB the dir is a
/// stub or a partial install, and the model falls back to the cited
/// range instead of trusting it.
pub const MEASURED_RUNTIME_MIN_MB: ModelConstant = ModelConstant {
    value: 50.0,
    unit: "MB",
    citation: MODEL_PARAMETER,
    confidence: Confidence::Low,
    note: "Below 50 MB the dist dir is a stub, not an installed runtime.",
};

// --- UI preservation -------------------------------------------------------

/// Percentage points deducted from the UI carry-over estimate per
/// Electron-API touch point found in renderer sources (a `window.require`
/// of Electron, a direct `ipcRenderer` use, a Node builtin in renderer
/// code, ...). Each touch point is a call site that needs a bridge call
/// or a port instead of carrying over unchanged.
pub const UI_TOUCH_POINT_DEDUCTION_PCT: ModelConstant = ModelConstant {
    value: 3.0,
    unit: "percentage points",
    citation: MODEL_PARAMETER,
    confidence: Confidence::Low,
    note: "Per-touch-point cost; no measurement behind it — calibration will replace it.",
};

/// Maximum total deduction from touch points, so a renderer riddled with
/// Electron APIs bottoms out at 60% carry-over from this factor instead
/// of going negative or to zero on touch points alone.
pub const UI_TOUCH_POINT_MAX_DEDUCTION_PCT: ModelConstant = ModelConstant {
    value: 40.0,
    unit: "percentage points",
    citation: MODEL_PARAMETER,
    confidence: Confidence::Low,
    note: "Cap: touch points alone cannot take carry-over below 60%.",
};

/// Carry-over factor (%) when the renderer framework cannot be identified
/// from `package.json`. The renderer still carries over verbatim, but an
/// unknown framework means the carry-over is unverified, so the model
/// discounts it slightly.
pub const UI_UNKNOWN_FRAMEWORK_FACTOR_PCT: ModelConstant = ModelConstant {
    value: 90.0,
    unit: "percent",
    citation: MODEL_PARAMETER,
    confidence: Confidence::Low,
    note: "Unknown framework: carried over, but unverified — small discount.",
};

/// Carry-over factor (%) for an electron-vite app: the build script
/// targets Electron's triple and needs adapting to a plain `vite build`
/// (see the converter's renderer caveats), so the model discounts it.
pub const UI_ELECTRON_VITE_FACTOR_PCT: ModelConstant = ModelConstant {
    value: 90.0,
    unit: "percent",
    citation: MODEL_PARAMETER,
    confidence: Confidence::Low,
    note: "electron-vite build scripts need adapting for Tauri (IMPLEMENTATION.md §4).",
};

/// Carry-over factor (%) for a Next.js app with `output: 'export'`:
/// static export works in Tauri, but the export step itself is a change
/// from the app's current setup.
pub const UI_NEXT_EXPORT_FACTOR_PCT: ModelConstant = ModelConstant {
    value: 95.0,
    unit: "percent",
    citation: MODEL_PARAMETER,
    confidence: Confidence::Low,
    note: "Next.js static export works in Tauri; the export step is still a change.",
};

/// Carry-over factor (%) for a Next.js app *without* static export:
/// Tauri serves static files, so server-side rendering must be replaced
/// before the UI can run at all.
pub const UI_NEXT_NO_EXPORT_FACTOR_PCT: ModelConstant = ModelConstant {
    value: 80.0,
    unit: "percent",
    citation: MODEL_PARAMETER,
    confidence: Confidence::Low,
    note: "Next.js without static export needs SSR replaced (IMPLEMENTATION.md §4).",
};

/// Carry-over factor (%) for a bundler the model does not know
/// (Parcel, esbuild, rollup, ...): the build probably works, but it is
/// unverified, so the model discounts it slightly.
pub const UI_OTHER_BUNDLER_FACTOR_PCT: ModelConstant = ModelConstant {
    value: 90.0,
    unit: "percent",
    citation: MODEL_PARAMETER,
    confidence: Confidence::Low,
    note: "Unrecognized bundler: probably fine, unverified — small discount.",
};

// --- Effort hint -----------------------------------------------------------

/// MANUAL share (fraction of weighted call sites) at or above which the
/// effort hint is `large`.
pub const EFFORT_MANUAL_SHARE_LARGE: ModelConstant = ModelConstant {
    value: 0.25,
    unit: "fraction",
    citation: MODEL_PARAMETER,
    confidence: Confidence::Low,
    note: "A quarter or more of call sites needing manual ports is a large migration.",
};

/// Native-module count at or above which the effort hint is `large`.
pub const EFFORT_NATIVE_MODULES_LARGE: ModelConstant = ModelConstant {
    value: 2.0,
    unit: "modules",
    citation: MODEL_PARAMETER,
    confidence: Confidence::Low,
    note: "Two or more native modules to port is a large migration.",
};

// --- Warning lists ---------------------------------------------------------

/// Electron APIs with no system-webview equivalent (Chromium-only
/// surface): when the scan finds one, the simulator warns that it needs
/// a manual design. Sourced from the mapping table's MANUAL entries for
/// Chromium-specific surface (see `converters/electron/mappings/`).
pub const CHROMIUM_ONLY_APIS: &[&str] =
    &["session", "protocol", "crashReporter", "desktopCapturer"];

/// Markers searched (case-insensitively) in app sources for DRM usage.
/// Widevine is Chromium-only and has no Tauri equivalent.
pub const DRM_MARKERS: &[&str] = &["widevine"];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every range constant must satisfy low < point < high — the
    /// interval-sanity invariant the simulator's predictions inherit.
    #[test]
    fn model_ranges_are_ordered() {
        for (name, range) in [
            ("ELECTRON_INSTALLER_MB", ELECTRON_INSTALLER_MB),
            ("TAURI_BINARY_ZERO_PLUGIN_MB", TAURI_BINARY_ZERO_PLUGIN_MB),
            ("TAURI_PER_PLUGIN_MB", TAURI_PER_PLUGIN_MB),
            ("TAURI_OPTIMISED_INSTALLER_MB", TAURI_OPTIMISED_INSTALLER_MB),
            ("MEMORY_AFTER_RATIO_MACOS", MEMORY_AFTER_RATIO_MACOS),
            ("MEMORY_AFTER_RATIO_WINDOWS", MEMORY_AFTER_RATIO_WINDOWS),
            ("STARTUP_TIME_RATIO", STARTUP_TIME_RATIO),
            (
                "ELECTRON_PROCESSES_ONE_WINDOW",
                ELECTRON_PROCESSES_ONE_WINDOW,
            ),
            (
                "TAURI_PROCESSES_ONE_WINDOW_MACOS",
                TAURI_PROCESSES_ONE_WINDOW_MACOS,
            ),
            (
                "TAURI_PROCESSES_ONE_WINDOW_WINDOWS",
                TAURI_PROCESSES_ONE_WINDOW_WINDOWS,
            ),
            (
                "TAURI_PROCESSES_ONE_WINDOW_LINUX",
                TAURI_PROCESSES_ONE_WINDOW_LINUX,
            ),
        ] {
            assert!(
                range.low < range.point && range.point < range.high,
                "{name} must satisfy low < point < high, got {} < {} < {}",
                range.low,
                range.point,
                range.high
            );
            assert!(
                range.low.is_finite() && range.point.is_finite() && range.high.is_finite(),
                "{name} must be finite"
            );
        }
    }

    /// Every single-value constant must be finite and in its valid domain.
    #[test]
    fn model_constants_are_sane() {
        assert!((0.0..1.0).contains(&MEASURED_RUNTIME_SPREAD.value));
        const {
            assert!(MEASURED_RUNTIME_MIN_MB.value > 0.0);
        }
        assert!((0.0..100.0).contains(&UI_TOUCH_POINT_DEDUCTION_PCT.value));
        assert!((0.0..100.0).contains(&UI_TOUCH_POINT_MAX_DEDUCTION_PCT.value));
        for (name, factor) in [
            (
                "UI_UNKNOWN_FRAMEWORK_FACTOR_PCT",
                UI_UNKNOWN_FRAMEWORK_FACTOR_PCT,
            ),
            ("UI_ELECTRON_VITE_FACTOR_PCT", UI_ELECTRON_VITE_FACTOR_PCT),
            ("UI_NEXT_EXPORT_FACTOR_PCT", UI_NEXT_EXPORT_FACTOR_PCT),
            ("UI_NEXT_NO_EXPORT_FACTOR_PCT", UI_NEXT_NO_EXPORT_FACTOR_PCT),
            ("UI_OTHER_BUNDLER_FACTOR_PCT", UI_OTHER_BUNDLER_FACTOR_PCT),
        ] {
            assert!(
                (0.0..=100.0).contains(&factor.value),
                "{name} must be a percentage, got {}",
                factor.value
            );
        }
        assert!((0.0..1.0).contains(&EFFORT_MANUAL_SHARE_LARGE.value));
        const {
            assert!(EFFORT_NATIVE_MODULES_LARGE.value >= 1.0);
        }
    }

    /// Every constant carries a citation with a non-empty name and URL,
    /// and a confidence — the "show your work" rule for the model.
    #[test]
    fn model_constants_carry_citations() {
        let cited: Vec<(&str, Citation, Confidence)> = vec![
            (
                "ELECTRON_INSTALLER_MB",
                ELECTRON_INSTALLER_MB.citation,
                ELECTRON_INSTALLER_MB.confidence,
            ),
            (
                "TAURI_BINARY_ZERO_PLUGIN_MB",
                TAURI_BINARY_ZERO_PLUGIN_MB.citation,
                TAURI_BINARY_ZERO_PLUGIN_MB.confidence,
            ),
            (
                "TAURI_PER_PLUGIN_MB",
                TAURI_PER_PLUGIN_MB.citation,
                TAURI_PER_PLUGIN_MB.confidence,
            ),
            (
                "TAURI_OPTIMISED_INSTALLER_MB",
                TAURI_OPTIMISED_INSTALLER_MB.citation,
                TAURI_OPTIMISED_INSTALLER_MB.confidence,
            ),
            (
                "MEMORY_AFTER_RATIO_MACOS",
                MEMORY_AFTER_RATIO_MACOS.citation,
                MEMORY_AFTER_RATIO_MACOS.confidence,
            ),
            (
                "MEMORY_AFTER_RATIO_WINDOWS",
                MEMORY_AFTER_RATIO_WINDOWS.citation,
                MEMORY_AFTER_RATIO_WINDOWS.confidence,
            ),
            (
                "STARTUP_TIME_RATIO",
                STARTUP_TIME_RATIO.citation,
                STARTUP_TIME_RATIO.confidence,
            ),
            (
                "ELECTRON_PROCESSES_ONE_WINDOW",
                ELECTRON_PROCESSES_ONE_WINDOW.citation,
                ELECTRON_PROCESSES_ONE_WINDOW.confidence,
            ),
            (
                "TAURI_PROCESSES_ONE_WINDOW_MACOS",
                TAURI_PROCESSES_ONE_WINDOW_MACOS.citation,
                TAURI_PROCESSES_ONE_WINDOW_MACOS.confidence,
            ),
            (
                "TAURI_PROCESSES_ONE_WINDOW_WINDOWS",
                TAURI_PROCESSES_ONE_WINDOW_WINDOWS.citation,
                TAURI_PROCESSES_ONE_WINDOW_WINDOWS.confidence,
            ),
            (
                "TAURI_PROCESSES_ONE_WINDOW_LINUX",
                TAURI_PROCESSES_ONE_WINDOW_LINUX.citation,
                TAURI_PROCESSES_ONE_WINDOW_LINUX.confidence,
            ),
            (
                "MEASURED_RUNTIME_SPREAD",
                MEASURED_RUNTIME_SPREAD.citation,
                MEASURED_RUNTIME_SPREAD.confidence,
            ),
            (
                "MEASURED_RUNTIME_MIN_MB",
                MEASURED_RUNTIME_MIN_MB.citation,
                MEASURED_RUNTIME_MIN_MB.confidence,
            ),
            (
                "UI_TOUCH_POINT_DEDUCTION_PCT",
                UI_TOUCH_POINT_DEDUCTION_PCT.citation,
                UI_TOUCH_POINT_DEDUCTION_PCT.confidence,
            ),
            (
                "UI_TOUCH_POINT_MAX_DEDUCTION_PCT",
                UI_TOUCH_POINT_MAX_DEDUCTION_PCT.citation,
                UI_TOUCH_POINT_MAX_DEDUCTION_PCT.confidence,
            ),
            (
                "UI_UNKNOWN_FRAMEWORK_FACTOR_PCT",
                UI_UNKNOWN_FRAMEWORK_FACTOR_PCT.citation,
                UI_UNKNOWN_FRAMEWORK_FACTOR_PCT.confidence,
            ),
            (
                "UI_ELECTRON_VITE_FACTOR_PCT",
                UI_ELECTRON_VITE_FACTOR_PCT.citation,
                UI_ELECTRON_VITE_FACTOR_PCT.confidence,
            ),
            (
                "UI_NEXT_EXPORT_FACTOR_PCT",
                UI_NEXT_EXPORT_FACTOR_PCT.citation,
                UI_NEXT_EXPORT_FACTOR_PCT.confidence,
            ),
            (
                "UI_NEXT_NO_EXPORT_FACTOR_PCT",
                UI_NEXT_NO_EXPORT_FACTOR_PCT.citation,
                UI_NEXT_NO_EXPORT_FACTOR_PCT.confidence,
            ),
            (
                "UI_OTHER_BUNDLER_FACTOR_PCT",
                UI_OTHER_BUNDLER_FACTOR_PCT.citation,
                UI_OTHER_BUNDLER_FACTOR_PCT.confidence,
            ),
            (
                "EFFORT_MANUAL_SHARE_LARGE",
                EFFORT_MANUAL_SHARE_LARGE.citation,
                EFFORT_MANUAL_SHARE_LARGE.confidence,
            ),
            (
                "EFFORT_NATIVE_MODULES_LARGE",
                EFFORT_NATIVE_MODULES_LARGE.citation,
                EFFORT_NATIVE_MODULES_LARGE.confidence,
            ),
        ];
        for (name, citation, confidence) in cited {
            assert!(!citation.name.is_empty(), "{name} needs a citation name");
            assert!(
                citation.url.starts_with("https://"),
                "{name} needs a citation URL, got {:?}",
                citation.url
            );
            assert!(
                confidence != Confidence::Unknown,
                "{name} must state a real confidence, not unknown"
            );
        }
    }

    /// The model version is stamped on reports; it must be the
    /// research-backed `0.1.0` — the provisional era ended with the
    /// research reconciliation.
    #[test]
    fn model_version_is_research_backed() {
        assert_eq!(MODEL_VERSION, "0.1.0");
        assert!(
            !MODEL_VERSION.contains("provisional"),
            "model version must no longer say provisional: {MODEL_VERSION}"
        );
    }

    /// The zero-plugin range's high end absorbs the unexplained ~6 MB
    /// anomaly: electro-dash-v2 (17.10 MB) must fit inside it.
    #[test]
    fn zero_plugin_range_absorbs_the_anomaly() {
        let gap = TAURI_BINARY_ZERO_PLUGIN_MB.high - TAURI_BINARY_ZERO_PLUGIN_MB.point;
        assert!(
            (gap - 6.0).abs() < 1e-9,
            "high end must sit ~6 MB above the point (the anomaly), got a {gap} MB gap"
        );
        let (low, high) = (
            TAURI_BINARY_ZERO_PLUGIN_MB.low,
            TAURI_BINARY_ZERO_PLUGIN_MB.high,
        );
        assert!(
            low <= 11.42 && 17.10 <= high,
            "both measured 0-plugin binaries (11.42 and 17.10 MB) must fall inside the range"
        );
    }

    /// The startup model must never predict a speedup: the point is
    /// exactly 1.0 (no change) and the interval straddles it.
    #[test]
    fn startup_model_predicts_no_improvement() {
        assert_eq!(STARTUP_TIME_RATIO.point, 1.0);
        let (low, high) = (STARTUP_TIME_RATIO.low, STARTUP_TIME_RATIO.high);
        assert!(low < 1.0 && high > 1.0);
    }

    /// The Windows process model must show the count going UP: Tauri's
    /// one-window point exceeds Electron's.
    #[test]
    fn windows_process_count_goes_up() {
        let (tauri, electron) = (
            TAURI_PROCESSES_ONE_WINDOW_WINDOWS.point,
            ELECTRON_PROCESSES_ONE_WINDOW.point,
        );
        assert!(
            tauri > electron,
            "Tauri on Windows must have MORE processes than Electron ({tauri} vs {electron})"
        );
    }
}
