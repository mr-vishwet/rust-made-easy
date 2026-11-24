# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `e2r scan <app-dir>` — the X-ray report: every Electron API the app uses, every IPC channel,
  a feasibility score, and each API classified AUTO / ASSISTED / MANUAL.
- `e2r convert <app-dir> --out <dir>` — generates a Tauri 2 project: `src-tauri` with
  `Cargo.toml`, `tauri.conf.json`, `lib.rs`, `commands.rs`, and `main.rs`, plus a TypeScript
  bridge (`src/lib/e2r-bridge.ts`) replacing `ipcRenderer`, and a `MIGRATION-NOTES.md` listing what
  converted, what needs a human, and what can't convert yet.
- Data-driven API mappings in `converters/electron/mappings/electron-to-tauri.json`, covering the core Electron
  surface: `app`, `BrowserWindow`, IPC (`ipcMain`/`ipcRenderer`), dialogs, menus, tray,
  `shell`, notifications, global shortcuts, the updater, clipboard, `contextBridge`, and
  `electron-store`, with honest statuses for the rest.
- A golden fixture app under `converters/electron/fixtures/` that the test suite scans and converts, with
  `cargo check` run against the generated Tauri project so generated code is proven to compile.
- Documentation: the research doc ([docs/RND.md](docs/RND.md)), the architecture doc
  ([docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)), and the API map
  ([docs/API-MAP.md](docs/API-MAP.md)).
- CI ([.github/workflows/ci.yml](.github/workflows/ci.yml)): `cargo fmt --check`,
  `cargo clippy -- -D warnings`, and `cargo test` on every push and pull request.

## [0.1.0] - Unreleased

First public release. See [Unreleased] above.

[Unreleased]: https://github.com/mr-vishwet/rust-made-easy/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/mr-vishwet/rust-made-easy/releases/tag/v0.1.0
