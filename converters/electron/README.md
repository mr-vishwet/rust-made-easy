# electron-to-rust

> Phase 1 converter of **[Rust Made Easy](../../README.md)** — the anything-to-Rust
> migration toolkit. This module converts Electron desktop apps to Rust (Tauri 2).

[![CI](https://github.com/mr-vishwet/rust-made-easy/actions/workflows/ci.yml/badge.svg)](https://github.com/mr-vishwet/rust-made-easy/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/electron-to-rust?color=E2862A&label=crates.io)](https://crates.io/crates/electron-to-rust)
[![License: MIT](https://img.shields.io/badge/license-MIT-555)](../../LICENSE)
[![GitHub stars](https://img.shields.io/github/stars/mr-vishwet/rust-made-easy?style=social)](https://github.com/mr-vishwet/rust-made-easy)

**Electron to Rust, as a command.** `electron-to-rust` (`e2r`) helps you **convert Electron app to Rust**: it scans your Electron desktop app, reports exactly which Electron APIs it uses, and generates a working Tauri 2 project from it. If you are planning an **Electron to Tauri migration**, start here — one command to see the shape of the job, one more to get a running Tauri app.

> **Status:** early. v0.1 covers `scan`, `simulate`, `convert`, `calibrate`, and `serve` for common Electron apps. The main process becomes Rust; the renderer is carried over automatically when e2r can find it — built output, bundler source, or static files (see [Quickstart](#quickstart) and [What converts](#what-converts)). See [docs/RND.md](../../docs/RND.md) for why this exists.

## The 30-second pitch

Electron apps ship a whole Chromium browser with every install: a typical installer runs ~106 MB (90–160 MB; our measured Electron 45.5.0 runtime is 273.7 MiB unpacked), and the memory cost is visible to every user. Tauri 2 keeps your web UI but swaps the shell for a Rust backend and the operating system's own webview — our measured e2r-generated Tauri binaries run 11–27 MB by default (11.4 MB with zero plugins, ~2 MB per plugin), and ~5 MB with the docs' size-optimised profile. The catch has always been the migration: someone has to read your main process, map every Electron API to its Tauri equivalent, and rewrite the IPC layer by hand.

`e2r` does the mechanical part. It scans your app and tells you, API by API, what converts automatically, what needs a human, and what can't convert yet — then generates the Tauri project, the Rust commands, the TypeScript bridge, and a migration-notes file that says exactly what is left for you.

## Why now

On November 4, 2026, Discord co-founder and CTO Stanislav Vishnevskiy confirmed on X that Discord is rebuilding its desktop client in Rust — Electron gone, React Native gone on mobile — with GPUI (the Zed team's GPU framework) drawing the interface, explicitly not Tauri. An early Windows build, codenamed "Discord Phoenix", showed ~290 MB of RAM in a single Task Manager screenshot — one snapshot, not a benchmark, and no release date yet. But the direction is unmistakable: the biggest Electron app in the world has decided the browser-shell era is over. Every other Electron team is now asking the same question Discord already answered. This tool is the practical answer for teams that don't have Discord's headcount: a real migration path, in stages, with the UI untouched.

The full research — prior art, the gap in the market, the hard parts, and the sources — is in [docs/RND.md](../../docs/RND.md).

## Quickstart

```sh
# From a clone of the repo (crates.io release coming with v0.1.0)
git clone https://github.com/mr-vishwet/rust-made-easy
cd rust-made-easy
cargo install --path converters/electron/crates/e2r-cli

# 1. X-ray your app: APIs used, IPC channels, feasibility score
e2r scan ./my-electron-app

# 2. Predict the migration before any code changes (see [Commands](#commands))
e2r simulate ./my-electron-app --record ./predictions.json

# 3. Generate a Tauri 2 project
e2r convert ./my-electron-app --out ./my-app-tauri
```

**What `scan` gives you:** a report of every Electron API your app uses, every IPC channel, a feasibility score, and each API classified as AUTO (converts mechanically), ASSISTED (converts with a human decision), or MANUAL (needs a rewrite or a recipe).

**What `simulate` gives you:** the migratable breakdown, a UI preservation estimate, predicted gains with confidence ranges (bundle size, memory, startup time, process count — CPU is refused, not guessed), warnings, and an effort hint. Predictions are ranges with confidence levels, never single numbers, and every number carries its basis. See [Commands](#commands).

**What `convert` gives you:**

```
my-app-tauri/
├── src-tauri/
│   ├── Cargo.toml          # Tauri 2 app, plugins for what you actually use
│   ├── tauri.conf.json     # windows, bundle config
│   ├── build.rs            # required by Tauri's generate_context!()
│   ├── icons/
│   │   └── icon.png        # placeholder icon — replace with your own
│   └── src/
│       ├── main.rs         # entry point
│       ├── lib.rs          # plugin setup, window setup
│       ├── commands.rs     # one Rust command per ipcMain handler (TODO stubs)
│       └── menu.rs         # menu builder (when a static template was found)
├── src/
│   ├── lib/
│   │   └── e2r-bridge.ts   # typed invoke() wrapper replacing ipcRenderer
│   └── (your renderer source, when a bundler setup was found)
├── dist/
│   └── index.html          # your built frontend (copied) — a placeholder only when no renderer was found
├── package.json
└── MIGRATION-NOTES.md      # what converted, what didn't, what to do next
```

e2r carries your renderer over automatically: built output from `dist/`/`build/`/`out/`, bundler source plus its build script, or static files at the app root. Only when it finds no renderer at all does it generate a placeholder `dist/index.html` — `MIGRATION-NOTES.md` says which case applied. If your app has a bundler setup, build the frontend once (`npm run build:frontend`); `tauri build` runs it automatically afterwards. Then run it:

```sh
cd my-app-tauri
npm install
npm run tauri dev
```

> **Honest v0.1 note:** the renderer is carried over when e2r can find it (see above), and the dev server port is detected from your Vite config, dev script, or bundler default — but exotic setups (electron-vite, Next.js without static export, custom build output dirs) still need manual adaptation, and `MIGRATION-NOTES.md` spells out exactly what. The generated Rust commands are compiling stubs: each one marks the `ipcMain.handle` channel it came from with a `TODO(e2r)` for you (or your coding agent) to port.

## Commands

| Command | What it does |
| --- | --- |
| `e2r scan <app-dir> [--json] [--out <file>]` | X-ray the app: APIs, IPC channels, feasibility score, AUTO / ASSISTED / MANUAL per API. |
| `e2r simulate <app-dir> [--format human\|json\|markdown] [--json] [--target-platform macos\|windows\|linux] [--record <file>] [--out <file>] [--calibration-log <path>] [--no-calibration]` | Predict the migration's shape and payoff before any code changes. |
| `e2r calibrate <app-dir> --actual <measurements.json> [--record <file>] [--log <path>]` | Compare a predictions record against measured post-migration actuals; append the comparisons to the calibration log. |
| `e2r convert <app-dir> --out <dir>` | Generate a working Tauri 2 project. Also writes the decision log (`.e2r/decisions.jsonl`, one entry per API finding) — see [docs/MCP.md](../../docs/MCP.md#the-decision-log). |
| `e2r serve` | Run the MCP server on stdio: six tools for AI agents (analyse, plan, look up, simulate, validate, decision log). Full reference: [docs/MCP.md](../../docs/MCP.md); agent setup: [skills/e2r/](../../skills/e2r/). |

### `simulate` flags

- `--format` — report format: `human` (default), `json`, or `markdown` (condensed,
  for pasting into issues and PRs). `--json` is a shorthand for `--format json`.
- `--target-platform` — the platform the per-platform predictions (memory, process
  count) are for: `macos`, `windows`, or `linux`. Defaults to the platform the
  binary runs on. Memory is platform-dependent (macOS −40%, Windows ~0%, Linux
  refused as unmeasured) and the process count goes UP on Windows (4 → 7), so the
  model never blends platforms — the platform is part of the metric id
  (`memory_macos`, `process_count_windows`) in every output format.
- `--record <file>` — also write a predictions-record JSON (schema version 1) for
  the calibration loop: the predicted *after* ranges, the scan summary, and the UI
  preservation estimate.
- `--calibration-log <path>` / `--no-calibration` — which calibration log to read
  for interval adjustment (default `~/.cache/e2r/calibration.jsonl`, or
  `E2R_CALIBRATION_LOG` when set); `--no-calibration` simulates with the model's
  unadjusted intervals.

### `calibrate` and the `measurements.json` schema

After the migration, measure the real app and write the actuals into a
`measurements.json` file, then compare:

```sh
e2r calibrate ./my-electron-app --actual ./measurements.json --record ./predictions.json
```

`--record` defaults to `<app-dir>/.e2r/predictions.json` when omitted. `calibrate`
reports per-metric error (predicted vs actual, absolute + % error, whether the
actual fell inside the predicted interval) and appends one JSONL entry per
compared metric to the calibration log (`~/.cache/e2r/calibration.jsonl` by
default, overridable via `--log` / `E2R_CALIBRATION_LOG`). Future `simulate` runs
read the log and adjust confidence intervals: ≥3 logged comparisons per metric,
widen 1.5× when actuals repeatedly fall outside, narrow 0.8× (floored at ±10% of
the point) when they consistently fall inside. The point never moves — calibration
adjusts confidence, not the central estimate. Full design:
[docs/IMPLEMENTATION.md](../../docs/IMPLEMENTATION.md#54-calibration-loop).

```jsonc
// measurements.json — schema_version 1. Every metric is optional;
// calibrate compares only metrics present in BOTH the record and the
// actuals, and says so for the rest.
{
  "schema_version": 1,
  "app": { "name": "my-app", "version": "1.0.0" },
  "measured_at": "2026-11-25T04:00:00Z",
  "platform": "linux",              // "macos" | "windows" | "linux"
  "metrics": {
    "bundle_size_mb": 14.2,         // measured Tauri release binary, MB
    "memory_before_mb": 210.0,      // Electron idle memory, MB
    "memory_after_mb": 130.0,       // Tauri idle memory, MB
    "startup_before_ms": 420.0,     // Electron startup, ms
    "startup_after_ms": 390.0,      // Tauri startup, ms
    "process_count": 4              // Tauri process count (1 window, idle)
  }
}
```

Memory and startup predictions are *ratios*, so they need a before AND an after
measurement (the ratio is after / before); a lone after-value is reported as not
comparable. A measurements file whose platform does not match the predictions
record's platform produces a warning — memory and process-count predictions are
per-platform.

## What converts

Full table: [docs/API-MAP.md](../../docs/API-MAP.md). The source of truth is `mappings/electron-to-tauri.json` (in this converter's directory).

| Electron API | Tauri target | Status |
| --- | --- | --- |
| `app`, `app.getPath`, `app.quit` | `tauri::App`, `app.path()`, `app.exit()` | AUTO |
| `BrowserWindow` (incl. `electron-window-state` defaults, `minWidth`/`minHeight`, `backgroundColor`, icon) | `WebviewWindow` | AUTO |
| `ipcMain.handle` / `ipcRenderer.invoke` | `#[tauri::command]` + `invoke()` | AUTO |
| `ipcMain.on` | `app.listen` stubs in `listeners.rs` + `emit()` | AUTO |
| `ipcRenderer.on` / `webContents.send` | `listen()` wrappers (`mainEvents`) | AUTO |
| `dialog.showOpenDialog` / `showSaveDialog` | `tauri-plugin-dialog` | AUTO |
| `dialog.showMessageBox` | `tauri-plugin-dialog` | AUTO |
| `shell.openExternal` / `openPath` | `tauri-plugin-opener` | AUTO |
| `Notification` | `tauri-plugin-notification` | AUTO |
| `globalShortcut` | `tauri-plugin-global-shortcut` | AUTO |
| `autoUpdater` / `electron-updater` | `tauri-plugin-updater` | AUTO |
| `clipboard` | `tauri-plugin-clipboard-manager` | AUTO |
| `nativeTheme` | `set_theme` command (`window.set_theme`) | AUTO |
| `app.requestSingleInstanceLock` | `tauri-plugin-single-instance` | AUTO |
| `app.setAsDefaultProtocolClient` | `tauri-plugin-deep-link` | AUTO |
| `electron-window-state` | `tauri-plugin-window-state` | AUTO |
| `contextBridge` | typed `invoke()` bridge (from the recovered preload shape) | AUTO |
| `electron-store` | `tauri-plugin-store` | AUTO |
| Plugin permissions | `src-tauri/capabilities/default.json` (`core:default` + each plugin's `:default`) | AUTO |
| Renderer (built output in `dist/` / `build/` / `out/`) | copied into the generated project, `frontendDist` wired | AUTO |
| Renderer (bundler source + `build` script) | source copied, build script preserved as `build:frontend` | AUTO |
| Renderer (static `index.html` + assets) | copied into `dist/` | AUTO |
| Renderer (electron-vite / Next.js / exotic bundler) | source copied; build step needs manual adaptation | ASSISTED |
| `Menu` | `tauri::menu` | ASSISTED |
| `Tray` | `tauri::tray` | ASSISTED |
| `protocol` | `tauri::UriSchemeProtocol` / custom protocol | ASSISTED |
| `safeStorage` | OS keychain via plugin | ASSISTED |
| `contextIsolation: false` | bridge replaces direct Node/Electron access; renderer needs review | ASSISTED |
| `session` / cookies | Webview cookie APIs (partial) | MANUAL |
| `nodeIntegration: true` | no Node runtime in a webview; renderer rewrite needed | MANUAL |
| `remote` / `@electron/remote` | no equivalent; each call becomes `invoke()` + a Rust command | MANUAL |
| `powerMonitor` | no direct equivalent | MANUAL |
| `crashReporter` | external crash service | MANUAL |
| `node-pty`, `better-sqlite3` | Rust crates (`portable-pty`, `rusqlite`) | MANUAL |

The `shell` / `Notification` / `autoUpdater` / `globalShortcut` / `clipboard` /
`nativeTheme` rows are proven end to end: the `full-apis-electron-app` golden
fixture uses all six, and its golden test runs a real `cargo check` on the
generated Tauri project — so the `tauri-plugin-opener` /
`tauri-plugin-notification` / `tauri-plugin-updater` /
`tauri-plugin-global-shortcut` / `tauri-plugin-clipboard-manager` init
expressions and the `set_theme` command in the generated code are verified to
compile. The same fixture also uses dialog and `electron-store`, so seven
plugin inits are proven together in one project; the `dialog` and `store`
inits were already proven by the `minimal-electron-app` fixture, whose
`ipcMain.on` channel proves the `listeners.rs` codegen compiles too.

Battle-tested on real apps (November 2026): `electro-dash` (Electron 9, thin
main process) scans 90/100 — the 10 points are the MANUAL `nodeIntegration`
finding — and `simplenote-electron` (Electron 34, 2,060 lines of main-process
JS, 7 `ipcMain.on` handlers) scans 80/100 (MANUAL: `session`, `remote`). Both
generated projects pass a real `cargo check`; the Simplenote project proves
the single-instance / deep-link / window-state plugin inits compile, and its
`tauri.conf.json` carries the real 1024×768 window, version 2.27.1, and bundle
id `com.automattic.simplenote`. (The full per-bug fix record lives in the
working tree alongside this repo, `battle-tests/FIXES.md` — it is not part
of the repo itself.)

## The 3-stage migration ladder

You don't have to port everything at once. `e2r` supports the ladder proven by projects like [agent-pane](https://github.com/copse-dev/agent-pane):

1. **Stage 1 — Tauri shell + Node sidecar.** Your app runs in Tauri on day one. The Electron main process is shimmed and runs as a Node sidecar; zero logic ported, IPC over a loopback channel.
2. **Stage 2 — Port handlers one by one.** Each `ipcMain` handler becomes a Rust command. Record/replay parity checks (in the [rust-anything](https://github.com/erkamyaman/rust-anything) style) prove each ported handler behaves like the original before the sidecar copy is dropped.
3. **Stage 3 — Pure Rust.** The sidecar is removed. One binary, no Node runtime.

See [docs/RND.md](../../docs/RND.md#the-3-stage-migration-ladder) for the full description.

## Roadmap

The v0.1 CLI is Phase 1 of a bigger product: the **Migration Brain** — an AI-powered
migration-intelligence layer where the coding agent does the coding and this tool provides the
intelligence, rules, checks, and tools. Full vision: [PRD.md](../../PRD.md).

| Phase | Name | What ships |
| --- | --- | --- |
| **1** | Core Intelligence & Simulation | The v0.1 CLI (`scan` + `convert`, data-driven mappings, golden fixture, CI) plus the parser-based analyzer, the Intermediate Representation, the pre-migration simulator (shipped: `simulate` + `calibrate`), and the decision registry. |
| **2** | Agent Integration | MCP server (shipped: `analyse_electron_app`, `get_migration_plan`, `get_rust_replacement`, `simulate_migration`, `validate_migration`, `get_decision_log`), agent skills (shipped: OpenCode, Claude Code, Codex; Antigravity planned), IDE extensions, record/replay parity checks, and the Stage 1 sidecar-shim mode. |
| **3** | Companion Experience | The Live Migration Companion window (floating overlay + full workspace app; Simulation / Live / Verification modes), the structured event bus, Migration Replay, and the Developer Dashboard. |
| **4** | ML & Validation | The Performance Prediction Engine's learning loop (the deterministic core ships now as `e2r calibrate`; phase 4 adds per-profile calibration, the automatic bench step, and ML on top), UI regression detection, native-module recipes (`node-pty`, `better-sqlite3`), and the full-native UI target option (egui / Iced / Dioxus / Slint — and GPUI for teams that want Discord's route). |
| **5** | Marketplace & Hosted Intelligence | Community-contributed mappings and recipes as a marketplace, and an optional hosted platform. The open-source core stays open source. |
| **5+** | Anything-to-Rust | The IR makes other sources possible: Android apps and other frameworks as migration sources — the full Anything-to-Rust vision, built on the same Brain. |

## FAQ

**Is this affiliated with Tauri, Discord, or rust-anything?**
No. This is an independent project by [Vishwet Nadimetla](https://github.com/mr-vishwet). It targets Tauri 2, it cites Discord's public announcement as motivation, and it is complementary to rust-anything (which ports application logic; this tool ports the Electron shell). None of those projects endorse it.

**Does it rewrite my UI?**
No — your renderer — the HTML, CSS, and JavaScript your users see — runs unchanged in Tauri's webview. `convert` carries it over automatically: built output from `dist/`/`build/`/`out/` is copied verbatim, bundler source is copied with its build script preserved (so `tauri build` rebuilds it), and plain static files are copied into `dist/`. Only when no renderer files are found does it generate a placeholder `dist/index.html`, and `MIGRATION-NOTES.md` says so. Only the main process and the IPC layer change. (A native-UI target is phase 4, opt-in.)

**What about native modules like `node-pty` or `better-sqlite3`?**
Honestly: they are the hard part, and v0.1 does not convert them automatically. `scan` flags them as MANUAL with the Rust crate that replaces them (`portable-pty`, `rusqlite`), and phase 3 adds tested recipes for the common ones. If your app leans heavily on native modules, expect real porting work — the migration notes will say so up front.

**How good is the generated code?**
It compiles — every conversion is checked with `cargo check` in CI against the golden fixture. It is idiomatic scaffolding, not hand-tuned Rust: expect to read it, adjust the parts flagged ASSISTED, and delete what you don't need.

**Does it handle my `preload.js`?**
A sandboxed preload that only uses `contextBridge` and `ipcRenderer` ports essentially verbatim — the transport is rebound from `ipcRenderer` to Tauri's `invoke()`. Preloads that touch Node APIs directly need the sidecar stage or a manual port.

**Will my app be smaller / use less memory?**
Size, almost certainly: our measured e2r-generated Tauri binaries run 11–27 MB by default (11.4 MB with zero plugins, ~2 MB per plugin) versus a ~106 MB Electron installer (90–160 MB; our measured Electron 45.5.0 runtime is 273.7 MiB unpacked). Memory is platform-dependent — macOS sees a real idle-memory win (about 40% less in the best-measured study), Windows sees about none (WebView2 is Chromium), Linux is unmeasured — and startup shows no reliable advantage either way. We make no benchmark claims about *your* app: run `e2r simulate` for the ranges, then measure. Full data: [docs/RND.md](../../docs/RND.md#11-simulator-research-and-calibration).

**What if my app uses an API that isn't mapped?**
`scan` lists it as unmapped, `convert` leaves a stub and a note in `MIGRATION-NOTES.md`, and the app still builds. Then please [open an issue](https://github.com/mr-vishwet/rust-made-easy/issues) — new mappings are the easiest contribution (see [CONTRIBUTING.md](../../CONTRIBUTING.md)).

**Can I use this on a proprietary / closed-source app?**
Yes. `e2r` runs entirely on your machine, reads your code, and generates code. It never phones home and collects no telemetry. Your code never leaves your disk.

## Contributing

Bug reports, new API mappings, fixtures from real apps, and documentation fixes are all welcome. See [CONTRIBUTING.md](../../CONTRIBUTING.md).

## License

MIT. See [LICENSE](../../LICENSE).

---

*Phase 1 of the [Anything-to-Rust](../../docs/RND.md) vision: every app, eventually, in Rust — starting with the desktop shell.*
