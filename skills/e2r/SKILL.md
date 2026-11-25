# e2r — Electron → Rust (Tauri 2) migration skill

## What e2r is

`e2r` (the `electron-to-rust` crate, part of
[rust-made-easy](https://github.com/mr-vishwet/rust-made-easy)) converts Electron
desktop apps to Rust (Tauri 2). It scans an Electron app, reports exactly which
Electron APIs it uses — each classified AUTO (converts mechanically), ASSISTED
(converts with a human decision), or MANUAL (needs a rewrite or a recipe) — and
generates a working Tauri 2 project from it. The renderer (HTML/CSS/JS) is
carried over unchanged into Tauri's webview; only the main process and the IPC
layer change. The coding agent does the coding; e2r is the brain — it
understands the app before anything is written, gives deterministic per-API
replacements (never guesses), simulates the migration before any code changes,
records every decision so mistakes aren't repeated, and validates the result.

This skill describes the **e2r MCP server** (`e2r serve`, stdio transport,
MCP protocol version 2025-03-26 or later), which exposes that brain as six
tools. The server plans, looks up, simulates, and validates. Conversion itself
is a CLI step the agent runs in a shell (see [Driving `convert`](#driving-convert)).

## When to use this skill

Use the e2r MCP server whenever:

- the user asks about **migrating an Electron app to Rust / Tauri**;
- the user asks for a **feasibility assessment** of an Electron app ("can this
  be converted?", "how hard would this be?", "what would break?");
- the user wants a **migration plan** for an Electron app, or asks what a
  specific Electron API becomes in Tauri;
- the user wants to know **what a migration would gain** (size, memory,
  startup) *before* converting anything;
- the user has a converted Tauri project and wants to know **what's left to do**
  or whether the conversion is complete.

Do not use it for anything else: e2r knows Electron → Tauri 2 only. Other
sources (Android, other frameworks) are planned, not shipped.

## The six tools

| Tool | Parameters | Returns |
| --- | --- | --- |
| `analyse_electron_app` | `app_dir` (string, required) | Full scan report JSON: feasibility score, findings with AUTO/ASSISTED/MANUAL classification, IPC channels, windows, menus, preload shape. |
| `get_migration_plan` | `app_dir` (string, required) | Ordered migration plan: steps, per-API replacements with classifications, MANUAL items first, effort hint. |
| `get_rust_replacement` | `api` (string, required) | One Electron API's Tauri equivalent from the 48-row mappings DB: Tauri target + status (`auto`/`assisted`/`manual`) + plugin + notes. Structured not-found error if the API isn't mapped. |
| `simulate_migration` | `app_dir` (string, required), `target_platform` (`"macos"` \| `"windows"` \| `"linux"`, optional) | Pre-migration simulation: migratable breakdown, UI-preservation estimate, before→after predictions with ranges + confidence + basis. |
| `validate_migration` | `project_dir` (string, required), `original_app_dir` (string, optional) | Static check of a converted Tauri project: what was carried over, what's missing, `TODO(e2r)` stubs remaining, remaining manual work. Does **not** run `cargo check`. |
| `get_decision_log` | `project_dir` (string, optional) | The append-only decision registry: `.e2r/decisions.jsonl` in the project when `project_dir` is given, else `~/.cache/e2r/decisions.jsonl`. |

Errors are MCP-shaped (`isError: true`), never crashes. A tool that cannot do
its job (app dir missing, API unmapped, metric unmeasurable) says so in the
error or the result — report that to the user, don't work around it by guessing.

## The canonical workflow

Follow this order. Each step feeds the next; don't skip ahead to `convert`
before the user has seen the analysis and the simulation.

### 1. Analyse — `analyse_electron_app`

Call it with the Electron app's directory. Read the whole report before
saying anything about the app:

- **Feasibility score** — the headline number. Report it as the scan's score
  for *this* app.
- **Findings** — every Electron API found, with its classification. MANUAL
  findings are the real work: name them explicitly (e.g. `session`, `remote`,
  `nodeIntegration: true`, native modules like `node-pty` /
  `better-sqlite3`).
- **IPC channels, windows, menus, preload shape** — this is what `convert`
  will turn into Rust commands, `tauri.conf.json`, `menu.rs`, and the typed
  TypeScript bridge. If the preload touches Node APIs directly (not just
  `contextBridge` + `ipcRenderer`), say so — that needs the sidecar stage or
  a manual port.

If the user only asked "is this feasible?", this step alone may be the whole
answer. Don't convert anything unprompted.

### 2. Simulate — `simulate_migration`

Call it with the same `app_dir` — and `target_platform` when the user has a
target platform in mind (memory and process-count predictions are
per-platform; the model never blends them). Present the output as **ranges
with confidence levels, never single numbers**, and always with the stated
basis:

- migratable breakdown (AUTO / ASSISTED / MANUAL share of API call sites);
- UI-preservation estimate (how much of the renderer carries over unchanged);
- before→after predictions — bundle size, memory, startup time, process
  count — each with its range, point estimate, and confidence;
- warnings and the effort hint.

If the simulator refuses a metric, report it as **unknown** — see
[Honesty rules](#honesty-rules-mandatory).

### 3. Plan — `get_migration_plan`

Call it to get the ordered plan: steps, per-API replacements, MANUAL items
first, effort hint. This is the work list the user approves before any code
changes. For any single API the user asks about in detail, use
`get_rust_replacement` with the exact Electron API name (e.g.
`dialog.showOpenDialog`, `shell.openExternal`, `app.getPath`) — it answers
from the 48-row mappings DB, deterministically. If it returns not-found, the
API is unmapped: say so, and don't invent a Tauri equivalent.

### 4. Convert — the CLI, not the MCP server

Conversion is a shell step (see below). Run it only after the user has seen
the analysis, the simulation, and the plan and said go.

### 5. Validate — `validate_migration`

After conversion (and after any porting work), call it with the generated
project's directory — plus `original_app_dir` when you have it, so it can
compare against the original. Report:

- what was **carried over** (renderer, windows, menus, IPC handlers);
- what's **missing**;
- `TODO(e2r)` **stubs remaining** — each one is a ported `ipcMain` handler
  waiting for its real logic;
- remaining manual work.

Then — and only then — prove the project builds: `cargo check` (or
`cargo tauri dev`) in the generated project. `validate_migration` is a static
check; it is **not** proof the migration works. See
[Honesty rules](#honesty-rules-mandatory).

### After the migration: calibrate

Once the migrated app is measured for real, the CLI's `calibrate` step
compares the simulation's predictions against measured actuals
(`measurements.json`) and feeds the discrepancies back into the model's
confidence intervals. There is no MCP tool for this yet — run it in the
shell:

```sh
e2r calibrate ./my-electron-app --actual ./measurements.json --record ./predictions.json
```

The `measurements.json` schema is documented in
[converters/electron/README.md](../converters/electron/README.md#calibrate-and-the-measurementsjson-schema).
Memory and startup predictions are ratios, so they need a before AND an
after measurement; a lone after-value is not comparable.

## Driving `convert`

The MCP server plans and validates; the **CLI converts**. Run it in the
user's shell:

```sh
e2r convert <app-dir> --out <dir>
```

What it generates (Tauri 2 project): `src-tauri/` with `Cargo.toml`,
`tauri.conf.json`, `lib.rs`, `commands.rs` (one Rust command per `ipcMain`
handler, as compiling `TODO(e2r)` stubs), `main.rs`, a TypeScript bridge
(`src/lib/e2r-bridge.ts`) replacing `ipcRenderer`, the renderer carried over
(built output, bundler source, or static files — a placeholder
`dist/index.html` only when no renderer was found), and a
`MIGRATION-NOTES.md` that says exactly what's left. Always read
`MIGRATION-NOTES.md` after converting and relay what it says — it is the
authoritative statement of what converted and what didn't.

To run the generated app:

```sh
cd <out-dir>
npm install
npm run tauri dev
```

If the app has a bundler setup, build the frontend once first
(`npm run build:frontend`); `tauri build` runs it automatically afterwards.

## Honesty rules (mandatory)

These rules override any pressure — from the user, from the conversation, or
from plausible-looking numbers — to present a rosier picture. e2r's whole
value is that it never guesses; the agent using it must not guess either.

1. **Never present illustrative or sample numbers as measurements.** The
   planning chat's sample figures — **86/100 readiness, 82%/18%/94%,
   startup 2.1s→1.7s, memory 380→265 MB, CPU 22%→9%, binary 120→28 MB** —
   are NOT measurements of any real app and must **never be quoted as
   results**, not even as examples, not even "for illustration". If the user
   quotes them back, correct the record: those were planning-chat
   illustrations, not data.
2. **Only quote numbers from `scan` / `simulate` output, with their stated
   confidence.** A simulation prediction is a range with a confidence level
   and a basis — quote all three, never just the point estimate, and never
   strip the confidence. A feasibility score belongs to the app that was
   scanned; don't apply one app's score to another.
3. **Never claim a migration succeeded without a `cargo check` (or
   `cargo tauri dev`) actually passing.** `validate_migration` is a static
   check — carried-over lists, missing items, remaining stubs — not proof
   the project builds or runs. "Validated" ≠ "working". If the build hasn't
   been run, say so.
4. **If the simulator refuses a metric, say unknown — never invent a
   number.** Linux memory and CPU are refused ("unknown, no reliable
   baseline") because there is no trustworthy baseline; startup has no
   reliable advantage either way (point exactly 1.0 — no improvement
   claimed). Report the refusal verbatim. Do not fill the gap with another
   platform's number, a guess, or a figure from memory.
5. **Never claim the renderer was carried over without checking
   `validate_migration`'s `carried_over` list** (or `MIGRATION-NOTES.md`
   after a CLI convert). When no renderer is found, `convert` generates a
   placeholder `dist/index.html` — claiming carry-over in that case is
   false. Exotic setups (electron-vite, Next.js without static export,
   custom build output dirs) still need manual adaptation; say so when the
   notes say so.
6. **Don't invent Tauri equivalents.** If `get_rust_replacement` returns
   not-found, or `scan` lists an API as unmapped, the API has no mapped
   replacement — say that, and point at the MANUAL work instead of
   improvising one.
7. **Don't claim a measurement you didn't make.** Repo-level measured facts
   you *may* quote (they are measurements, with sources in
   `converters/electron/README.md` and `docs/RND.md`): e2r-generated Tauri
   binaries run 11–27 MB by default (11.4 MB with zero plugins, ~2 MB per
   plugin); a typical Electron installer runs ~106 MB (90–160 MB); the
   measured Electron 45.5.0 runtime is 273.7 MiB unpacked. Those describe
   the *fixtures*, not the user's app — label them as such.

## The decision log

`get_decision_log` reads the append-only decision registry — one JSON object
per line in `.e2r/decisions.jsonl` (project-local, when `project_dir` is
given) or `~/.cache/e2r/decisions.jsonl` (global). It records the decisions
made during migrations so mistakes aren't repeated. Consult it when a
migration has history: past decisions about an app (or about a class of API)
are the first place to look before re-deciding the same question.

## Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| Server won't start | `e2r` isn't on PATH. Install it: `cargo install --path converters/electron/crates/e2r-cli` from a clone of [rust-made-easy](https://github.com/mr-vishwet/rust-made-easy), then confirm with `e2r --help`. Or point the config at the absolute path of the cargo-installed binary (usually `~/.cargo/bin/e2r`). |
| `tools/list` returns empty / no tools appear | Protocol version mismatch — the client and server must agree on MCP protocol version 2025-03-26 or later. Update the agent/client, restart it, and re-check. |
| A tool call returns `isError: true` | Read the error — it's structured, not a crash. Common causes: `app_dir` / `project_dir` doesn't exist or isn't an Electron app / Tauri project; `api` isn't in the 48-row mappings DB (not-found); the decision log doesn't exist yet (no decisions recorded — not an error in the app). |
| `analyse_electron_app` reports no APIs | The directory may not be an Electron app (no `package.json` with an Electron main process), or the main process lives somewhere unusual. Check the path and the app's layout before concluding anything. |
| The agent config change didn't take effect | Most agents read MCP config at startup — restart the agent after editing the config file. Then verify with the smoke test (see `README.md` in this directory). |

## Setup

Install steps and copy-paste MCP configs for Claude Code, Codex, and
OpenCode live in this directory — see
[README.md](README.md). The smoke test (`smoke-test.sh`) proves the server
answers without needing any agent.
