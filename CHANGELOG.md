# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `e2r serve` — the MCP server: the Migration Brain over the Model Context Protocol (stdio, rmcp 3.11.0), exposing six tools to AI coding agents — `analyse_electron_app`, `get_migration_plan`, `get_rust_replacement`, `simulate_migration`, `validate_migration`, `get_decision_log`. Every tool returns structured JSON; failures are structured errors (`INVALID_ARGUMENT`, `NOT_FOUND`, `API_NOT_FOUND`, `NOT_E2R_PROJECT`, `SCAN_FAILED`, `SIMULATION_FAILED`, `IO_ERROR`), never crashes. Conversion stays a CLI step — there is no `convert` tool — and `validate_migration` is a static check that never runs `cargo check`. Full reference: [docs/MCP.md](docs/MCP.md).
- The decision registry: `convert` now appends one entry per Electron API finding to `<out-dir>/.e2r/decisions.jsonl` (format v1, append-only JSONL: API, Tauri replacement, classification, rationale, confidence, source), readable via the `get_decision_log` MCP tool or the shared log at `~/.cache/e2r/decisions.jsonl` (`E2R_DECISIONS_LOG` overrides). A corrupt line is skipped on read, a missing log reads empty, and a write failure warns instead of failing the conversion; the log is a metadata sidecar, not listed in `files_written`.
- The e2r agent skill ([skills/e2r/](skills/e2r/)): `SKILL.md` (when to use the server, the canonical analyse → simulate → plan → convert → validate workflow, mandatory honesty rules), copy-paste MCP configs for Claude Code, Codex, and OpenCode v2, and `smoke-test.sh` (bash + python3, no jq) proving the server answers with no agent involved.
- `e2r simulate <app-dir>` — the pre-migration simulator: migratable breakdown (AUTO / ASSISTED / MANUAL, weighted by occurrences), UI preservation estimate (renderer kind, framework, bundler, Electron-API touch points with file:line), performance predictions with confidence ranges (bundle size, binary size, memory, startup time, process count; CPU refused, not guessed), warnings, and an effort hint. Human, JSON, and Markdown output; `--record` writes a predictions record for the calibration loop.
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
- Proof-campaign fix round (accuracy on VS Code / Signal / Joplin / GitHub Desktop /
  Mailspring was 85/116 = 73.3% strict before these fixes; the fixes are generic,
  scanner-level, not app-specific):
  - Test files are now excluded from scan findings everywhere (findings, occurrences,
    IPC channels, windows, renderer touch points): `test/`, `tests/`, `spec/`, `specs/`,
    `__tests__/`, `fixtures/`, `playwright/`, `*.test.*`, `*.spec.*`, `*_test.*`, `test-*.*`.
  - Identifier resolution: string literals are stripped before matching non-IPC patterns,
    and a bare identifier must resolve to an Electron import binding (destructured,
    aliased, or `require()` forms), an inline `require('electron')` /
    `require('@electron/remote')` access, or an Electron export passed in as a parameter
    or via DI. Same-named locals (`this.session`, `window.reduxActions.app`,
    `navigator.clipboard`, `state.remote`, Testing Library `screen`) no longer produce
    findings; remote-mediated calls are forced to MANUAL. A file-local declaration of the
    same name shadows the app-wide fallback (typed `session: DraftEditingSession`
    parameters, `const session = ...`, non-Electron imports — the smoke re-scan caught
    Mailspring's draft/agent sessions and GitHub Desktop's credential sessions counting
    as Electron's `session`), and an object-literal property key (`{ session: ... }`) is
    not a use. Untyped parameters keep the DI benefit; an `Electron.*`-typed parameter is
    not a shadow.
  - Window detection resolves options passed as a variable and accepts the
    `electron.BrowserWindow` prefix; a window with no resolvable size is reported as
    unknown instead of a confident 800×600.
  - Monorepo `package.json`: all `package.json` files are read (bounded
    depth) and the Electron app package supplies name/version/Electron version/main entry; native modules
    are detected from dependencies across all packages (added: `@signalapp/sqlcipher`,
    `sqlite3`, `keytar`, `sqlite-vec`, `fs-admin`, `registry-js`, `node-llama-cpp`) and
    the full list is recorded in the scan output. `vendor/` directories are excluded
    from package discovery (a vendored package's `main` + existing target outscored the
    real app package), and score ties go to the shallower package (an internal package
    tied with `app/` and won under the old deeper-first tie-break).
  - Renderer detection searches monorepo layouts (`packages/*/`, `app/src/ui/`, `app/src/`,
    `ts/` + `bundles/`/`out/`/`dist/` under package dirs), excludes the main-process entry
    from renderer candidates, and only reports `vite` for a real vite dependency, config,
    or script token; `rolldown` is detected as its own bundler. The converter records the
    found source directory on the renderer plan, and the simulator scans that directory —
    not the fixed root set — for touch points.
  - Menu: all `Menu.buildFromTemplate` sites are collected and the app menu (the one
    passed to `Menu.setApplicationMenu`) is reported separately from tray/context menus.
  - `app.setAsDefaultProtocolClient` now matches registration calls only; the `'open-url'`
    listener alternative was dropped.
  - New scan patterns + mappings (map grew from 48 to 95 entries): `WebContentsView`,
    `utilityProcess`, `MessageChannelMain`, `desktopCapturer`, `systemPreferences`,
    `webFrame`, `webUtils`, `contentTracing`, `ipcMain.handleOnce`, `MenuItem`,
    `getGlobal` / `getCurrentWindow` / `getCurrentWebContents`, the `@electron/remote`
    inline-require form, `Menu.setApplicationMenu`, variable-channel `ipcRenderer.invoke`
    (recorded as `"dynamic"`), aliased `app`, member-level `app.*` (MANUAL) and `net.*`
    (`resolveHost` / `isOnline` are ASSISTED, not `reqwest`).
  - Preload: `*Preload.js` suffixes, `setPreloadScripts` registration, and variable preload
    paths are detected; the reported file list is the count that matters.
  - Comment stripping is regex-aware: a regex literal containing quotes no longer leaves a
    phantom open quote that hides `//` comments. JSX closing tags (`</div>`) are not
    treated as regex starts, and an unterminated "regex" no longer duplicates the newline
    it stops at — both corrupted line numbers in `.tsx` files (windows reported +2/+6
    lines late in the smoke re-scan).
  - `simulate` excludes e2r's own outputs (`predictions.json`, scan/simulate outputs,
    `.e2r/`, proof notes) from the measured app payload, and merges `package.json` files
    across the tree for framework/bundler detection.
  - 25 new regression tests (17 scan, 5 converter, 3 simulator), including 3 more from
    the smoke re-scan (JSX line-number preservation at the stripper and scan levels;
    monorepo tie-break + vendor exclusion; local-shadow session false positives); the
    MCP replacement list-all count updated from 48 to 95.
