# e2r agent skill + MCP configs

The e2r MCP server (`e2r serve`) exposes the
[rust-made-easy](https://github.com/mr-vishwet/rust-made-easy) Migration Brain
to AI coding agents over the Model Context Protocol (stdio transport, MCP
protocol version 2025-03-26 or later). The agent does the coding; e2r is the
brain — it analyses an Electron app, simulates the migration, plans it
deterministically, and validates the result.

## What's in this directory

| File | What it is |
| --- | --- |
| `SKILL.md` | The agent skill: when to use the server, the canonical analyse → simulate → plan → convert → validate workflow, the six tools, the mandatory honesty rules, troubleshooting. Point your agent at it, or paste it into your agent's skill/instructions file. |
| `claude-code.mcp.json` | Claude Code MCP config — copy to `.mcp.json` in your project root, or merge the `e2r` entry into `~/.claude.json`. |
| `codex-config.toml` | Codex MCP config — merge into `~/.codex/config.toml`. |
| `opencode.json` | OpenCode v2 MCP config — merge the `mcp` object into `opencode.json` (project root) or `~/.config/opencode/opencode.json` (global). |
| `smoke-test.sh` | Proves the server answers, with no agent involved (see below). |

Each config file carries a header comment (a `_comment` key in the JSON
files, `#` comments in the TOML) saying where it goes.

## Install

```sh
# 1. Install e2r (from a clone of rust-made-easy)
git clone https://github.com/mr-vishwet/rust-made-easy
cd rust-made-easy
cargo install --path converters/electron/crates/e2r-cli

# 2. Confirm it's on PATH
e2r --help

# 3. Drop the right snippet into your agent's config (see the table above)
#    — the command must resolve on PATH, or use the absolute path to the
#    cargo-installed binary (usually ~/.cargo/bin/e2r) instead of "e2r".

# 4. Restart the agent so it picks up the new MCP server.

# 5. Verify the server answers (no agent needed):
./skills/e2r/smoke-test.sh
```

## The six tools

| Tool | What it does |
| --- | --- |
| `analyse_electron_app` | Scan an Electron app: feasibility score, findings (AUTO/ASSISTED/MANUAL), IPC channels, windows, menus, preload shape. |
| `get_migration_plan` | Ordered migration plan: steps, per-API replacements, MANUAL items first, effort hint. |
| `get_rust_replacement` | Look up one Electron API in the 48-row mappings DB: Tauri equivalent + status (`auto`/`assisted`/`manual`) + plugin + notes. |
| `simulate_migration` | Pre-migration simulation: migratable breakdown, UI-preservation estimate, before→after predictions with ranges + confidence + basis. |
| `validate_migration` | Static check of a converted Tauri project: carried over, missing, `TODO(e2r)` stubs remaining, remaining manual work. Does **not** run `cargo check`. |
| `get_decision_log` | The append-only decision registry (`.e2r/decisions.jsonl` in a project, else `~/.cache/e2r/decisions.jsonl`). |

## Smoke test

`smoke-test.sh` starts `e2r serve`, sends `initialize`,
`notifications/initialized`, `tools/list`, and one `tools/call`
(`analyse_electron_app` on the repo's minimal fixture,
`converters/electron/fixtures/minimal-electron-app`), prints PASS/FAIL per
step, and exits non-zero on any failure. It needs only bash + python3 (no
jq), and times out instead of hanging forever if the server stops responding.

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

Environment overrides: `E2R_BIN` (path to the binary; default `e2r` on PATH,
falling back to `target/debug/e2r` then `target/release/e2r` in the repo),
`E2R_FIXTURE` (app dir for the `tools/call` step),
`E2R_TIMEOUT` (seconds for the whole conversation; default 30).

## Honesty rules (read before quoting any number)

The skill's honesty rules are mandatory, not advisory — the short version:

- Never present the planning chat's sample figures (86/100 readiness,
  82%/18%/94%, startup 2.1s→1.7s, memory 380→265 MB, CPU 22%→9%, binary
  120→28 MB) as measurements. They are not measurements.
- Only quote numbers from `scan` / `simulate` output, with their stated
  confidence.
- Never claim a migration succeeded without a `cargo check` (or
  `cargo tauri dev`) actually passing — `validate_migration` is a static
  check, not proof.
- If the simulator refuses a metric (Linux memory, CPU — "unknown, no
  reliable baseline"), say unknown. Never invent a number.
- Never claim the renderer was carried over without checking
  `validate_migration`'s `carried_over` list.

Full rules: [SKILL.md](SKILL.md#honesty-rules-mandatory).
