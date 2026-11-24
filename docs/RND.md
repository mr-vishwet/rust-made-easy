# R&D: Why Electron-to-Rust, Why Now

The research behind `electron-to-rust`: the market moment, the prior art, the gap this tool
fills, the architecture it is built on, and — honestly — the hard parts. Every external claim
here traces to a source in [Sources](#sources); anything we could not verify is marked
**unverified**.

---

## 1. Executive summary

Electron won the desktop by making it cheap to ship a cross-platform app: write a website, wrap
it in Chromium, ship it. The price was always visible — a whole browser engine per app, bundles
of 85–160 MB and up, memory measured in gigabytes — but for a decade it was a price teams were
willing to pay, because the alternative was maintaining three native apps.

That calculation has flipped. On November 4, 2026, Discord — arguably the largest and most
visible Electron app in the world — confirmed it is rebuilding its desktop client in Rust and
dropping Electron entirely. Discord is not a startup making a fashion choice; it is a company
that has used Rust since 2018 and is now finishing the job. The announcement landed in a market
already sensitive to memory costs, and it made the question every Electron team was quietly
asking into a public one: *when do we leave, and how?*

The "how" is the gap. There is, as of November 2026, no shipped automatic converter from
Electron to Rust. Teams today either rewrite by hand (months of work, high risk), hire a
migration agency (expensive, bespoke), or stay put. The rewrites that have shipped — 1Password
8, Firefox's Oxidation, Slack's piece-by-piece replacement — all prove the same pattern: the
migration is a long, staged, one-component-at-a-time job, and the tooling for it is what nobody
has built.

`electron-to-rust` is that tooling. It is a CLI, `e2r`, that:

1. **Scans** an Electron app and produces an X-ray: which Electron APIs it uses, which IPC
   channels exist, and a feasibility score with every API classified AUTO / ASSISTED / MANUAL.
2. **Converts** it into a working Tauri 2 project — Rust backend, system webview, your renderer
   unchanged — with a TypeScript bridge replacing `ipcRenderer` and a `MIGRATION-NOTES.md` that
   says exactly what is left for a human.
3. **Supports the staged migration** the prior art proves is the only sane way to do this: run
   in a Tauri shell with a Node sidecar on day one, port IPC handlers to Rust one by one with
   record/replay parity checks, remove the sidecar last.

The target is Tauri 2, not a native-UI framework: it keeps the web renderer, which is where
most Electron apps hold most of their value, and it is the smallest step that still removes
Chromium from the bundle. A native-UI target (GPUI-style) is a later phase, not the first one.

This document is the reasoning. The tool is the bet: that the Electron-to-Rust migration is a
mechanical problem with a long tail of judgment calls, and that a tool which does the mechanical
part and names the judgment calls honestly is worth building.

---

## 2. Market timing: the Discord moment

### 2.1 What happened

On **November 4, 2026**, Discord co-founder and CTO **Stanislav Vishnevskiy** confirmed on X
that Discord is rebuilding its desktop client in Rust. The announcement, as reported by The
Verge (November 5, 2026), Windows Latest, and others:

- **Electron goes away.** The desktop client is being rebuilt as a native Rust application.
- **React Native goes away too**, on mobile. Mobile keeps a native interface but shares app code
  with desktop through Rust.
- **The UI is GPUI, not Tauri.** Vishnevskiy said it directly: *"it's not Tauri, it's GPUI."*
  GPUI is the GPU-accelerated Rust UI framework built by the team behind the Zed editor. Discord
  is not swapping one webview for a lighter one; it is dropping the browser entirely.
- **Codename "Discord Phoenix."** The name appeared in a Task Manager screenshot of an early
  Windows 11 build, showing **290.2 MB of RAM**.
- **No release date.** Discord says it hopes to get the new client into user testing soon. The
  app people use today still runs on Electron.
- **The new client is reported to look and work identically to the current one**, and the
  screenshot's 290.2 MB figure was with **two accounts signed in at once**, per Vishnevskiy's
  follow-up replies.

The quote that frames the whole move:

> "We've been early adopters of Rust since 2018 and a lot of our infra, voice tech, and even
> some of the current client is already Rust, the holy grail was getting the whole client
> there."

Discord's Rust history is long: Rust in infrastructure and voice since 2018, and the Read
States service rewritten from Go to Rust in 2020. The desktop rebuild is the end of an
eight-year road, not a sudden pivot.

### 2.2 What the 290 MB number does and does not prove

The 290.2 MB figure is doing a lot of work in the headlines, so it deserves care:

- It is **one screenshot** of an early build, with no stated workload. Nobody outside Discord
  has measured the new client.
- The comparison is loose. Windows Latest's own testing put the current Electron client at
  1–2 GB when idle, with spikes up to 4 GB; other outlets report the current client climbing
  past 700 MB once several servers and voice channels are active. None of these were measured
  under the same conditions as the screenshot, so no like-for-like ratio exists.
- The early build almost certainly has fewer features wired up, and memory use may rise as
  features are added.
- The two-accounts detail cuts the other way: the screenshot's figure covers more concurrent
  state than a single-account idle measurement would.

What the number *does* prove is narrower and still significant: a full-featured chat client,
written natively in Rust, can sit at a few hundred megabytes where its Electron predecessor
sits at a gigabyte or more. The order of magnitude is the story, not the decimal.

### 2.3 Why Electron's cost is now visible

Three things changed at once:

1. **The proof case arrived.** Discord is the app Electron skeptics always pointed to and the
   app Electron defenders always cited as "it works at scale." When the biggest success story
   leaves, the default stops being automatic.
2. **Memory is expensive again.** High memory prices have put RAM-hungry software under closer
   scrutiny (noted in contemporary coverage of the Discord move). An app that holds a gigabyte
   idle used to be an embarrassment; now it is a line item users notice.
3. **The alternatives matured.** Tauri 2 shipped with mobile support from the same codebase,
   a plugin system covering the common desktop surface (filesystem, dialogs, store, updater,
   tray, menu, notifications), and a real ecosystem. The gap between "Electron works" and
   "the alternative works" closed before Discord's announcement; the announcement just made
   people look.

None of this means Electron is dead tomorrow. Discord itself shipped an Electron 42 upgrade in
its September 8, 2026 patch notes — two months before announcing the rebuild — and the current
client will be the only one available until testing opens. The migration wave will take years.
That is the opportunity: years of teams asking *how*, with no tool answering them.

### 2.4 Why Tauri, not GPUI, is the right first target

Discord chose GPUI because Discord can afford to rebuild its entire UI natively and wants the
browser gone completely. Most Electron teams cannot, and should not try:

- **The renderer is most of the app.** For a typical Electron app, the UI — the HTML, CSS, and
  JavaScript — is the majority of the code and nearly all of the user-visible behavior. Tauri
  keeps it verbatim inside the system webview. A GPUI port rewrites it from scratch.
- **The migration is therefore a shell migration, not a rewrite.** The main process (window
  management, IPC, filesystem, menus, tray) becomes Rust; the renderer stays web. That is a
  tractable, automatable job. A full UI rewrite is not.
- **Tauri 2 is the mature option.** System webview (WebView2 on Windows, WKWebView on macOS,
  WebKitGTK on Linux), a Rust backend, plugins for the common surface, and mobile from the same
  codebase since v2. Typical bundles are 3–15 MB versus Electron's 85–160 MB+ (widely cited
  ranges, not our measurement).
