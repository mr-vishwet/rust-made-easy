# Contributing to Rust Made Easy

Thanks for helping. Bug reports, fixes, new API mappings, fixtures from real Electron apps, and
documentation improvements are all welcome. You don't need to ask before opening a pull request
for a bug fix or a new mapping.

This repo is the umbrella for the Anything-to-Rust vision. The first converter — Electron →
Rust (`e2r`) — lives under `converters/electron/`; most contributions today land there.

## Before you start

- **A bug:** open an [issue](https://github.com/mr-vishwet/rust-made-easy/issues/new) with the
  command you ran, its output, your platform, and the `e2r` version (`e2r --version`). A tiny
  Electron app that reproduces it helps most.
- **A bigger change** (a new command, a new codegen target, a change to what a mapping or a
  feasibility score means): open an issue first so we can agree on the shape before you write it.
  The reasoning behind past choices lives in [docs/RND.md](./docs/RND.md) and
  [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md).
- **A security problem:** don't open a public issue; see [SECURITY.md](./SECURITY.md).

## Set up

You need Rust (stable, 1.85 or newer). The tests also build the generated Tauri code, so a
working `cargo` with network access for the first `cargo test` run (to fetch Tauri crates) is
assumed; the golden-fixture check runs `cargo check` on the generated project. That check takes
a few minutes on the first run (it compiles Tauri and its plugins).

On Linux, the golden check also needs Tauri's system libraries (GTK + WebKit). On
Debian/Ubuntu:

```sh
sudo apt-get update
sudo apt-get install -y libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Without them the check fails in the `gdk-sys` / `webkit2gtk-sys` build scripts — not in
generated code. See <https://v2.tauri.app/start/prerequisites/> for other platforms. (CI
installs these before running the tests.)

```sh
git clone https://github.com/mr-vishwet/rust-made-easy
cd rust-made-easy
cargo test
```

Before every commit, run:

```sh
cargo fmt
cargo clippy -- -D warnings
cargo test
```

CI runs the same three checks on every push and pull request (see
[.github/workflows/ci.yml](./.github/workflows/ci.yml)). `cargo fmt --check` and
`cargo clippy -- -D warnings` must both pass clean.

## Make the change

- **Every fix comes with a test that fails without it.** Run the test against the old code once to
  be sure it does.
- **Keep the generated code compiling.** If you touch codegen, the golden-fixture test must still
  pass `cargo check` on the generated Tauri project. A mapping that generates code that doesn't
  compile is worse than no mapping.
- **Keep the docs in sync.** The API map, the architecture doc, and the mappings file describe the
  same reality; a change to one that contradicts another will be sent back.

## Code style

- `cargo fmt` is the arbiter. Don't hand-format.
- Clippy warnings are errors in CI (`-D warnings`), so fix them rather than allowing them. If a
  lint is genuinely wrong for a case, add a targeted `#[allow(...)]` with a comment saying why.
- Prefer small, named functions over clever ones. The codegen modules are read by contributors who
  are learning the Electron API surface, not just by Rust developers.
- Error messages name the file, the API, and what the user can do next. Never a bare `unwrap()` on
  user input or on a file the user supplied.
- No new dependencies without a note in the PR explaining why the standard library or an existing
  dependency can't do it. This tool is installed with `cargo install`; its dependency tree is part
  of its install time.

## Adding a new API mapping

This is the most valuable small contribution. Most Electron APIs map to a Tauri plugin or a few
lines of Rust, and each one you add makes the next conversion more automatic.

1. Add an entry to `converters/electron/mappings/electron-to-tauri.json`: the Electron API name, the Tauri target, the
   plugin or crate (if any), the status (`AUTO`, `ASSISTED`, or `MANUAL`), and a short note.
2. If the status is `AUTO`, add the codegen for it in `converters/electron/crates/e2r-cli` (see
   [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) for where each mapping's codegen lives) and a
   test that the generated code compiles.
3. Add a row to [docs/API-MAP.md](./docs/API-MAP.md) mirroring the JSON entry.
4. Add the API to a fixture (below) so the scan test covers it.

Statuses mean:

- **AUTO** — converts mechanically, no human decision needed.
- **ASSISTED** — converts, but a human must confirm a choice (a permission, a window option, a
  fallback). The generated `MIGRATION-NOTES.md` must name the choice.
- **MANUAL** — no mechanical conversion exists yet. The entry should name the Rust replacement or
  the reason it can't convert, so the user gets guidance instead of silence.

## Adding a fixture

Fixtures are small, real Electron apps (or realistic excerpts) that the tests scan and convert.
To add one:

1. Create `converters/electron/fixtures/<name>/` with a minimal `package.json` and the smallest `main.js` /
   `preload.js` that exercises the API you care about.
2. Add the expected scan output (or the assertions on it) in the test suite, and the expected
   conversion result — at minimum, that the generated project passes `cargo check`.
3. Keep fixtures small and dependency-free where possible. They run in CI on every push.

If your fixture comes from a real open-source app, say which one in the fixture's README and keep
its license notice if it has one.

## Good first issues

Look for the `good first issue` label. The usual shape:

- **A new mapping** for one Electron API (see above) — self-contained, well-specified, and
  immediately useful.
- **A fixture** for an API the scan already knows but no fixture exercises.
- **A docs fix** — a stale row in the API map, a broken link, a README example that drifted from
  the CLI's actual output.
- **A scan-pattern fix** — the v0.1 scanner is regex-based (see
  [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md)); a false positive or false negative on a real
  app's code is a good, bounded fix.

## Pull request process

1. Fork, branch from `main`, keep the change focused — one mapping, one bug, one fixture per PR
   where you can.
2. Run `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo test` before pushing.
3. Add a line to [CHANGELOG.md](./CHANGELOG.md) under `[Unreleased]`, in the right section.
4. Open the PR against `main` with a short description of what changed and why. Link the issue if
   there is one.
5. Expect review within a few days. Small, well-tested PRs merge fastest.

## Code of conduct

Be kind, assume good intent, criticize code and ideas rather than people. The full text is in
[CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md).
