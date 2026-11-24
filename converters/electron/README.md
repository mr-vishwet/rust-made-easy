# electron-to-rust

> Phase 1 converter of **[Rust Made Easy](../../README.md)** — the anything-to-Rust
> migration toolkit. This module converts Electron desktop apps to Rust (Tauri 2).

[![CI](https://github.com/mr-vishwet/rust-made-easy/actions/workflows/ci.yml/badge.svg)](https://github.com/mr-vishwet/rust-made-easy/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/electron-to-rust?color=E2862A&label=crates.io)](https://crates.io/crates/electron-to-rust)
[![License: MIT](https://img.shields.io/badge/license-MIT-555)](../../LICENSE)
[![GitHub stars](https://img.shields.io/github/stars/mr-vishwet/rust-made-easy?style=social)](https://github.com/mr-vishwet/rust-made-easy)

**Electron to Rust, as a command.** `electron-to-rust` (`e2r`) helps you **convert Electron app to Rust**: it scans your Electron desktop app, reports exactly which Electron APIs it uses, and generates a working Tauri 2 project from it. If you are planning an **Electron to Tauri migration**, start here — one command to see the shape of the job, one more to get a running Tauri app.

> **Status:** early. v0.1 covers `scan` and `convert` for common Electron apps. The main process becomes Rust; the renderer is carried over by you (see [Quickstart](#quickstart)). See [What converts](#what-converts) for honest coverage, and [docs/RND.md](../../docs/RND.md) for why this exists.

## The 30-second pitch

Electron apps ship a whole Chromium browser with every install: typical bundles run 85–160 MB and up, and the memory cost is visible to every user. Tauri 2 keeps your web UI but swaps the shell for a Rust backend and the operating system's own webview — typical bundles are 3–15 MB. The catch has always been the migration: someone has to read your main process, map every Electron API to its Tauri equivalent, and rewrite the IPC layer by hand.

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

# 2. Generate a Tauri 2 project
e2r convert ./my-electron-app --out ./my-app-tauri
```

**What `scan` gives you:** a report of every Electron API your app uses, every IPC channel, a feasibility score, and each API classified as AUTO (converts mechanically), ASSISTED (converts with a human decision), or MANUAL (needs a rewrite or a recipe).

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
│   └── lib/
│       └── e2r-bridge.ts   # typed invoke() wrapper replacing ipcRenderer
├── dist/
│   └── index.html          # placeholder frontend — replace with your build
├── package.json
└── MIGRATION-NOTES.md      # what converted, what didn't, what to do next
```

Then copy your renderer source into `src/` (or point `build.frontendDist` in
`src-tauri/tauri.conf.json` at your existing build output), delete the placeholder
`dist/index.html`, and run it:

```sh
cd my-app-tauri
npm install
npm run tauri dev
```

> **Honest v0.1 note:** `convert` does not copy your renderer yet — that step is yours, and
> `MIGRATION-NOTES.md` spells it out. Automatic renderer copying is planned. The generated
> Rust commands are compiling stubs: each one marks the `ipcMain.handle` channel it came
> from with a `TODO(e2r)` for you (or your coding agent) to port.

## What converts

Full table: [docs/API-MAP.md](../../docs/API-MAP.md). The source of truth is `mappings/electron-to-tauri.json` (in this converter's directory).

| Electron API | Tauri target | Status |
| --- | --- | --- |
| `app`, `app.getPath`, `app.quit` | `tauri::App`, `app.path()`, `app.exit()` | AUTO |
| `BrowserWindow` | `WebviewWindow` | AUTO |
| `ipcMain.handle` / `ipcRenderer.invoke` | `#[tauri::command]` + `invoke()` | AUTO |
| `ipcMain.on` / `ipcRenderer.send` | `#[tauri::command]` + `emit`/`listen` | AUTO |
| `dialog.showOpenDialog` / `showSaveDialog` | `tauri-plugin-dialog` | AUTO |
| `dialog.showMessageBox` | `tauri-plugin-dialog` | AUTO |
| `shell.openExternal` / `openPath` | `tauri-plugin-opener` | AUTO |
| `Notification` | `tauri-plugin-notification` | AUTO |
| `globalShortcut` | `tauri-plugin-global-shortcut` | AUTO |
| `autoUpdater` | `tauri-plugin-updater` | AUTO |
| `clipboard` | `tauri-plugin-clipboard-manager` | AUTO |
| `contextBridge` | typed `invoke()` bridge | AUTO |
| `electron-store` | `tauri-plugin-store` | AUTO |
| `Menu` | `tauri::menu` | ASSISTED |
| `Tray` | `tauri::tray` | ASSISTED |
| `protocol` | `tauri::UriSchemeProtocol` / custom protocol | ASSISTED |
| `safeStorage` | OS keychain via plugin | ASSISTED |
| `session` / cookies | Webview cookie APIs (partial) | MANUAL |
| `powerMonitor` | no direct equivalent | MANUAL |
| `crashReporter` | external crash service | MANUAL |
| `node-pty`, `better-sqlite3` | Rust crates (`portable-pty`, `rusqlite`) | MANUAL |

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
| **1** | Core Intelligence & Simulation | The v0.1 CLI (`scan` + `convert`, data-driven mappings, golden fixture, CI) plus the parser-based analyzer, the Intermediate Representation, the pre-migration simulator, and the decision registry. |
| **2** | Agent Integration | MCP server (`analyse_electron_app`, `get_migration_plan`, `get_rust_replacement`, `simulate_migration`, `validate_migration`, `get_decision_log`), agent skills (OpenCode, Claude Code, Codex, Antigravity), IDE extensions, record/replay parity checks, and the Stage 1 sidecar-shim mode. |
| **3** | Companion Experience | The Live Migration Companion window (floating overlay + full workspace app; Simulation / Live / Verification modes), the structured event bus, Migration Replay, and the Developer Dashboard. |
| **4** | ML & Validation | The Performance Prediction Engine's learning loop (predicted vs. actual, fed back into the simulator), UI regression detection, native-module recipes (`node-pty`, `better-sqlite3`), and the full-native UI target option (egui / Iced / Dioxus / Slint — and GPUI for teams that want Discord's route). |
| **5** | Marketplace & Hosted Intelligence | Community-contributed mappings and recipes as a marketplace, and an optional hosted platform. The open-source core stays open source. |
| **5+** | Anything-to-Rust | The IR makes other sources possible: Android apps and other frameworks as migration sources — the full Anything-to-Rust vision, built on the same Brain. |

## FAQ

**Is this affiliated with Tauri, Discord, or rust-anything?**
No. This is an independent project by [Vishwet Nadimetla](https://github.com/mr-vishwet). It targets Tauri 2, it cites Discord's public announcement as motivation, and it is complementary to rust-anything (which ports application logic; this tool ports the Electron shell). None of those projects endorse it.

**Does it rewrite my UI?**
No — and in v0.1 it doesn't copy your renderer either: you copy your frontend into the
generated project (or point `frontendDist` at your build output) as `MIGRATION-NOTES.md`
explains. Your renderer — the HTML, CSS, and JavaScript your users see — then runs unchanged
in Tauri's webview. Only the main process and the IPC layer change. (Automatic renderer
copying is planned, and a native-UI target is phase 4, opt-in.)

**What about native modules like `node-pty` or `better-sqlite3`?**
Honestly: they are the hard part, and v0.1 does not convert them automatically. `scan` flags them as MANUAL with the Rust crate that replaces them (`portable-pty`, `rusqlite`), and phase 3 adds tested recipes for the common ones. If your app leans heavily on native modules, expect real porting work — the migration notes will say so up front.

**How good is the generated code?**
It compiles — every conversion is checked with `cargo check` in CI against the golden fixture. It is idiomatic scaffolding, not hand-tuned Rust: expect to read it, adjust the parts flagged ASSISTED, and delete what you don't need.

**Does it handle my `preload.js`?**
A sandboxed preload that only uses `contextBridge` and `ipcRenderer` ports essentially verbatim — the transport is rebound from `ipcRenderer` to Tauri's `invoke()`. Preloads that touch Node APIs directly need the sidecar stage or a manual port.

**Will my app be smaller / use less memory?**
Typical Tauri bundles are 3–15 MB versus 85–160 MB+ for Electron (widely cited ranges — we have not measured your app). Memory depends on what your app does; the webview still holds your UI. We make no benchmark claims. Run it and measure.

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