- Proof-campaign fix round 2 (v2 accuracy 85/101 = 84.2% strict; the v2 scores had
  saturated — see the score-formula change below):
  - Package picker: the root package now gets a scoring bonus and packages under
    `extensions/` a −50 penalty, so VS Code's scan reports the real app
    (code-oss-dev 1.149.0 / Electron 43.8.10 from `nativeBuild.target`) instead of
    `extensions/copilot` (copilot-chat 0.77.0). The Electron version falls back to the
    app package's `nativeBuild.target` before any other package's `electron`
    dependency.
  - Preload entry points vs modules: only files a window actually loads count as
    preload files; `*.preload.ts` modules (Signal's ~600) are listed separately as
    preload-context modules. Referenced preload paths are resolved against the app
    tree (`path.join(__dirname, …)` segment by segment, `.min.js` → `.ts` source
    fallback, root-relative paths against the app root) and nonexistent ones are
    dropped — VS Code's phantom preloads and Mailspring's
    `print/lib/print-preload.js` no longer appear.
  - Test exclusion extended to `test-node/`, `*_test.main.ts`, and `.vscode/`; fixture
    windows in `simulation/` scenes are not app windows.
  - Native modules: `@getinsomnia/node-libcurl` and `@parcel/watcher` added to the
    detection list.
  - Insomnia: `ipcMainHandle` / `ipcMainOn` / `ipcMainOnce` wrapper registrations are
    now counted (the `ipcMain.*` call inside a wrapper definition is not);
    the `<webview>` tag and the `webviewTag` webPreference are MANUAL findings (new
    mapping entry — the map is now 96 entries); `electron-updater`'s `autoUpdater`
    is reported as `electron-updater`, never as Electron's `autoUpdater`; window
    sizes given as `const NAME = <number>` resolve to the number.
  - Remote mediation is now a flag: findings forced to MANUAL through
    `@electron/remote` (including remote-derived `webContents`, `Menu`, and
    `webContents.send`) carry `remote_mediated: true`, so "no Tauri equivalent"
    can be split from "reached through remote". Typed `session: Session`
    parameters and `Electron.Session` aliases (`directSession`) make
    `session.setProxy` and friends real `session` findings.
  - Dependencies are no longer findings: `package.json` dependency declarations
    (port-needing JS deps, build tooling) no longer appear as findings at
    `package.json:1`; they still show in the report and in `scan.json`
    (`port_needing_deps` / `build_tooling`).
  - Dev URL ranking: candidates are ranked (a `loadURL` in main-process code wins
    over an extension service URL or a CLI docs server) and must score above 0,
    or no dev URL is reported. The dev script is searched in workspace packages
    too (`dev` → `start:dev-server` → `dev:server` → `start` → `serve`) —
    Insomnia's `packages/insomnia` `start:dev-server` (`vite dev`, port 3334) is
    found, while Electron-only `start` scripts still correctly report no dev
    server.
  - Three regexes that used look-behind (unsupported by the `regex` crate, so
    they silently never matched) now use an explicit boundary prefix: the
    bare-assignment `require(...).member` binding, the
    `.session.fromPartition` session alias, and the bare-assignment
    remote-derived names.
  - Calibrate compared unlike quantities (proof-campaign addition 1): the
    `bundle_size` prediction is binary + payload, but `measurements.json`
    `bundle_size_mb` is the binary alone — five log entries compared 22–27 MB
    binaries against 42–400 MB bundle intervals and reported 0/5 inside.
    The simulator now emits an explicit `binary_size` prediction (zero-plugin
    binary 11.0–17.4 MB + ~2 MB per plugin, no payload), and `calibrate`
    compares the measured binary against the binary interval only;
    `bundle_size` compares only against a measured shipped bundle (new
    `measurements.json` field `shipped_bundle_size_mb`) and is reported as
    unmeasured without one, never silently scored as a miss. The five wrong
    log entries were removed from `~/.cache/e2r/calibration.jsonl` with a
    documented correction (removed lines preserved at
    `proof/calibration-correction/`). Once like is compared with like, all
    five binaries land inside their binary intervals and binary + predicted
    payload lands inside the bundle interval for all five.
  - Renderer pick (proof-campaign addition 2): `convert` copied the wrong
    renderer for 3/5 apps — Joplin `packages/pdf-viewer`, Signal
    `sticker-creator/src`, Insomnia `examples/insomnia-plugin-sandbox-demo`
    — while the real renderers went unpicked. The scan and the convert
    shared the pick (`detect_renderer` / `find_source_dir` is one code
    path, used by both), so the fix is in that function: example/demo/
    sample/sandbox/playground trees are never descended into; dirs inside
    a foreign package (a sibling sub-app or library, not the app package)
    are not candidates; candidates rank by `.tsx`/`.jsx` density first,
    then a strict entry (`.tsx`/`.jsx` stem or `index.html` — a barrel
    `index.ts` is not a renderer entry); entry-less dirs count when dense
    (≥10 `.tsx`/`.jsx`); container dirs collapse to the deepest dir holding
    ≥90% of their renderer files. The v3 scans (not a re-convert) prove
    the fix; the v2 convert outputs stand as measured with the earlier
    binary.
  - Golden test `test_scan_full_apis_fixture`: the fixture binds
    `autoUpdater` from `require('electron-updater')`, so it now correctly
    expects an `electron-updater` finding and asserts no bare `autoUpdater`
    finding exists.
  - IPC-wrapper scan panic (found by the v3 smoke run): `find_ipc_wrappers`
    sliced `content[tail_start..tail_start + 600]`; when byte 600 landed
    inside a multi-byte character the slice panicked and the whole scan
    died — Joplin's sources contain 'Ř' at exactly such a spot, so the
    fixed binary could not scan Joplin at all. The end is now snapped
    down to a char boundary.
  - Renderer pick, second round (found by the v3 verification): the first
    foreign-package rule wrongly excluded Mailspring's 53
    `internal_packages/*` (each with a `main` field) and narrowed its
    renderer from `app/` to `app/src/` (touch points 520 → 23). Foreign
    now means the owning package sits outside the app package's dir, or
    is a standalone sub-app (no `main` field) — internal packages with a
    `main` are the app's own UI. A second bug in the same function: the
    density count capped its walk at depth 4 relative to each dir, so
    `app/` counted 269 files while `internal_packages/` counted 260 (a
    96.7% ratio that collapsed `app/` away) against true shares of 464
    vs 308 (66%); the count now has no depth limit (the 2,000 cap bounds
    the work). `app/` is restored (526 touch points) and all five
    renderer picks were verified with convert smoke runs against the
    final binary.
  - Dev-URL gate (found by the v3 verification): a candidate now needs
    score ≥ 10 (a `loadURL` line or a window-loading file). The +5
    main-process path bonus alone admitted VS Code's OAuth loopback port
    (127.0.0.1:33418, a redirect URI in `mainThreadAuthentication.ts`)
    as its dev URL; VS Code has no dev server, so it now correctly
    reports none, while Insomnia's real dev server (localhost:3334,
    score 15) is kept.
  - Re-scan script self-inflation (found by the v3 run): the script
    copied the run-1 predictions record to `predictions.run1.json`
    INSIDE the app dir, and run 2 counted that copy as payload (+0.302 MB
    for VS Code — exactly the record's size). The run-1 copy now lives
    outside the app dir (`proof/.rescan-tmp/`), and the comparison
    ignores `generated_at` (a timestamp, which can never match
    byte-for-byte) while asserting the payload is identical. All five
    v3 payloads are identical across runs (VS Code 365.465 MB, Signal
    156.514, Joplin 262.919, Mailspring 69.586, Insomnia 27.653).
  - MCP list-all count (found by the fix-round-2 completion run): the
    `webview` mapping entry took the table from 95 to 96 entries, but
    the MCP handler test and its doc comment still asserted 95, so the
    full suite failed on `test_handlers_return_structured_json_for_every_tool`.
    Both now assert 96.
- Proof campaign (5 real Electron apps converted — full report:
  [docs/PROOF.md](docs/PROOF.md)):
  - VS Code, Signal Desktop, Joplin, Mailspring, Insomnia (all `--depth 1`
    clones, 2026-11-25; GitHub Desktop kept as first alternate): 5/5
    `e2r convert` exit 0, 5/5 `cargo check` PASS, 5/5 `cargo build
    --release` PASS (Tauri 2.12.7), no hand-fixes to generated code.
    Measured release binaries 22.42–27.26 MB (unstripped) vs measured
    Linux .deb installers 117.3–463.6 MiB (binary-vs-installer, not
    bundle-vs-bundle — every generated project ships a placeholder
    frontend, so no shipped-bundle figure exists yet).
  - v3 feasibility scores (new formula, see Changed): 19 / 42 / 23 / 23 /
    46. Latest hand-verified scan accuracy is v2's 85/101 = 84.2%
    (v1: 102/134 = 76.1%); v3 accuracy was **not** hand-verified.
  - Calibrate like-with-like fix (see the fix-round-2 entry above): the
    five wrong log entries were removed with a documented correction
    (`proof/calibration-correction/`); once binary is compared against
    the binary interval, all five measured binaries land inside, and
    binary + predicted payload lands inside the bundle interval for all
    five. No runtime metric was measured (no built frontend to launch);
    memory/CPU predictions remain refused, startup a flat 0.8–1.3×.

### Changed

- Feasibility score formula (v3): the v2 formula saturated — every proof app hit both
  the MANUAL (−40) and ASSISTED (−15) caps, so the score degenerated to
  `45 − 15 × natives` and Joplin scored 0. The new formula charges
  `min(10 × natives, 30) + min(2 × MANUAL_groups, 30) +
  min(max(ASSISTED_groups − 5, 0), 15) + round(20 × MANUAL_occurrences /
  total_occurrences) + (10 if ipcRenderer is used with no preload file)`,
  clamped to [0, 100]. On the v2 proof numbers it gives VS Code 29, Signal 42,
  Joplin 23, Mailspring 24, Insomnia 62 — five different scores, none 0 by
  construction. Group counts remain the primary signal. Documented in
  [docs/RND.md](docs/RND.md#feasibility-score-v3-formula) and the converter
  README.

## [0.1.0] - Unreleased

First public release. See [Unreleased] above.

[Unreleased]: https://github.com/mr-vishwet/rust-made-easy/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/mr-vishwet/rust-made-easy/releases/tag/v0.1.0
