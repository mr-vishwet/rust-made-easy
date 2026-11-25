# The e2r MCP server

`e2r serve` exposes the Migration Brain to AI coding agents over the Model Context
Protocol (stdio transport). The agent does the coding; the server is the brain — it
analyses an Electron app, plans its migration deterministically, looks up Tauri
replacements from the mappings database, simulates the payoff before any code changes,
validates a converted project, and reads back the decision registry. Conversion itself
is a CLI step (`e2r convert`); the server plans and validates, it does not convert.

Server name `e2r`, version `0.1.0`. Built on [rmcp](https://crates.io/crates/rmcp)
3.11.0 (the official Rust MCP SDK) + tokio; the transport, protocol negotiation, and
JSON-RPC framing are the SDK's job. The server answers the `initialize` handshake with
protocol version `2025-03-26` and the six tools below. It runs until the client
disconnects (stdin EOF), then exits promptly.

## Setup

Install `e2r` first (from a clone of the repo — same command as the
[README](../README.md#quickstart--phase-1-electron--rust)):

```sh
cargo install --path converters/electron/crates/e2r-cli
e2r --help
```

Then point your agent at the server. Copy-paste configs live in
[`skills/e2r/`](../skills/e2r/) — one file per agent, each with a header comment
saying where it goes:

| Agent | Config file | Where it goes |
| --- | --- | --- |
| Claude Code | `skills/e2r/claude-code.mcp.json` | `.mcp.json` in your project root, or merge the `e2r` entry into `~/.claude.json` |
| Codex | `skills/e2r/codex-config.toml` | Merge into `~/.codex/config.toml` |
| OpenCode (v2) | `skills/e2r/opencode.json` | Merge the `mcp` object into `opencode.json` (project root) or `~/.config/opencode/opencode.json` |

Every config runs the same thing: `e2r` with the `serve` argument, over stdio.
(`e2r` must resolve on PATH, or use the absolute path to the cargo-installed
binary, usually `~/.cargo/bin/e2r`.) Restart the agent after editing its config —
most agents read MCP config at startup.

The agent skill itself is [`skills/e2r/SKILL.md`](../skills/e2r/SKILL.md): when to
use the server, the canonical analyse → simulate → plan → convert → validate
workflow, and the mandatory honesty rules. Setup, troubleshooting, and the full
skill are documented in [`skills/e2r/README.md`](../skills/e2r/README.md).

## Smoke test

No agent needed — the smoke test starts `e2r serve`, speaks the stdio protocol
(`initialize`, `notifications/initialized`, `tools/list`, one `tools/call` against
the repo's minimal fixture), and prints PASS/FAIL per step:

```sh
./skills/e2r/smoke-test.sh
# [PASS] initialize — server='e2r' protocolVersion='2025-03-26'
# [PASS] notifications/initialized — sent (no response expected)
# [PASS] tools/list — 6 tools: [...]
# [PASS] tools/call analyse_electron_app — scan report returned (...)
# [PASS] server exits on stdin EOF — exit code 0
#
# 5/5 steps passed
```

It needs only bash + python3 (no jq). Environment overrides: `E2R_BIN` (path to
the binary; default `e2r` on PATH, falling back to `target/debug/e2r` then
`target/release/e2r` in the repo), `E2R_FIXTURE` (app dir for the `tools/call`
step), `E2R_TIMEOUT` (seconds for the whole conversation; default 30).

## Response shape

Every tool returns structured JSON: success as `structuredContent` plus a text
mirror of the same JSON, so clients without structured-content support still see
the result. Failures are `isError: true` with a structured error object — never
a crash, never a panic:

```json
{
  "code": "NOT_FOUND",
  "message": "app_dir '/nope' does not exist or is not a directory",
  "details": { "argument": "app_dir", "path": "/nope" }
}
```

### Error codes

Stable strings agents can match on:

| Code | Meaning |
| --- | --- |
| `INVALID_ARGUMENT` | A required argument is missing, empty, or malformed (e.g. an unknown `target_platform`). |
| `NOT_FOUND` | A directory or file the tool needs does not exist. |
| `API_NOT_FOUND` | No mapping-table row for the requested API (`get_rust_replacement` only). |
| `NOT_E2R_PROJECT` | `validate_migration` was pointed at a directory that is not an e2r-generated Tauri project. |
| `SCAN_FAILED` | The underlying scan failed; `message` carries the reason. |
| `SIMULATION_FAILED` | The underlying simulation failed; `message` carries the reason. |
| `IO_ERROR` | A filesystem operation (or report serialization) failed. |

Two errors live one level up, in the JSON-RPC layer rather than the tool layer:
calling a tool that does not exist is a JSON-RPC error with code `-32602`, and a
call missing a required argument is rejected by rmcp before the handler runs
(`isError: true`, no structured content — the text names the missing field, e.g.
`missing field \`app_dir\``).

## The six tools

Input schemas are generated from the Rust request structs with schemars
(`src/mcp.rs`) and served verbatim by `tools/list`; the schemas below are quoted
from the server's own `tools/list` output.

### `analyse_electron_app` — scan an Electron app

```jsonc
// input
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "properties": {
    "app_dir": {
      "description": "Directory of the Electron app to analyse.",
      "type": "string"
    }
  },
  "required": ["app_dir"],
  "type": "object"
}
```

Returns the full scan report (`ScanReport`) as JSON: `app_name`, `app_version`,
`product_name`, `app_id`, `electron_version`, `main_entry`, `files_scanned`,
`ipc_channels`, `main_to_renderer`, `findings` (one per API per file: `api`,
`category`, `file`, `line`, `classification`, `tauri_target`, `note`,
`occurrences`), `preload`, `native_modules`, `port_needing_deps`, `dev_url`,
`feasibility_score` (0–100), `verdict`, `windows`, `menu`.

Errors: `NOT_FOUND` when `app_dir` does not exist, `SCAN_FAILED` when the scan
itself fails.

### `get_migration_plan` — the ordered migration plan

```jsonc
// input
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "properties": {
    "app_dir": {
      "description": "Directory of the Electron app to plan the migration for.",
      "type": "string"
    }
  },
  "required": ["app_dir"],
  "type": "object"
}
```

Scans the app and simulates it, then composes the two into a plan. The plan is
deterministic: same app → same plan (no timestamps, no unordered collections).
The effort hint and breakdown come from the simulator, which reads the
calibration log exactly like `e2r simulate` does; the scan is the single source
of truth for the replacements.

```jsonc
// output (MigrationPlan)
{
  "app_name": "my-electron-app",
  "feasibility_score": 90,
  "verdict": "Good fit — mostly automatic",
  "steps": [
    // 4 steps, in order: analyse → simulate → convert → validate.
    // Each: { "order": 1, "name": "analyse", "description": "...",
    //         "command": "e2r scan <app-dir>" }.
    // Step 4's command is the validate_migration tool call, not a CLI command.
  ],
  "manual_first": [
    // MANUAL findings, called out first, in scan order:
    // { "api": "session", "file": "main.js", "line": 9, "note": "..." }
  ],
  "replacements": [
    // Per-API replacements, deduplicated by API, in first-seen order:
    // { "api": "dialog.showOpenDialog", "classification": "auto",
    //   "tauri_target": "tauri-plugin-dialog open()",
    //   "plugin": "tauri-plugin-dialog", "notes": "...",
    //   "files": ["main.js:12", "main.js:40"] }
  ],
  "effort": "small",            // "trivial" | "small" | "medium" | "large"
  "effort_note": "...",         // what the hint is based on, and its limits
  "breakdown": {                // migratable breakdown from the simulator
    "auto": 20, "assisted": 2, "manual": 0, "total": 22,
    "auto_pct": 90.9, "assisted_pct": 9.1, "manual_pct": 0.0
  }
}
```

Errors: `NOT_FOUND` when `app_dir` does not exist, `SCAN_FAILED` /
`SIMULATION_FAILED` when the underlying scan or simulation fails.

### `get_rust_replacement` — look up one API (or dump the table)

```jsonc
// input
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "properties": {
    "api": {
      "default": null,
      "description": "Electron API name, e.g. \"dialog.showOpenDialog\". Required unless `list_all` is true.",
      "type": ["string", "null"]
    },
    "list_all": {
      "default": null,
      "description": "When true, dump the whole mappings table instead of one lookup.",
      "type": ["boolean", "null"]
    }
  },
  "type": "object"
}
```

A single lookup returns the mapping row verbatim from
`converters/electron/mappings/electron-to-tauri.json`:

```json
{
  "electron_api": "dialog.showOpenDialog",
  "tauri_target": "tauri-plugin-dialog open()",
  "plugin": "tauri-plugin-dialog",
  "status": "auto",
  "notes": "File picker options map closely; multi-select and filters are supported."
}
```

`status` is `"auto"`, `"assisted"`, or `"manual"`. With `list_all: true` the
tool returns the whole table: `{ "count": 48, "mappings": [...] }`.

Errors: `INVALID_ARGUMENT` when `api` is empty (and `list_all` is false),
`API_NOT_FOUND` when the table has no row for the API.

### `simulate_migration` — predict the migration before any code changes

```jsonc
// input
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "properties": {
    "app_dir": {
      "description": "Directory of the Electron app to simulate.",
      "type": "string"
    },
    "target_platform": {
      "default": null,
      "description": "Target platform for the per-platform predictions: \"macos\", \"windows\", or \"linux\". Defaults to the host platform.",
      "type": ["string", "null"]
    }
  },
  "required": ["app_dir"],
  "type": "object"
}
```

Returns the full simulation report (`SimulationReport`) as JSON: `app_name`,
`app_version`, `feasibility_score`, `verdict`, `breakdown` (AUTO / ASSISTED /
MANUAL call sites, counts + percentages), `ui_preservation`, `predictions` (one
per metric — bundle size, memory, startup time, process count, CPU — each a
range with a confidence level, a basis, and assumptions), `warnings`, `effort`,
`effort_note`, `model_version` (`"0.1.0"`). Calibration comes from the default
log, exactly like `e2r simulate` (a missing log behaves like no calibration).
`target_platform` is case-insensitive; omitted, it defaults to the host
platform.

Errors: `NOT_FOUND` when `app_dir` does not exist, `INVALID_ARGUMENT` for an
unknown platform, `SIMULATION_FAILED` when the simulation itself fails.

### `validate_migration` — re-check a converted project (no `cargo check`)

```jsonc
// input
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "properties": {
    "original_app_dir": {
      "default": null,
      "description": "Directory of the original Electron app, when available: the validator re-scans it and lists MANUAL findings as remaining manual work.",
      "type": ["string", "null"]
    },
    "project_dir": {
      "description": "Directory of the converted Tauri project to validate.",
      "type": "string"
    }
  },
  "required": ["project_dir"],
  "type": "object"
}
```

A static check of a converted Tauri project — it does **not** run `cargo check`
(too slow for an MCP tool). A directory counts as an e2r project when it has
`src-tauri/tauri.conf.json` plus at least one e2r marker
(`.e2r/decisions.jsonl`, `MIGRATION-NOTES.md`, or `src/lib/e2r-bridge.ts`).

```jsonc
// output (ValidationReport)
{
  "project_dir": "my-app-tauri",
  "is_e2r_project": true,   // always true on success — a non-e2r project is an error
  "carried_over": [         // expected generated files that exist, in check order
    "src-tauri/src/main.rs", "src-tauri/src/lib.rs", /* ... */
  ],
  "missing": [],            // expected generated files that are missing
  "renderer_carried_over": true,
  "renderer_detail": "build output carried over (dist/index.html exists)",
  "todo_stubs": 3,          // total TODO(e2r) occurrences in the generated Rust + TypeScript
  "todo_stub_files": [      // per-file stub counts (files with at least one stub)
    { "file": "src-tauri/src/commands.rs", "count": 3 }
  ],
  "manual_remaining": [     // MANUAL findings from the original app (only when
    { "api": "session", "file": "main.js", "line": 9, "note": "..." }
  ],
  "next_steps": [           // most urgent first; always ends with the cargo check
    "Port the 3 TODO(e2r) stub(s) in src-tauri/src/ ...",
    "Run `cargo check` in my-app-tauri/src-tauri to verify the generated Rust compiles",
    "Run the frontend build (if any), then `cargo tauri dev` to launch the converted app"
  ]
}
```

The 11 expected files, in check order: `src-tauri/src/main.rs`,
`src-tauri/src/lib.rs`, `src-tauri/src/commands.rs`,
`src-tauri/tauri.conf.json`, `src-tauri/capabilities/default.json`,
`src-tauri/Cargo.toml`, `src-tauri/build.rs`, `src-tauri/icons/icon.png`,
`src/lib/e2r-bridge.ts`, `package.json`, `MIGRATION-NOTES.md`. `TODO(e2r)`
stubs are counted in the generated Rust sources plus the bridge
(`src-tauri/src/{main,lib,commands,listeners,menu}.rs`,
`src/lib/e2r-bridge.ts`); an unreadable file counts as zero stubs rather than
failing the validation.

`next_steps` always ends with the `cargo check` and `cargo tauri dev` commands:
validation is a static check, not proof the migration works. Never claim a
migration succeeded without a `cargo check` (or `cargo tauri dev`) actually
passing.

Errors: `NOT_FOUND` when a directory does not exist, `NOT_E2R_PROJECT` when
the directory is not an e2r-generated project, `SCAN_FAILED` when the original
app cannot be re-scanned.

### `get_decision_log` — read the decision registry

```jsonc
// input
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "properties": {
    "project_dir": {
      "default": null,
      "description": "Directory of the converted project whose decision log to read.\nOmit to read the shared log (`~/.cache/e2r/decisions.jsonl`).",
      "type": ["string", "null"]
    }
  },
  "type": "object"
}
```

Returns the decision registry (see [The decision log](#the-decision-log)):
entries in file order (oldest first), plus the count and the number of corrupt
lines skipped:

```json
{
  "path": "my-app-tauri/.e2r/decisions.jsonl",
  "count": 12,
  "skipped": 0,
  "entries": [ /* DecisionEntry objects, oldest first */ ]
}
```

A missing log reads as an empty list, not an error. Errors: `NOT_FOUND` when
the given `project_dir` does not exist.

## The decision log

Every conversion records one decision per Electron API finding it acted on, in
an append-only JSONL file — the audit trail for a migration ("why did this
become that?"). Format **v1**; one JSON object per line, only ever appended to,
never rewritten:

```json
{
  "version": 1,
  "timestamp": "2026-11-25T06:30:00Z",
  "electron_api": "dialog.showOpenDialog",
  "tauri_replacement": "tauri-plugin-dialog open()",
  "classification": "auto",
  "rationale": "Direct Tauri equivalent exists; conversion is mechanical.",
  "confidence": "high",
  "source": "mappings-db"
}
```

| Field | Meaning |
| --- | --- |
| `version` | Entry format version. Always `1` in this release. |
| `timestamp` | When the decision was recorded (RFC 3339 UTC, `...Z`). |
| `electron_api` | Electron API name, e.g. `"dialog.showOpenDialog"`. |
| `tauri_replacement` | The Tauri replacement chosen, or `null` when no Tauri equivalent exists (manual ports). |
| `classification` | `"auto"`, `"assisted"`, or `"manual"`. |
| `rationale` | Why this replacement was chosen, in one line (the scanner's note for the finding; a classification-derived fallback when the finding has no note). |
| `confidence` | `"high"` (auto), `"medium"` (assisted), or `"low"` (manual) — follows the classification. |
| `source` | `"mappings-db"` when the mappings table has a row for the API, `"rule"` when a scanner/converter rule decided it, `"human-override"` reserved for a future override tool (no writer produces it yet). |

**Paths.** `convert` writes to `<out-dir>/.e2r/decisions.jsonl`; `get_decision_log`
reads `<project-dir>/.e2r/decisions.jsonl` when a project directory is given.
Otherwise the shared log is used: `E2R_DECISIONS_LOG` when set and non-empty,
else `~/.cache/e2r/decisions.jsonl` (when `HOME` is unset, `.e2r/decisions.jsonl`
under the current directory).

**Tolerant reader.** A corrupt line is skipped on read and counted in `skipped`
(blank lines don't count); a missing file reads as an empty log. A damaged log
never breaks the tools that consume it. Writing the log must never fail a
conversion: on a write error `convert` records a warning in its report and
continues. The log is a metadata sidecar — it is deliberately not listed in
`convert`'s `files_written`.

## What the server does NOT do

- **No conversion over MCP.** There is no `convert` tool. The server plans,
  looks up, simulates, and validates; the agent runs `e2r convert <app-dir>
  --out <dir>` in a shell, after the user has seen the analysis, the simulation,
  and the plan and said go.
- **No `cargo check` inside `validate_migration`.** It is a static check
  (expected files, renderer carry-over, stubs, manual work). Proving the project
  builds is a shell step, and `next_steps` always ends by saying so.
- **No calibration over MCP.** `e2r calibrate` (compare predictions against
  measured actuals, feed the discrepancies back into the model's confidence
  intervals) is a CLI step; there is no MCP tool for it yet. The tools that
  simulate do honour the calibration log, like the CLI.
- **No decisions are written over MCP.** The server only *reads* the decision
  registry (`get_decision_log`); entries are written by `convert`. A
  human-override tool is a future addition, not a shipped one.
- **No network, no telemetry.** Like the CLI, the server runs entirely on the
  local machine; code never leaves the disk.

## Architecture

Server design (handler/transport split, why rmcp, the tokio runtime confined to
the `serve` arm, the decision-registry design, test strategy) is documented in
[IMPLEMENTATION.md](IMPLEMENTATION.md#11-mcp-server--shipped-phase-3).
