# Converters

One converter per source platform. Each lives in its own directory with its own
crate(s), mappings, fixtures, and README. The shared vision, research, and
architecture docs live at the repo root (`docs/`, `PRD.md`).

| # | Converter | Status | What it does |
| --- | --- | --- | --- |
| 1 | [electron/](electron/) — `electron-to-rust` (`e2r`) | **v0.1 — shipped** | Convert Electron desktop apps to Rust (Tauri 2): `e2r scan` X-rays an app (APIs, IPC channels, feasibility score); `e2r convert` generates a working Tauri 2 project. |
| 2 | `android/` | **Planned** | Convert Android apps to Rust. The Intermediate Representation (see `docs/IMPLEMENTATION.md`) is what makes a second source possible without a second rewrite of the Brain. |

## Adding a converter

A new converter is a new directory under `converters/` with:

- `README.md` — what it converts, quickstart, honest coverage table.
- `crates/` — the converter's Rust crates (CLI + library).
- `mappings/` — the source-to-Rust compatibility database (the deterministic heart;
  codegen reads it, never hardcoded translations).
- `fixtures/` — small real apps the tests scan and convert, with the generated output
  proven to compile.

See `electron/` for the reference layout, and `docs/ARCHITECTURE.md` for where shared
pieces (the IR, the simulator, the MCP server, the companion) belong as they arrive.
