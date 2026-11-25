# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `e2r serve` — the MCP server: the Migration Brain over the Model Context Protocol (stdio, rmcp 3.11.0), exposing six tools to AI coding agents — `analyse_electron_app`, `get_migration_plan`, `get_rust_replacement`, `simulate_migration`, `validate_migration`, `get_decision_log`. Every tool returns structured JSON; failures are structured errors (`INVALID_ARGUMENT`, `NOT_FOUND`, `API_NOT_FOUND`, `NOT_E2R_PROJECT`, `SCAN_FAILED`, `SIMULATION_FAILED`, `IO_ERROR`), never crashes. Conversion stays a CLI step — there is no `convert` tool — and `validate_migration` is a static check that never runs `cargo check`. Full reference: [docs/MCP.md](docs/MCP.md).
- The decision registry: `convert` now appends one entry per Electron API finding to `<out-dir>/.e2r/decisions.jsonl` (format v1, append-only JSONL: API, Tauri replacement, classification, rationale, confidence, source), readable via the `get_decision_log` MCP tool or the shared log at `~/.cache/e2r/decisions.jsonl` (`E2R_DECISIONS_LOG` overrides). A corrupt line is skipped on read, a missing log reads empty, and a write failure warns instead of failing the conversion; the log is a metadata sidecar, not listed in `files_written`.
- The e2r agent skill ([skills/e2r/](skills/e2r/)): `SKILL.md` (when to use the server, the canonical analyse → simulate → plan → convert → validate workflow, mandatory honesty rules), copy-paste MCP configs for Claude Code, Codex, and OpenCode v2, and `smoke-test.sh` (bash + python3, no jq) proving the server answers with no agent involved.
- `e2r simulate <app-dir>` — the pre-migration simulator: migratable breakdown (AUTO / ASSISTED / MANUAL, weighted by occurrences), UI preservation estimate (renderer kind, framework, bundler, Electron-API touch points with file:line), performance predictions with confidence ranges (bundle size, memory, startup time, process count; CPU refused, not guessed), warnings, and an effort hint. Human, JSON, and Markdown output; `--record` writes a predictions record for the calibration loop.
- Research-backed prediction model (model version `0.1.0`): bundle size modeled from measured e2r-generated Tauri 2.12.7 release binaries (11.4 MB zero-plugin floor, ~2 MB per plugin, interval absorbing the unexplained ±~6 MB electro-dash anomaly) and a measured Electron 45.5.0 baseline (273.7 MiB unpacked, 118.2 MiB compressed); memory per platform (macOS −40%, Windows ~0%, Linux refused as unmeasured); startup predicts no improvement (point exactly 1.0); process count per platform (Windows rises 4 → 7). The third-party "3–15 MB" range and the Teams 2.0 vendor claims are no longer model inputs. Data sources and citations: [docs/RND.md](docs/RND.md#11-simulator-research-and-calibration).
- `e2r calibrate <app-dir> --actual <measurements.json> [--record <path>] [--log <path>]` — compares a predictions record against measured post-migration actuals (per-metric absolute + % error, inside/outside the predicted interval) and appends the comparisons to a local calibration log (`~/.cache/e2r/calibration.jsonl`, overridable via `--log` / `E2R_CALIBRATION_LOG`). `simulate` reads the log and deterministically adjusts confidence intervals (widen 1.5× when actuals repeatedly fall outside, narrow 0.8× floored at ±10% when they stay inside, ≥3 comparisons per metric); the point never moves.
- `e2r scan <app-dir>` — the X-ray report: every Electron API the app uses, every IPC channel,
  a feasibility score, and each API classified AUTO / ASSISTED / MANUAL.
- `e2r convert <app-dir> --out <dir>` — generates a Tauri 2 project: `src-tauri` with
  `Cargo.toml`, `tauri.conf.json`, `lib.rs`, `commands.rs`, and `main.rs`, plus a TypeScript
  bridge (`src/lib/e2r-bridge.ts`) replacing `ipcRenderer`, and a `MIGRATION-NOTES.md` listing what
  converted, what needs a human, and what can't convert yet.
- Rust event listeners for `ipcMain.on` channels: `convert` now generates
  `src-tauri/src/listeners.rs` with one `app.listen` stub per channel (registered from
  `lib.rs`), and the bridge gains `listen()` wrappers (`mainEvents`) for every
  `webContents.send` / `ipcRenderer.on` channel.
- Tauri 2 capabilities: `convert` now emits `src-tauri/capabilities/default.json` granting
  `core:default` plus the `:default` permission set of every plugin the project includes.
- `nativeTheme` codegen: a real `set_theme` command (callable via `api.setTheme` in the bridge)
  mapping `"dark"` / `"light"` / `"system"` onto the Tauri window theme API.
- Typed preload bridges: the bridge now derives typed wrappers from the recovered preload
  shape (including `platform()` booleans for `process.platform` checks) instead of a generic
  `Proxy`, which invented commands and turned booleans into functions.
- App identity carry-through: `version`, `productName`, and `appId` from `package.json` (or
  `electron-builder.json`) now flow into `Cargo.toml`, `tauri.conf.json`, and `package.json`.
- Window fidelity: `electron-window-state` `defaultWidth`/`defaultHeight` are resolved for
  dynamic window sizes, and literal `minWidth`/`minHeight`/`backgroundColor` are carried into
  `tauri.conf.json`; unresolvable dimensions are reported as unknown instead of a confident
  800×600. The real app icon is copied when a `BrowserWindow` references a PNG on disk.
- New scan patterns: `nodeIntegration: true` (MANUAL), `contextIsolation: false`,
  `remote` (MANUAL), single-instance (`tauri-plugin-single-instance`), deep links
  (`tauri-plugin-deep-link`), and `electron-window-state` (`tauri-plugin-window-state`).
  The scanner also strips comments before matching, requires a call-site reference for
  bare-word APIs (dead imports no longer drive findings or plugins), extends the
  Electron-import gate to ecosystem packages (`electron-updater`, `electron-store`,
  `electron-window-state`, `@electron/remote`), dedups preload references against the
  referencing file's directory, records occurrence counts per finding, and detects the dev
  server URL from the main process (`loadURL` / `url.format`).
- Pure-JS port-needing dependencies (`electron-updater`, `electron-store`,
  `electron-window-state`, `@electron/remote`) are now reported separately from true native
  addons and no longer cost 15 feasibility points each when a 1:1 plugin equivalent exists.
- Renderer carry-over in `e2r convert`: existing build output (`dist/`/`build/`/`out/`
  with an `index.html`) is copied verbatim, bundler source is copied with its `build`
  script preserved as `build:frontend` (wired as `beforeBuildCommand`), and plain static
  renderers are copied into `dist/` — a placeholder `dist/index.html` is generated only
  when no renderer files are found. The dev server port is detected from the dev script,
  the Vite config, or the bundler default instead of a hardcoded 1420.
- A second golden fixture (`converters/electron/fixtures/built-output-electron-app/`) with
  a real renderer build output, asserting the generated project contains the real files
  and `frontendDist` points at them.
- Data-driven API mappings in `converters/electron/mappings/electron-to-tauri.json`, covering the core Electron
  surface: `app`, `BrowserWindow`, IPC (`ipcMain`/`ipcRenderer`), dialogs, menus, tray,
  `shell`, notifications, global shortcuts, the updater, clipboard, `contextBridge`, and
  `electron-store`, with honest statuses for the rest.
- A golden fixture app under `converters/electron/fixtures/` that the test suite scans and converts, with
  `cargo check` run against the generated Tauri project so generated code is proven to compile.
- A third golden fixture (`converters/electron/fixtures/full-apis-electron-app/`) using
  `autoUpdater` (check for updates, notify, quit-and-install), `shell.openExternal`, and
  `Notification` (show + click handling), with a preload that exposes a second key
  (`versions`); its golden test runs a real `cargo check` on the generated Tauri project,
  proving the updater / opener / notification plugin init expressions compile. The fixture
  also uses dialog and `electron-store`, so all five plugin inits are proven together in one
  generated project.
- Documentation: the research doc ([docs/RND.md](docs/RND.md)), the architecture doc
  ([docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)), and the API map
  ([docs/API-MAP.md](docs/API-MAP.md)).
- CI ([.github/workflows/ci.yml](.github/workflows/ci.yml)): `cargo fmt --check`,
  `cargo clippy -- -D warnings`, and `cargo test` on every push and pull request.

### Fixed

- The scanner now skips `build/` output directories (alongside `dist/` and `out/`), so bundled
  JS in a Create React App `build/` folder no longer produces junk findings.
- `app.getPath` was mapped to `tauri-plugin-opener`; it now maps to the core Tauri path API
  (`app.path()`) with no plugin.
- The bridge's `to_camel_case` flattened camelCase channel names (`appStateUpdate` became
  `appstateupdate`); existing case is now preserved.
- Stale docs: [PRD.md](PRD.md) no longer claims v0.1 generates a placeholder frontend (renderer
  carry-over is implemented), and the `e2r` CLI docs no longer call `convert` a stub.

## [0.1.0] - Unreleased

First public release. See [Unreleased] above.

[Unreleased]: https://github.com/mr-vishwet/rust-made-easy/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/mr-vishwet/rust-made-easy/releases/tag/v0.1.0