- **GPUI stays on the roadmap.** Phase 4 adds a native-UI target option for teams that want to
  follow Discord all the way. But the first target is the one that gets teams out of Electron
  with their UI intact.

---

## 3. Prior art: what each migration proves

Every serious Electron-to-Rust (or Electron-to-anything) migration has been a hand-built,
staged effort. The table below is the evidence base for this project's design: each row is a
real migration, what it did, and what it proves for a tool builder.

| Project | What happened | What it proves |
| --- | --- | --- |
| **Discord** (2026–) | Rebuilding the desktop client in Rust with GPUI; Electron and React Native dropped; early "Phoenix" build ~290 MB RAM in one screenshot; no release date. | The largest Electron app has decided the browser shell is over. Also: the full native-UI route is a multi-year, well-staffed effort — not the default path for everyone. |
| **1Password 8** | Rewrote the desktop apps on a shared Rust core, keeping the web UI. | The hybrid model works at scale: Rust core + web UI is a proven architecture, and it is exactly the shape Tauri gives you. |
| **Bitwarden desktop** | Rust only at the OS seam: keychain access, biometrics. | You don't have to port everything to get Rust's benefits. A staged migration that ports the sensitive seams first is a legitimate end state, not a failure. |
| **Firefox Oxidation** | Replaced components one at a time (CSS engine, media stack, URL parser) over years, inside a working browser. | The one-component-at-a-time pattern is the only one that has ever worked at scale. Big-bang rewrites don't ship; staged ports do. |
| **Slack** (2017–19) | Kept Electron, replaced piece by piece (notably the desktop client rewrite that kept the shell). | Even staying on Electron, the winning move was incremental replacement. The lesson transfers: migrate in stages, keep the app working at every step. |
| **Microsoft Teams 2.0** | Rebuilt from Electron to WebView2 (Edge's webview) instead of a full native rewrite. Microsoft claims 2x speed and half the memory (vendor claim, not independently verified here). | The webview-swap alone — keeping the web UI, dropping the bundled Chromium — captures much of the win. This is the Tauri thesis, validated by Microsoft's own rebuild. |
| **GitButler** | Moving its *next* client back to Electron 44 from Tauri, citing WebKit text-input bugs and Linux packaging pain. | Tauri has sharp edges, and they are real: WebKitGTK behavior on Linux and packaging friction can be deal-breakers for some apps. Any honest migration tool must surface these up front, not discover them mid-migration. |
| **SideX** (Sidenai) | VS Code rebuilt on Tauri: Electron main → Tauri Rust backend; BrowserWindow → WebviewWindow; ipcMain/ipcRenderer → invoke() + events; Node fs/pty → Rust commands (std::fs, portable-pty); Menu/Dialog/Clipboard → Tauri plugins; renderer unchanged in the webview; extension host as a sidecar (in progress). They claim "96% smaller" (their claim, attributed). | The mapping from Electron to Tauri is known and demonstrated on the hardest possible app. VS Code is the proof that the API map this tool encodes is real, not aspirational. |
| **agent-pane** (copse-dev) | Tauri + Servo migration plan: shim the `electron` module and run the whole main process as a Node sidecar, IPC over a loopback WebSocket; a sandboxed preload (pure contextBridge + ipcRenderer) ports verbatim by rebinding the transport. | The sidecar-shim stage works: an app can run in its new shell on day one with zero logic ported. This is Stage 1 of our migration ladder, and the 3-shim architecture is a proven pattern, not an invention. |
| **rust-anything** (erkamyaman) | Records what the original app does, ports it to Rust piece by piece, and checks every piece against the recording (record / bridge / diff / lift). Ports logic, not the shell. | Record/replay parity checking is the right verification model for a staged port — and rust-anything explicitly defers the desktop-shell step. That deferred step is this project. |

The pattern across all ten rows is consistent: **nobody does this in one jump, everybody keeps
something working at every step, and the tooling for the mechanical parts has never been built.**
That is the product.

### 3.1 Reference repos: small, real apps (verified)

Three small repositories cited in the planning chat were checked on GitHub (November 2026).
They are not migrations themselves, but they are the shape of the apps this tool must handle —
and the shape of the Tauri projects it must generate.

| Repo | What it is | Why it matters here |
| --- | --- | --- |
| [codeglocks/electro-dash](https://github.com/codeglocks/electro-dash) | Electron + React dashboard template with Material-UI (9 stars, 2020). `main.js` + webpack build. | A realistic Electron *source* app: the kind of `main.js` + React renderer the scanner must read and the converter must handle. A good fixture candidate. |
| [mbenja/tauri-todo-app](https://github.com/mbenja/tauri-todo-app) | Tauri + React + TypeScript to-do app with Prisma + SQLite, native menu and shortcuts (26 stars, 2022). | A realistic Tauri *target* app: shows the shape of generated output — Rust commands, a local database behind them, menus and shortcuts wired through Tauri. |
| [hschneider/tauri-quasar](https://github.com/hschneider/tauri-quasar) | Tauri 2 + Quasar + Vue 3 starter app, multi-window support, Linux WhiteScreen fix (6 stars, 2025). | Proof that Tauri 2 targets are not React-only: the generator must preserve whatever frontend framework the source app uses, including Vue/Quasar. |

---

## 4. Vision: the Migration Brain

The v0.1 CLI — `scan` and `convert` — is the first step of a larger product: a migration
intelligence layer, the **Migration Brain**. The full vision is specified in
[PRD.md](../PRD.md) and [IMPLEMENTATION.md](IMPLEMENTATION.md); this section summarizes the
parts that change how the research should be read.

### 4.1 The central principle

**The AI coding agent does the coding; our plugin provides the migration intelligence, the
rules, the checks, and the tools.** The developer should not need to be an expert in Electron
internals or in Rust. The goal is full conversion into Rust — UI, business logic, and system
integration — not just re-packaging.

Put another way: *the tool is not the agent, not the editor, not the compiler. It is the brain.
The agent is the hands.*

The Brain has six parts:

1. **It understands before it writes.** The repository is analyzed first — framework,
   structure, Electron APIs, Node dependencies, bottlenecks — and the migration plan follows
   from that understanding, not from a template.
2. **It is deterministic.** Agents never guess. Every Electron API has an exact, recorded
   replacement: `ipcMain.handle('read-file')` becomes
   `#[tauri::command] fn read_file()`, `ipcRenderer.invoke('read-file')` becomes
   `invoke('read_file')`. The mappings file is the law; the agent applies it.
3. **It simulates before it migrates.** The pre-migration simulator predicts the migratable
   percentage, how much UI is preserved, and the expected performance gains — with confidence
   ranges — before a single line of code changes. This is the killer feature: a team can see
   the shape and the payoff of the migration before committing to it.
4. **It remembers its decisions.** A decision registry records every migration decision with
   its reason, so agents don't repeat mistakes across sessions or across apps.
5. **It validates.** Runtime simulation, API coverage checks, UI regression detection,
   end-to-end tests, and benchmarks compare the migrated app against the original.
6. **It is agent-agnostic.** The Brain plugs into OpenCode, Claude Code, Codex, Antigravity,
   and VS Code IDEs via MCP servers, agent skills, IDE extensions, and the CLI — the same
   intelligence, whichever hands are doing the coding.

### 4.2 The differentiators

Three pieces of the vision are what make this more than a converter:

- **The Intermediate Representation (IR).** The engine does not do naive source-to-source
  conversion. It builds an IR of the app — its windows, IPC channels, APIs, dependencies, and
  data flows — and reasons about semantics. The IR is what lets the same engine later support
  other frameworks as sources and other targets (native UI, Android apps) without starting
  over. See [IMPLEMENTATION.md](IMPLEMENTATION.md).
- **The Performance Prediction Engine.** A first-class differentiator: predict memory, CPU,
  startup-time, and bundle-size gains with confidence ranges *before* migration, measure the
  actuals after, and learn from the discrepancies. Every migration makes the next prediction
  better. (Any numbers shown in planning materials are illustrative targets, not measurements —
  see the honesty note in [PRD.md](../PRD.md).)
- **The Live Migration Companion.** A signature feature: the plugin launches its own window
  alongside the coding agent, showing live progress — files analyzed, APIs replaced, Rust
  generated, simulation results, warnings, validation. Three modes: Simulation (before), Live
  migration (during), Verification (after, original vs. migrated). The companion is separate
  from the agent; the core publishes structured events over local IPC/WebSocket and the
  companion (itself a Tauri + Rust app) subscribes. Two forms — a floating overlay and a full
  workspace app — plus Migration Replay, a timeline of everything for debugging and auditing.

### 4.3 Two modes, one promise

- **UI-preserving mode (primary):** Rust backend + WebView frontend. The renderer is kept
  verbatim; only the shell and the IPC layer change. This is what v0.1 ships.
- **Full native mode:** Rust UI via egui, Iced, Dioxus, or Slint — for teams that want the
  browser gone entirely, the Discord route. A later phase.

The strongest promise, across both modes: **zero UI regression**. The migrated app must look
and behave like the original. Validation exists to prove that, not to assert it.

---

## 5. The gap: no shipped automatic converter

As of November 2026, **no shipped automatic Electron-to-Rust converter exists**. The Snowinch
desk study puts it bluntly:

> "a shipped automatic translator from Electron, as of today, does not exist"

— only an attempt announced in a VS Code issue. The evidence supports the claim: the
migrations in section 3 were all hand-built; the tooling around them is frameworks (Tauri,
Wails, Electrobun) for *new* apps, not converters for *existing* ones; and the closest
adjacent tool, rust-anything, ports application logic and explicitly defers the desktop shell.

That is the product opportunity, stated plainly: the biggest migration wave in desktop
software history is beginning, every team facing it has the same mechanical work to do, and
nobody has shipped the tool that does the mechanical work.

The gap is also a scope warning. A converter that claims to do everything will do nothing well.
This project's answer is the classification system — AUTO / ASSISTED / MANUAL — and the
migration ladder: automate what is mechanical, name what needs judgment, and never pretend the
hard parts don't exist. Section 8 lists them.

---

## 6. Architecture of electron-to-rust

The v0.1 architecture is a four-stage pipeline. The full engineering design, including the
planned IR and the later phases, is in [ARCHITECTURE.md](ARCHITECTURE.md) and
[IMPLEMENTATION.md](IMPLEMENTATION.md).

```
Electron app ──► SCAN ──► MAP ──► GENERATE ──► VERIFY ──► Tauri 2 project
                 (X-ray)  (data-  (codegen)   (cargo check
                          driven)              + parity tests)
```

1. **Scan (X-ray).** Read the app's `package.json`, main-process files, preload scripts, and
   renderer entry points. Detect the frontend framework, enumerate Electron APIs used and IPC
   channels, list Node dependencies (especially native modules), and produce a feasibility
   score with per-API AUTO / ASSISTED / MANUAL classification. v0.1 scanning is regex-based;
   parser-based analysis arrives with the IR in a later phase.
2. **Map (data-driven).** Every detected API is resolved through `converters/electron/mappings/electron-to-tauri.json`,
   the compatibility database. The mapping — not handwritten code — decides what the Tauri
   target is, which plugin or crate it needs, and what status it carries. New mappings are a
   data contribution, not a code change (see [CONTRIBUTING.md](../CONTRIBUTING.md)).
3. **Generate (Tauri 2 project).** Emit `src-tauri/` (`Cargo.toml`, `tauri.conf.json`,
   `lib.rs`, `commands.rs`, `main.rs`, `build.rs`, a placeholder icon), the TypeScript bridge
   (`src/lib/e2r-bridge.ts`), a placeholder `dist/index.html`, and `MIGRATION-NOTES.md`. One Rust
   command per `ipcMain` handler; plugins included only for the APIs the app actually uses.
   Note: v0.1 does **not** copy the renderer — the developer copies their frontend into the
   generated project (or points `frontendDist` at their build output) as the migration notes
   explain; automatic renderer copying is planned.
4. **Verify.** `cargo check` on the generated project (in CI, against the golden fixture, on
   every change). Record/replay parity checks — run the original and the migrated app through
   the same cases and diff the behavior — arrive in phase 2, in the rust-anything style.

The design constraints that matter most: **the tool never modifies the source app** (output
goes to a separate `--out` directory); **the generated code must compile** (a mapping that
generates broken code is worse than no mapping); and **the tool never phones home** (it reads
your code and generates code, locally, with no telemetry).

> **Diagram attribution.** The architecture diagrams in this repo — the hero diagram in
> [README.md](../README.md), the ecosystem and Migration Brain diagrams in
> [ARCHITECTURE.md](ARCHITECTURE.md), the pipeline diagram in
> [IMPLEMENTATION.md](IMPLEMENTATION.md), and the live-workflow diagram in
> [PRD.md](../PRD.md) — recreate the architecture diagrams from the original planning
> discussion, redrawn as native Mermaid. They are our own assets; no third-party images
> from that discussion are used.

---

## 7. The 3-stage migration ladder

The prior art is unanimous: nobody migrates in one jump. The ladder below is the supported
path, and the tooling is designed around it. Stage 1 is the pattern proven by agent-pane's
3-shim architecture; Stage 2 uses rust-anything-style record/replay parity checks.

### Stage 1 — Tauri shell + Node sidecar (app runs day one, zero logic ported)

The app runs inside Tauri immediately. The `electron` module is shimmed, and the whole
Electron main process runs as a Node sidecar, with IPC over a loopback channel. A sandboxed
preload — pure `contextBridge` + `ipcRenderer` — ports verbatim by rebinding the transport.
Nothing is ported yet; the win is that the app is already running in its new shell, and the
team can see it working.

### Stage 2 — Port IPC handlers to Rust, one by one (parity-checked)

Each `ipcMain` handler becomes a Rust `#[tauri::command]`, one at a time. Before the sidecar
copy is dropped, record/replay parity checks prove the Rust handler behaves like the original:
record what the original does on real inputs, replay the same inputs against the Rust port,
and diff. A handler ships only when its cases match. The sidecar shrinks handler by handler;
the app works at every step.

### Stage 3 — Pure Rust (sidecar removed)

The last handler is ported, the sidecar is removed, and the app is one binary with no Node
runtime. Bundle size, memory, and startup reflect the full migration.

Teams may stop at any stage — Bitwarden's "Rust only at the OS seam" is a legitimate end state,
and a Stage 1 app that ships is worth more than a Stage 3 app that doesn't. The ladder is a
path, not a purity test.

---

## 8. Honest hard parts

A migration tool that hides the hard parts is a trap. These are the known ones; `scan` flags
them, `MIGRATION-NOTES.md` names them, and the roadmap addresses them in order of pain.

- **Native Node modules.** `node-pty`, `better-sqlite3`, and friends are compiled C/C++ bound
  to Node. There is no mechanical conversion: each needs a Rust replacement
  (`portable-pty`, `rusqlite`) and a port of the code around it. Phase 3 adds tested recipes
  for the common ones; until then they are MANUAL with a named replacement.
- **Chromium-only APIs.** `webContents` quirks, `session` and cookie handling, and the deeper
  parts of the `BrowserWindow` API have no exact Tauri equivalent — the system webview is not
  Chromium, and its API surface is thinner. Some behaviors need redesign, not translation.
- **DRM / Widevine.** Widevine content decryption is tied to Chromium. An app that depends on
  it (streaming, protected media) cannot migrate to a system webview without losing it. This
  is a hard blocker for a small class of apps, and `scan` must say so rather than generate a
  broken app.
- **Deep OS integration.** Global shortcuts have edge cases (registration conflicts,
  platform differences); custom protocol handlers need per-platform registration; deep links,
  file associations, and auto-launch all have platform-specific shapes. Tauri covers the
  common cases; the exotic ones need manual work.
- **WebKit text-input bugs on Linux.** The GitButler lesson: WebKitGTK — the Linux webview —
  has real text-input bugs, and Linux packaging has its own friction. An app whose users are
  mostly on Linux, or whose core interaction is text input, must weigh this before migrating.
  Tauri's sharp edges are part of the feasibility score, not a footnote.
- **Accessibility gaps in non-webview targets.** The webview inherits the platform's
  accessibility support for web content. A full native-UI target (phase 4: egui, Iced,
  Dioxus, Slint) does not get that for free, and each framework's accessibility story is at a
  different stage of maturity. **Unverified** in detail for each framework — the phase 4
  design must assess this per framework before recommending any of them.

## 9. Competitive landscape

| Player | What it is | Relationship to this project |
| --- | --- | --- |
| **rust-anything** (erkamyaman) | Records what an app does, ports it to Rust piece by piece, checks every piece against the recording. Ports logic, not the shell. | **Complementary.** We *are* the Tauri step it defers. Stage 2 of our ladder uses its record/replay model, and the two tools should interoperate, not compete. |
| **Manual migration agencies** | Consultancies that hand-migrate Electron apps to Tauri or native. | The expensive incumbent. A tool that does the mechanical 80% makes agencies faster and makes small teams self-sufficient. Not a competitor to beat — a market to equip. |
| **Tauri CLI `migrate`** | Migrates Tauri 1 projects to Tauri 2. | Adjacent, not overlapping: it is Tauri-to-Tauri, not Electron-to-Tauri. Our generated projects target Tauri 2 directly, so its users are downstream of us, not instead of us. |
| **Electrobun / Wails** | Frameworks for building new desktop apps (Bun + webview; Go + webview). | Frameworks for *new* apps, not converters for *existing* ones. They validate the webview-shell model; they don't migrate anyone's Electron app. |
| **Tauri itself** | The target framework. | Our target and our dependency, not a competitor. The better Tauri's plugin ecosystem gets, the more of the API map becomes AUTO. |
| **Hand-rolled internal tools** | Every large team that migrated (Discord, 1Password, Slack) built its own one-off tooling. | The proof that the tooling is needed — and that nobody has productized it. Those tools were never shipped; this one is. |

The honest summary: the competition is not another converter (none exists). It is inertia —
teams staying on Electron because the migration looks too big — and hand-rolled effort. The
tool wins by making the first step small and the whole path visible.

---

## 10. Sources

Every external claim in this document traces to one of these. Where a source could not be
verified, that is marked.

### Discord's Rust rebuild (November 2026)

- **The Verge, November 5, 2026** — report on Vishnevskiy's announcement and the 290 MB
  screenshot. Cited by name and date; the article URL could not be verified from search
  results, so no link is given rather than a guessed one.
- **Windows Latest, November 6, 2026** — "Every Windows 11 app should kill Electron, and
  Discord just showed why" — the 290.2 MB reading, the two-accounts detail, the cold-start
  comparison clip, and the "it's not Tauri, it's GPUI" follow-up.
  https://www.windowslatest.com/2026/11/06/every-windows-11-app-should-kill-electron-and-discord-just-showed-why-no-starting-screen-and-70-less-ram-even-with-two-accounts-signed-in/
- **DEV Community, f9xrteam** — "Discord Drops Electron for Rust: Should You Follow?" — a
  careful summary of the announcement with the caveats on the 290 MB figure.
  https://dev.to/f9xrteam/discord-drops-electron-for-rust-should-you-follow-473b
- **Stanislav Vishnevskiy on X, November 4, 2026** — the primary announcement ("We've been
  early adopters of Rust since 2018… the holy grail was getting the whole client there").
  Quoted via the coverage above; the post itself was not fetched directly.
- Corroborating coverage: [tech-insider.org](https://tech-insider.org/discord-electron-rust-gpui-rebuild-290mb-ram-2026/),
  [talkesport.com](https://www.talkesport.com/tech/discord-rust-client-phoenix-electron-ram/),
  [hwbusters.com](https://hwbusters.com/news/discord-is-dropping-electron-for-a-rust-client-and-an-early-build-idles-at-290mb-of-ram/),
  [tweaktown.com](https://www.tweaktown.com/news/114511/discord-is-replacing-electron-with-a-native-rust-desktop-app-early-windows-build-uses-just-290mb-of-ram/index.html),
  [wccftech.com](https://wccftech.com/discord-is-ditching-electron-for-rust-with-early-windows-build-using-just-290mb-of-ram/).

### The gap

- **Snowinch** — "Why Electron is heavy: Tauri, React Native alternatives" — the desk study
  concluding that "a shipped automatic translator from Electron, as of today, does not exist,"
  with only an attempt announced in a VS Code issue.
  https://www.snowinch.com/en/blog/why-electron-is-heavy-tauri-react-native-alternatives

### Prior art and patterns

- **SideX** (Sidenai) — VS Code rebuilt on Tauri; the Electron→Tauri mapping and the "96%
  smaller" claim (theirs, attributed).
  https://github.com/Sidenai/sidex
- **agent-pane** (copse-dev) — Tauri + Servo migration plan; the shim-the-`electron`-module /
  Node-sidecar pattern and the 3-shim architecture.
  https://github.com/copse-dev/agent-pane
- **rust-anything** (erkamyaman) — record / bridge / diff / lift; the record-replay parity
  model and the deferred desktop-shell step.
  https://github.com/erkamyaman/rust-anything
- **Tauri v2 documentation** — system webview, plugins, mobile support, capabilities.
  https://v2.tauri.app/

### Reference repos (verified November 2026)

- **codeglocks/electro-dash** — Electron + React dashboard template.
  https://github.com/codeglocks/electro-dash
- **mbenja/tauri-todo-app** — Tauri + React + TypeScript to-do app (Prisma + SQLite).
  https://github.com/mbenja/tauri-todo-app
- **hschneider/tauri-quasar** — Tauri 2 + Quasar + Vue 3 starter app.
  https://github.com/hschneider/tauri-quasar

### Unverified claims in this document

- The per-framework accessibility maturity of egui, Iced, Dioxus, and Slint (section 8).
  Marked **unverified** where it appears; the phase 4 design must assess each before
  recommending any native-UI target.
- The Verge article URL (above): the report itself is corroborated by multiple outlets, but
  the direct link was not verified.
- The exact wording of the VS Code issue announcing a translator attempt (Snowinch's account
  is the source; the issue itself was not fetched).
