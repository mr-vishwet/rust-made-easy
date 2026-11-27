# Proof — 5 real Electron apps, converted to Rust

Date: 2026-11-25. Corrected 2026-11-27 (v3 accuracy re-check, §4). Tool: `e2r` 0.1.0 (`electron-to-rust`, this repo).
Status: campaign complete. Nothing in this document is a plan or a projection
unless it is labelled **prediction** or **model estimate**.

## The claim, in one paragraph

Discord moved to Rust for performance. On **November 4, 2026**, Discord
co-founder and CTO Stanislav Vishnevskiy confirmed the desktop client is being
rebuilt in Rust ("Discord Phoenix", GPUI — explicitly *not* Tauri), with an
early build showing ~290 MB of RAM in one Task Manager screenshot; see
[docs/RND.md](RND.md) §2 for the full entry, sources, and what the 290 MB
figure does and does not prove. **Discord itself is NOT open source and is NOT
one of the five apps below — it is framing only.** This campaign shows the
same move, automated, on open-source apps of the same class: five mass-market,
daily-use Electron apps with documented high-RAM complaints were scanned,
simulated, converted to Tauri 2 projects, and those generated projects were
compiled and release-built. Every number below is either a measurement (method
stated) or a labelled prediction. Where the campaign got something wrong, it
says so — most prominently in §6, the calibration story, and in §4, the v3 accuracy corrections.

**Headline results**

| Result | Number |
|---|---|
| Apps converted | **5 / 5** (`e2r convert` exit 0) |
| Generated projects passing `cargo check` | **5 / 5** |
| Generated projects passing `cargo build --release` | **5 / 5** |
| Measured Tauri release binaries | **22.42–27.26 MB** (21.38–26.00 MiB, unstripped, Tauri 2.12.7) |
| Measured Electron installers they replace (Linux .deb) | **117.3–463.6 MiB** |
| Binary vs installer (binary-vs-installer, NOT like-for-like — see §5) | **~4.5×–~20× smaller** |
| Measured binaries inside their predicted binary interval | **5 / 5** (after the like-with-like fix, §6) |
| Latest hand-verified scan accuracy (v3) | **79 / 101 = 78.2%** (v2: 85 / 101 = 84.2%; v1: 102 / 134 = 76.1%) |
| v3 feasibility scores (computed) | 19 / 42 / 23 / 23 / 46 — honest scores: VS Code ≈32, Signal 42, Joplin 23, Mailspring ~28–29, Insomnia 48 (see §4) |

**What "cargo check passes" means, precisely:** the generated Tauri 2 project
compiles. It does **not** mean the app works. No generated app was launched —
every one ships a placeholder frontend (see §5 and §8) — so there is no
runtime verification anywhere in this campaign. Any reading of this document
that treats a green build as a working app is wrong.

## 1. Method — and how to reproduce it

### Tooling and versions

- `e2r` **0.1.0**, `converters/electron/crates/e2r-cli`. Mappings:
  `converters/electron/mappings/electron-to-tauri.json` — **48 entries (v1),
  95 (v2), 96 (v3)** (the v3 entry is `webview`).
- Rust **1.100.0** (`~/.cargo/bin`, not on default PATH). Tauri **2.12.7**.
  Linux x86_64 (host = target platform for every simulation and build).
- **Which binary produced which numbers — this matters.**
  Scans and simulations in this document are **v3**, produced by the
  fix-round-2 binary (`target/release/e2r`, 9,531,016 bytes, md5
  `bd94b97b24c52594bb70a13ce8ac3e44`). Converts, `cargo check`s, release
  builds, and measured binaries were produced with the **fix-round-1**
  binary (9,388,232 bytes). The round-2 changes affect finding counts
  (scanner accuracy), not generated-project compilability or binary size —
  the renderer picks, the one round-2 change that touches `convert`, were
  verified against the final binary with convert smoke runs (exit 0, no
  cargo check/build), not with a full re-convert. The v2 convert outputs
  therefore stand as measured.
- Tests on the final (round-2) code: `cargo test --lib` **183 passed,
  0 failed**; `cargo test --test golden` **10 passed, 0 failed**;
  `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings`
  clean.
- Repo tree at campaign time: main at pushed commit **959f3a10** plus
  uncommitted fix rounds 1 + 2 (the tree is not a git repo, so the
  uncommitted diff could not be recorded as a SHA). The campaign was pushed
  as commit **05f941f4** on 2026-11-26; the v3 accuracy corrections in §4
  were pushed after that (see [docs/ACCURACY-V3.md](ACCURACY-V3.md)).

### Exact commands

```sh
# 1. Clone (depth 1, HEAD as of 2026-11-25; full SHAs in §2)
git clone --depth 1 https://github.com/microsoft/vscode proof/vscode
# … likewise signalapp/Signal-Desktop, laurent22/joplin,
#    Foundry376/Mailspring, Kong/insomnia

# 2. Scan (twice: human + JSON)
e2r scan <slug> --out <slug>/scan.txt
e2r scan <slug> --json --out <slug>/scan.json

# 3. Simulate (twice: human + JSON; --record writes the predictions record
#    the calibration loop consumes). Payload is identical across runs.
e2r simulate <slug> --record <slug>/predictions.json --out <slug>/simulate.txt
e2r simulate <slug> --json --record <slug>/predictions.json --out <slug>/simulate.json

# 4. Convert
e2r convert <slug> --out proof/<slug>-tauri

# 5. Compile + release-build the generated project (sysroot mechanism from
#    converters/electron/crates/e2r-cli/tests/golden.rs)
export PKG_CONFIG_SYSROOT_DIR=<repo>/target/tauri-sysroot/root
export PKG_CONFIG_PATH=$PKG_CONFIG_SYSROOT_DIR/usr/lib/x86_64-linux-gnu/pkgconfig:$PKG_CONFIG_SYSROOT_DIR/usr/share/pkgconfig
export CARGO_BUILD_JOBS=2
cargo check        # in <slug>-tauri/src-tauri/
cargo build --release

# 6. Measure + calibrate (measurements.json holds the measured binary, MB)
e2r calibrate <slug> --actual <slug>-measurements.json --record <slug>/predictions.json
```

Per-app artefacts live in `~/workspace/rust-rnd/proof/<slug>/` (outside this
repo): `scan.txt`, `scan.json`, `simulate.txt`, `simulate.json`,
`predictions.json`, `META.md`, `ACCURACY.md`, `ACCURACY-V2.md`, `ACCURACY-V3.md` (and the
combined `proof/ACCURACY-V3.md`, copied into this repo as
`docs/ACCURACY-V3.md`),
`CONVERT.md`, plus `v1/` and `v2/` directories preserving the earlier runs'
outputs (Mailspring has no `v1/` — its v1 files were overwritten by an early
smoke re-scan; its v1 numbers survive in its `META.md` and in the campaign
summaries). Campaign summaries: `SCAN-SUMMARY.md` (v1+v2+v3),
`RESCAN-SUMMARY.md` (v2, v3, N1–N11, completion round),
`CONVERT-SUMMARY.md`, `COMPLAINT-RANKING.md`, `LICENSES.md`,
`calibration-correction/CORRECTION.md`. Generated projects:
`proof/<slug>-tauri/` (~3.1–3.5 GB each, mostly `src-tauri/target/`).
Logs: `proof/<slug>-{convert,cargo-check,cargo-build,calibrate}.log`,
`proof/<slug>-measurements.json`.

### What the simulator is, and is not

The simulator is model **0.1.0** (see `docs/RND.md` §11 for the research
basis). Its per-app inputs are real: the measured app payload (the clone,
minus e2r's own outputs) and the window inventory. Everything else is model
boilerplate, and this document labels it as such wherever it appears:

- **Bundle "before" is never measured.** No `node_modules/electron/dist`
  exists in a source clone, so "before" is the model's generic **90–160 MB**
  range for every app. §5 shows what the real installers actually measure —
  and that the generic range understates Insomnia by ~3×.
- **Memory (Linux) is refused, not predicted.** The model has no published
  WebKitGTK baseline and says so: `unknown` for all five apps.
- **Startup is a flat 0.8–1.3× (point exactly 1.0), low confidence, for all
  five.** Published measurements disagree on which framework starts faster,
  so the model predicts no meaningful change.
- **CPU is refused for all five** (no reliable baseline).
- The only per-app signals in the predictions are the payload-driven bundle
  interval, the plugin-count-driven binary interval, and the
  window-count-driven process interval.

## 2. The five apps

All five are purely open source (licenses verified from each clone's LICENSE
file + `package.json` `license` field — see `LICENSES.md`; Insomnia's
Apache-2.0 was verified the same way from its clone, as `LICENSES.md` omits
that row). Audience figures are **approx.** and labelled with their source;
they are vendor/platform claims, not measurements. Complaint evidence links
are in each app's `COMPLAINTS.md` (summarised in `COMPLAINT-RANKING.md`).

| App | Repo @ SHA (clone, `--depth 1`, 2026-11-25) | License | Electron | Clone size | Audience (approx.) | Why this app |
|---|---|---|---|---|---|---|
| **VS Code** | `microsoft/vscode` @ `036980bb5118eb94d072d58789ac263c29b9e64d` | MIT | 43.8.10¹ | 493 MB | ~50M monthly devs (VS + VS Code combined, Microsoft, May 2025) | The anchor: the editor developers live in all day; 195,740 stars |
| **Signal Desktop** | `signalapp/Signal-Desktop` @ `01e59719454fec204b3f7902fb663d68419848c9` | AGPL-3.0 | 45.4.1 | 260 MB | 70–100M MAU (Signal president Whittaker via NZZ, Apr 2025) | Mass-market daily messaging — the closest open-source analogue to Discord's audience |
| **Joplin** | `laurent22/joplin` @ `ce86e12150baa4d98dbc3e0cd2fd4cdd9e65d604` | AGPL-3.0-or-later | 42.11.18² | 414 MB | ~13.4M desktop downloads (official stats page) | Daily notes app; 57,348 stars, second-highest in the set |
| **Mailspring** | `Foundry376/Mailspring` @ `7a8ab27168960f967c3b572b014edea6db729655` | GPL-3.0 | 44.11.4 | 118 MB | 800k+ downloads (official site) | Always-on email; the best-measured leak in the campaign |
| **Insomnia** | `Kong/insomnia` @ `cb79b8d7c1477561294e3c7ba16329563636b58b` | Apache-2.0 | 43.2.0³ | 45 MB | 1M+ users (Kong press release, Sep 2024) | Daily API client for developers; worst startup profile in the set |

¹ VS Code has no `electron` dependency in `package.json`; 43.8.10 comes from
`nativeBuild.target`, resolved by `build/lib/electronVersion.ts`. ² Joplin:
from `packages/app-desktop/package.json` (monorepo root has no electron dep).
³ Insomnia: from `packages/insomnia/package.json` devDependencies.

**Complaint evidence (the reason each app is in the set — every figure is a
reported complaint, not a measurement by this campaign):**

| App | Complaint strength | Evidence (links in `COMPLAINTS.md`) |
|---|---|---|
| VS Code | **STRONG** | 7 GB+ total, renderer ~100% CPU freeze — [#284727](https://github.com/microsoft/vscode/issues/284727), [#307401](https://github.com/microsoft/vscode/issues/307401) (referenced fix commit 2026-04-06, closed in #308000), [#267343](https://github.com/microsoft/vscode/issues/267343). Pain is language servers + extension host + Copilot, not the core editor. |
| Signal Desktop | **STRONG** | 7 GB footprint with kernel OOM-kill, 400 MB → 2.5 GB slow leak, video-call VRAM leak — [#5267](https://github.com/signalapp/Signal-Desktop/issues/5267), [#6249](https://github.com/signalapp/Signal-Desktop/issues/6249), [#7451](https://github.com/signalapp/Signal-Desktop/issues/7451). Three distinct leak paths. |
| Joplin | **STRONG-MODERATE** | 1.5 GB while completely idle; 500 MB at startup; unresponsive at 10k+ notes — [#12354](https://github.com/laurent22/joplin/issues/12354), [discourse #37454](https://discourse.joplinapp.org/t/slowness-and-sometimes-the-window-is-not-responding-error-message/37454). Large-collection slowness more than one leak. |
| Mailspring | **STRONG** | Measured leak, +10–15 MB / 45.5 min stair-step over 23 hours, reaching several GB — [#1007](https://github.com/Foundry376/Mailspring/issues/1007), [#1758](https://github.com/Foundry376/Mailspring/issues/1758), [community #14064](https://community.getmailspring.com/t/memory-leak-in-mailsync-main-process/14064) (marked Resolved). |
| Insomnia | **STRONG** | 4 GB at startup (90 s), 9 GB, launch freezes before the UI is usable — [#4331](https://github.com/Kong/insomnia/issues/4331), [#7826](https://github.com/Kong/insomnia/issues/7826), [#8020](https://github.com/Kong/insomnia/issues/8020). |

**First alternate — GitHub Desktop.** `desktop/desktop` @
`638896e0680a7553c1419089830a2435225de473`, MIT, Electron 44.11.2, 45 MB
clone, 22,141 stars. Complaint evidence **MODERATE** (CPU/slowness-heavy;
RAM evidence older, second-hand, partly closed-as-unreproducible) and **no
public Desktop-specific user count exists** — the 100M+/225M+ GitHub figures
are platform-wide, not Desktop users. It was swapped out for Insomnia, which
beats it on both axes (STRONG complaints + 1M+ stated users). It was scanned
with the v2 binary (score 0, 83 groups, 202 occurrences, 2 real windows, UI
60.0%, bundle 38.12–58.52 MB, payload 23.615 MB; accuracy v1 15/21 = 71.4%,
no v2 re-check) and **was not converted**.

## 3. Compatibility matrix — Before → After

Read the score alongside the group counts, never instead of them. The score
is a summary; the counts are the signal.

### 3.1 Feasibility score (v1 → v2 → v3)

| App | v1 | v2 | v3 | What changed |
|---|---|---|---|---|
| VS Code | 30 | 15 | **19** (honest ≈32) | v2: keytar found (−15). v3: third native (@parcel/watcher) under the new formula — but keytar is a mock stub, not a native (§4); correcting only that error gives 29, correcting the audited false occurrences gives ≈32 |
| Signal | 45 | 30 | **42** | v2: sqlcipher found (−15). v3: test windows gone, formula un-saturates |
| Joplin | 45 | 0 | **23** | v2's 0 was a formula artifact (5 natives = −75, then both caps). v3 formula caps the native charge at −30 |
| Mailspring | 30 | 15 | **23** (honest ~28–29) | v2: node-llama-cpp found (−15). v3: remote-mediated MANUALs now split out; de-duplicated (remote roll-up groups overlap the remote-mediated findings) the score is ~28–29 (§4) |
| Insomnia | 45 | 45 | **46** (honest 48) | v2: libcurl still missed (score too lenient by ≥15). v3: libcurl found, MANUAL groups 11 → 14; IPC is ~285 real, not 268, and without the one false positive the score is 48 (§4) |
| GitHub Desktop (alternate) | 35 | 0 | — (not re-scanned) | v2's 0 is the same formula artifact as Joplin's |

**Score formula v3** (`scan.rs`, `docs/RND.md` §Feasibility score v3):
`score = 100 − min(10×natives, 30) − min(2×MANUAL_groups, 30) −
min(max(ASSISTED−5, 0), 15) − round(20×MANUAL_occ/total_occ) −
(10 if ipcRenderer with no preload)`, clamped [0, 100]. The v2 formula
(`100 − 15×natives − min(10×MANUAL, 40) − min(5×(ASSISTED−5), 15)`)
saturated: every app hit both caps, so the v2 score degenerated to
`45 − 15×natives` and stopped discriminating. The v3 formula discriminates
(19 / 42 / 23 / 23 / 46); Joplin and Mailspring share 23 by coincidence of
different terms, not by cap. Every v3 score recomputes exactly from its own
`scan.json`. The formula is implemented correctly — every v3 score
recomputes exactly, hand-verified in §4 — but the *inputs* are not all
honest: VS Code's 19 charges −10 for a mock native and inflates the
Manual-share term with a false-occurrence flood (honest ≈32); Mailspring's
23 double-counts remote-mediated calls (honest ~28–29); Insomnia's 46 is 2
points too harsh (honest 48). Signal (42) and Joplin (23) are the most
honest as reported.

### 3.2 API sites (v3 counts) — Before (v1) → After (v3)

AUTO = converts mechanically. ASSISTED = converts with a human decision.
MANUAL = needs a rewrite or a recipe. Groups = distinct API findings;
occurrences = call sites.

| App | Groups AUTO / ASSISTED / MANUAL (total) | Occurrences AUTO / ASSISTED / MANUAL (total) | AUTO share of call sites | MANUAL share of call sites |
|---|---|---|---|---|
| VS Code | 86 / 62 / 92 (**240**; v1: 96/57/22 = 175) | 225 / 302 / 237 (**764**; v1: 333/243/103 = 679) | 29.5% (v1: 49.0%) | 31.0% (v1: 15.2%) |
| Signal | 103 / 25 / 27 (**155**; v1: 112/22/9 = 143) | 271 / 51 / 60 (**382**; v1: 307/65/23 = 395) | 70.9% (v1: 77.7%) | 15.7% (v1: 5.8%) |
| Joplin | 60 / 37 / 16 (**113**; v1: 70/25/4 = 99) | 127 / 82 / 18 (**227**; v1: 146/54/5 = 205) | 55.9% (v1: 71.2%) | 7.9% (v1: 2.4%) |
| Mailspring | 127 / 41 / 205 (**373**; v1: 175/69/18 = 262) | 236 / 61 / 450 (**747**; v1: 321/131/67 = 519) | 31.6% (v1: 61.8%) | 60.2% (v1: 12.9%) |
| Insomnia | 134 / 23 / 14 (**171**; v1: 110/21/7 = 138) | 449 / 66 / 23 (**538**; v1: 253/64/11 = 328) | 83.5% (v1: 77.1%) | 4.3% (v1: 3.4%) |

Read the direction correctly: the AUTO share *fell* for four apps and the
MANUAL share *rose* for all five — not because the apps got worse, but
because the fixed scanner finds more real work (mappings 48 → 96, native
modules found, test files excluded, remote-mediated calls forced to MANUAL).
Mailspring's MANUAL explosion is the extreme case: 174/194 MANUAL groups in
v2 were `@electron/remote`-reached calls, including APIs whose mapping note
says "Direct equivalent" (`app.quit`, `clipboard`, `dialog`, `shell`).
The v3 scanner flags these separately (`remote_mediated: true`; 112 groups
for Mailspring), so "no Tauri equivalent" can now be split from "reached
through remote" — the count did not fall, it became honest. Hand-verification
qualifies that: 7 ASSISTED groups escape the flag (the detector only fires
on a single-line `require('@electron/remote').<Api>` pattern), so MANUAL is
really 212 / ASSISTED 34, and one flag is outright wrong
(`getCurrentWindow` at `app-env.ts:492` is MANUAL with
`remote_mediated: false` although the next line is literally
`return require('@electron/remote').getCurrentWindow();`) (§4).

Other v3 scan facts: files scanned 10,926 / 2,633 / 2,386 / 885 / 1,367
(v1: 16,131 / 3,261 / 3,843 / 1,224 / 1,951 — the drop is test/spec/fixture
exclusion, not smaller apps). Windows found 4 / 9 / 2 / 8 / 5 (all real
main windows found; Signal's 11 test windows and VS Code's fixture window
are gone in v3). Signal's window *sizes* are mostly wrong, though: only 1
of 9 is fully correct — five report height 44, but `height: 44` occurs
exactly once in `app/main.main.ts` (the scanner leaks one window's height
into every `const options` window) (§4). IPC channels 78 / 73 / 30 / 114 / **268** (Insomnia's
72 → 268 is the `ipcMainHandle`/`ipcMainOn`/`ipcMainOnce` wrappers finally
being counted; hand-verification finds ~285 real — 17 registrations missed,
including all three `once` sites, §4). UI carry-over 60.0% for all five — the scanner's floor, not a
measurement: all five report exactly 60.0%, so it carries no per-app
information (§4). The v2 figures of 79% / 67% / 94% were measured on the
wrong renderer files with 2–11 touch points each, so the drop is the
measurement getting honest, not the UI getting worse. Touch-point caveats:
Insomnia's 238 counts 157 in `src/main/`, 26 in `src/sync/`, 8 in test
mocks, and only 7 in true renderer files; VS Code's `src/` pick still
includes 10.7% `electron-main`/bootstrap/test files, and bundler "vite"
is a substring artifact (no vite dep or config; the real build is
gulp+rspack). Effort hints:
VS Code **large**, Joplin **large**, Mailspring **large**, Signal medium,
Insomnia medium. Warnings: 33 / 19 / 18 / 50 / 12.

### 3.3 Feature support per app — Before (Electron uses) → After (what e2r does)

Supported = converts automatically. Partial = converts with a human decision
or a stub. Manual = needs a human rewrite / has no Tauri equivalent. N/A =
the app does not use it.

| Feature | VS Code | Signal | Joplin | Mailspring | Insomnia |
|---|---|---|---|---|---|
| Menu | Before: custom workbench bar (not Electron `Menu`) → After: **Manual** — scan's hit is still a Copilot simulation fixture, flagged app-menu (wrong); the real menu is dynamic and unresolvable by the scanner | Before: real app menu (`main.main.ts:2607`) → After: **Partial** — dynamic template → `menu.rs` stub | Before: app menu (`MenuBar.tsx:1029`) → After: **Partial** — static template → `menu.rs` stub (original kept as comment) | Before: real app menu (`application-menu.ts:57`) → After: **Partial** — dynamic template → `menu.rs` stub | Before: app menu (`window-utils.ts:793`) → After: **Partial** — dynamic template → `menu.rs` stub |
| Tray | Before: no → After: **N/A** | Before: yes → After: **Partial** (system-tray plugin; flagged in convert warnings) | Before: yes → After: **Partial** | Before: yes → After: **Partial** | Before: no → After: **N/A** |
| Dialogs | Before: yes → After: **Supported** (dialog plugin) | Before: yes → After: **Supported** | Before: yes → After: **Supported** | Before: yes (heaviest: 31+ sites) → After: **Supported** | Before: yes → After: **Supported** |
| Updater | Before: custom service (macOS `autoUpdater` only) → After: **Manual** (custom service has no plugin equivalent) | Before: yes → After: **Partial** (updater plugin) | Before: yes (`electron-updater`) → After: **Partial** (updater plugin) | Before: yes → After: **Partial** — v3 newly finds Electron's built-in `autoUpdater` (14 sites) → 7th plugin; the Linux `autoUpdater` binding itself was lost from the v2 scan by the identifier gate (see §4) | Before: yes, DUAL (`autoUpdater` + `electron-updater`) → After: **Partial** — v3 correctly splits the two (Electron's at `updates.ts:81`, the library at `:95`) |
| Notifications | Before: yes → After: **Supported** (notification plugin) | Before: yes → After: **Supported** | Before: no (in-app alarms) → After: **N/A** | Before: yes → After: **Supported** | Before: no (in-app toasts) → After: **N/A** |
| Deep links | Before: yes ×5 (`vscode://`) → After: **Supported** (deep-link plugin) | Before: yes → After: **Supported** | Before: yes ×2 → After: **Supported** | Before: yes ×3 → After: **Supported** | Before: yes ×1 (`insomnia://`) → After: **Supported** |
| Native modules | Before: `node-pty`, `@parcel/watcher` (`keytar` is a mock stub, not a native — §4) → After: **Manual** — no Tauri equivalent; flagged in convert warnings | Before: `@signalapp/sqlcipher` → After: **Manual** | Before: `keytar`, `onnxruntime-node`, `sqlite-vec`, `sqlite3` (`sharp` is union-only, never passed on desktop — §4) → After: **Manual** | Before: `better-sqlite3`, `node-llama-cpp` → After: **Manual** — plus spawned native binaries (`mailsync`, `mailspring-fm`) the scanner cannot see at all (§8) | Before: `@getinsomnia/node-libcurl` → After: **Manual** — found only in v3; the v2 scan and convert were both blind to it |
| Preload | Before: 3 real files, contextBridge → After: **Partial** — typed bridge wrappers derived from the preload shape | Before: 26 entry files + 466 modules → After: **Partial** — bridge re-implements 11 `window.*` keys (v2 listed 603 files; only 14 were entry points) | Before: 2 files; main window has **none** (nodeIntegration + remote instead) → After: **Manual** for the main window | Before: 2 files + 1 module → After: **Partial** | Before: 3 files (the "+1 module" is `entry.preload.ts` double-counted — §4) → After: **Partial** |
| nodeIntegration (production) | Before: no (fixture only) → After: **N/A** (production is sandboxed) | Before: no (sandboxed) → After: **N/A** | Before: **YES** (main window + plugins) → After: **Manual** (security redesign, not a mechanical port) | Before: **YES** (main window) → After: **Manual** | Before: **YES** (plugin + hidden windows; main window secure) → After: **Manual** for those windows |

### 3.4 Convert result — Before (Electron app) → After (generated Tauri project)

All five converts ran with the fix-round-1 binary. No generated code was
hand-fixed; no convert failed.

| App | Convert | Files written | `cargo check` | Release build | Plugins | Convert warnings | Renderer carried over |
|---|---|---|---|---|---|---|---|
| VS Code | exit 0 (26.75 s) | 14,373 | **PASS** (408 s) | **PASS** (1,487 s) | 7 | 102 (92 "Manual port needed", almost all `session`) | **Partial** — whole `src/` copied (workbench source, 269 MB); `dist/index.html` is the placeholder; no build script, so `beforeBuildCommand` is empty. Generated project is named `copilot-chat` 0.77.0 (nested-package mis-pick, fixed in the v3 scanner) |
| Signal | exit 0 (10.0 s) | 192 | **PASS** (278 s) | **PASS** (1,026 s) | 7 | 55 (24 Manual) | **No** — copied `sticker-creator/src` (a mini-app), not `ts/`; `beforeBuildCommand` would rebuild the sticker-creator. 11 test windows leaked into `tauri.conf.json`; 603 preload files listed (14 real) |
| Joplin | exit 0 (8.9 s) | 48 | **PASS** (448 s) | **PASS** (1,331 s) | 7 | 28 (16 Manual) | **No** — copied `packages/pdf-viewer` (32 files), not `packages/app-desktop/gui/`; dev URL is the bogus CLI docs server (`localhost:41184`) |
| Mailspring | exit 0 (11.4 s) | 7,138 | **PASS** (282 s) | **PASS** (761 s) | 6¹ | 206 (194 Manual — heaviest of the five, almost all `@electron/remote`) | **Partial** — whole `app/` copied (the only one where the copied renderer looks like the actual app); build script preserved as `beforeBuildCommand`, but `dist/` is still the placeholder and was never run |
| Insomnia | exit 0 (7.6 s) | 20 | **PASS** (254 s) | **PASS** (754 s) | 6 | 23 (11 Manual) | **No** — copied `examples/insomnia-plugin-sandbox-demo`, not `packages/insomnia/src/`; no build script preserved |

¹ Mailspring's generated project has 6 plugins because it was converted
with the round-1 binary; the v3 scan finds a 7th (updater, from the newly
found built-in `autoUpdater`), so a re-convert with the final binary would
emit 7.

**The renderer pick is fixed in the v3 scanner** (shared `find_source_dir`:
example/demo/sample/sandbox/playground never descended into, foreign
packages excluded, density-first ranking) and verified by convert smoke runs
against the final binary: VS Code `src/`, Signal `ts/`, Joplin
`packages/app-desktop/gui/`, Mailspring `app/`, Insomnia
`packages/insomnia/src/`. The generated projects above still carry the v2
mis-picks because no full re-convert was run — that is a deliberate, stated
gap, not an oversight (§8).

### 3.5 Context rows — Phase 1 battle-test (smaller apps, earlier binary)

| App | Score | Findings (AUTO / ASSISTED / MANUAL) | Convert |
|---|---|---|---|
| electro-dash (fixture app, Phase 1) | 90/100 | 7 groups (4 / 2 / 1) | PASS, exit 0, ~3.5 min |
| simplenote-electron (Phase 1) | 80/100 | 37 groups (30 / 5 / 2) | PASS, exit 0, ~4 min |
| minimal fixture (golden test fixture) | — (fixture, no feasibility score published) | — | PASS (golden tests) |

The Phase-1 apps score higher because they are small and use few hard APIs;
the campaign apps score 19–46 because they are large and use `session`,
`powerMonitor`, `remote`, `crashReporter`, `nodeIntegration`, and native
modules at scale. The score gap is a property of the apps, not of the tool
getting worse.

## 4. Accuracy matrix — scan claims → hand-verified

Method (per app, `ACCURACY.md` v1 and `ACCURACY-V2.md` v2): a stratified
sample of findings (AUTO / ASSISTED / MANUAL + the highest-occurrence
findings), each verified against the real source line, plus a missed-API hunt
(`git grep` over the clone) and a scripted check of every finding's reported
line. A finding counts as **correct** only if the classification AND the
suggested Tauri replacement are right AND the reported line is a genuine call
site. Samples are small (18–33 findings per app); the combined figures are
the honest headline, not a per-API census.

| App | v1 accuracy | v2 accuracy | v3 accuracy | Change v2 → v3 |
|---|---|---|---|---|
| VS Code | 15/20 = 75.0% | 13/20 = 65.0% | 10/18 = **55.6%** | **−9.4 pts** |
| Signal | 19/21 = 90.5% | 19/21 = 90.5% | 20/21 = **95.2%** | +4.7 pts |
| Joplin | 17/21 = 81.0% | 19/21 = 90.5% | 16/21 = **76.2%** (81.0% with caveat) | −14.3 pts |
| Mailspring | 19/33 = 57.6% | 18/21 = 85.7% | 16/23 = **69.6%** | −16.1 pts |
| Insomnia | 17/18 = 94.4% | 16/18 = 88.9% | 17/18 = **94.4%** | +5.5 pts |
| GitHub Desktop (alternate) | 15/21 = 71.4% | — (no v2 re-check) | — (not re-scanned) | — |
| **Combined (final 5)** | **102/134 = 76.1%** | **85/101 = 84.2%** | **79/101 = 78.2%** | **−6.0 pts** |

**What the v2 fixes fixed (hand-verified):** all five real main windows
found; all five renderers found; real app menus found (4 of 5 — VS Code's
is still the Copilot fixture); monorepo metadata fixed (4 of 5 — VS Code's
was regressed in v2, fixed in v3); test exclusion works by the scanner's
rules; native modules found almost everywhere; `session` false positives
fixed (Mailspring 53 → 10 genuine sites); `webContents.send` variable
channels found; Insomnia's MCP-SDK `protocol` false positive gone.

**What the v2 regressions were — the identifier gate.** Fix round 1 added
an identifier-resolution gate (a bare identifier must resolve to an Electron
import binding; same-named locals no longer produce findings). It killed
false positives, but it also dropped real findings whose bindings the gate
cannot follow:

- **Joplin:** `session.protocol.handle` sites lost — `session` arrives as a
  typed function parameter (`handleContentProtocol = (logger, session:
  Session) => …`), which the gate drops. Same mechanism lost Signal's
  `protocol_filter` finding.
- **Mailspring:** the entire `autoUpdater` finding lost (14 occurrences) —
  the binding is `let autoUpdater = null`, conditionally assigned later
  (`:80/:83/:85`), then used at `:73/:94/:175`. The identifier gate drops
  conditionally-assigned bindings, so the updater — a flagship feature —
  is invisible in the v2 scan. (The v3 scanner's fixed bare-assignment
  binding re-finds Electron's built-in `autoUpdater`, 14 sites.)
- **VS Code (75% → 65%):** the sample lost points it had held despite the
  round finding real new APIs (`webFrame`, `webUtils`, `WebContentsView`,
  `desktopCapturer` now found). Still wrong in v2: the `remote` finding
  (`browserViewMainService.ts:388`) is VS Code's internal remote-connection
  service, not `@electron/remote` — `git grep @electron/remote` over the
  repo returns zero hits — and the menu inventory still reports the Copilot
  fixture as the app menu.
- **Insomnia (94.4% → 88.9%):** the v2 sample re-tested the v1 misses and
  they were still open — ipcMain wrappers invisible (212 sites, 0 `git.*`
  channels), `@getinsomnia/node-libcurl` still not a native module,
  preload phantoms not removed (6 listed for 3 real), window-size constants
  unresolved, "No frontend dev script" warning still misfiring. All five
  were closed in fix round 2 (v3 counts moved as predicted) — but see the
  warning below.

### v3 accuracy re-check (2026-11-27)

The v3 re-check is done: same stratified-sample method as v1/v2, all five
apps, full evidence in [docs/ACCURACY-V3.md](ACCURACY-V3.md) (combined
summary; per-app evidence in the campaign working directory). Combined
**79/101 = 78.2%** (v2: 85/101 = 84.2%). Read that figure with care:
Mailspring's ASSISTED slice was deliberately targeted at the fix surface,
so it holds all 7 of that app's wrong ASSISTED findings — the full
ASSISTED population there is 34/41 = 82.9% correct, and a v2-style random
sample would have scored ~20–21/23, putting the combined figure at
~82–83%. VS Code's drop is real (one new false group plus the untouched
`session` flood); Signal and Insomnia improved; Joplin's drop is sampling
(its sample caught two non-Electron false positives v2's sample missed,
while the known-false mass actually shrank to 6 occ / 227, 2.6%).

**Corrected numbers** (scanner claim → verified; methods in
`docs/ACCURACY-V3.md` §2):

| App | Claimed | Corrected |
|---|---|---|
| VS Code | 3 native modules (`@parcel/watcher`, `keytar`, `node-pty`) | **2** — `keytar` is a `file:./packageMocks/keytar` stub, three no-op `Promise.resolve()`s ("OVERRIDE Keytar since we don't need the feature"); zero import/call sites; real credential storage is Electron `safeStorage` |
| VS Code | score 19 | **≈32 honest** (keytar fix alone → 29; correcting also the fully-audited false occurrences gives MANUAL groups 40, ASSISTED 60, MANUAL occ 101, total occ 620, share term 3 → ≈32) |
| VS Code | MANUAL occ 237 (growth 205 → 237) | Growth is **one false group**: MANUAL `webview` `webviewPreloads.ts:187` ×32 — all TypeScript generics (`postNotebookMessage<webviewMessages.…>`); zero `<webview>` tags in the file or anywhere under `src/vs/workbench/contrib/notebook/` |
| VS Code | `session` class (57 groups, 126 occ) | **7/57 groups, 23/126 occ real (82% false)** — essentially unchanged from v2 |
| Signal | window sizes (9 windows) | **Only 1 of 9 fully correct** (300×280 loading window). Five report height 44, but `height: 44` occurs exactly once in `app/main.main.ts` — the scanner leaks one window's height into every `const options` window. Window *count* unaffected |
| Joplin | 5 native modules | **4 live on desktop** — `sharp` is union-only (never passed on desktop; `sharpEnabled()` is false). Score unaffected (native charge caps at 30) |
| Joplin | 7 menus | **6 logical menus** — two branches of one `updateMenu()` |
| Mailspring | MANUAL 205 / ASSISTED 41 | **MANUAL 212 / ASSISTED 34** if the 7 under-classified ASSISTED groups escalate. Score unchanged (capped terms) |
| Mailspring | score 23 | **~28–29 de-duplicated** — 71 `remote` roll-up groups (217 occ) overlap the 112 remote-mediated findings (196 occ) |
| Mailspring | quickpreview window 320×320 known | **≈414×414** — real size is `320 * (11/8.5)`; const-resolution dropped the multiplier. REGRESSION vs v2 (which honestly reported unknown) |
| Insomnia | IPC 268 channels | **~285 real** — 207/224 main-side registrations listed (17 missing = 92.4% coverage) |
| Insomnia | score 46 | **48 without the one false positive** — the I-18 `session` prose group is the only score-moving error; 46 is 2 points too harsh |
| Insomnia | preload "3 + 1 module" | **3 real files; the "+1 module" is `entry.preload.ts` double-counted** |

**Fix round 2 (N1–N11), re-verified:** N1, N2, N3, N4, N7, N9, N11 **closed**;
N5 **closed as implemented, wrong in substance for VS Code** (keytar);
N6 **mostly closed** (IPC 72 → 268 real, but 17 registrations missing);
N8 **partly held** ("CLOSED" overstates it — 7 ASSISTED groups escape the
flag plus 1 flag error); N10 **closed in code, unverifiable for Joplin**
(`scan.json` carries no warnings field). The **60.0% UI carry-over is the
scanner's floor, not a measurement** — all five apps report exactly 60.0%.

## 5. Performance matrix — Before → After

### 5.1 Size: Electron installer (measured) → Tauri release binary (measured)

Both sides are measurements. The comparison is **binary-vs-installer, NOT
bundle-vs-bundle**: the shipped Tauri bundle would also contain the built
frontend, which does not exist for any app (placeholder `dist/`), so no
honest shipped-bundle figure exists for any app (§6).

| App | Electron installer, Linux .deb (measured) | Tauri release binary (measured, unstripped, Tauri 2.12.7) | Ratio | Simulator binary interval (v3, prediction) | Binary inside? | Simulator bundle interval (v3, prediction) | Binary + predicted payload vs bundle interval |
|---|---|---|---|---|---|---|---|
| VS Code | **280.1 MiB** (`code_1.148.0-1795526052_amd64.deb`, 293,756,210 B; stable 1.148.0 via `update.code.visualstudio.com`; clone is 1.149.0-dev) | **26.41 MB / 25.19 MiB** (26,409,848 B; 7 plugins; binary `copilot-chat`) | ~11× | 14.5–34.9 MB (pt 25.4) | **yes** | 379.97–400.37 MB (pt 390.87); payload 365.465 MB | 391.88 MB — **inside** |
| Signal | **117.3 MiB** (`signal-desktop_8.36.0_amd64.deb`, 123,016,636 B; official apt repo `updates.signal.org`; clone is 8.39.0-alpha.1) | **27.26 MB / 26.00 MiB** (27,258,184 B; 7 plugins) | ~4.5× | 14.5–34.9 MB (pt 25.4) | **yes** | 171.01–191.41 MB (pt 181.91); payload 156.514 MB | 183.77 MB — **inside** |
| Joplin | **160.9 MiB** (`Joplin-3.7.24.deb`, 168,720,160 B; GitHub Releases v3.7.24, matches clone; AppImage 197.8 MiB) | **23.58 MB / 22.49 MiB** (23,582,400 B; 7 plugins) | ~7× | 14.5–34.9 MB (pt 25.4) | **yes** | 277.42–297.82 MB (pt 288.32); payload 262.919 MB | 286.50 MB — **inside** |
| Mailspring | **138.9 MiB** (`mailspring-1.26.2-amd64.deb`, 145,616,706 B; GitHub Releases 1.26.2; clone is 2.0.0, ahead of stable; rpm 179.9 MiB) | **22.42 MB / 21.38 MiB** (22,418,464 B; 6 plugins¹) | ~6.5× | 14.5–34.9 MB (pt 25.4) | **yes** | 84.09–104.49 MB (pt 94.99); payload 69.586 MB | 92.01 MB — **inside** |
| Insomnia | **463.6 MiB** (`Insomnia.Core-13.5.0.deb`, 486,158,652 B; GitHub Releases core@13.5.0; clone is 13.3.1, one minor behind; AppImage 646.1 MiB) | **23.82 MB / 22.72 MiB** (23,823,096 B; 6 plugins) | ~20× | 14.0–32.4 MB (pt 23.4) | **yes** | 41.65–60.05 MB (pt 51.05); payload 27.653 MB | 51.47 MB — **inside** (almost exactly on the point, 51.05) |

MB = bytes/10⁶ (the simulator's unit); MiB = bytes/2²⁰. The measured
binaries cluster tightly (**21.38–26.00 MiB**) despite payloads spanning
27.6–365.4 MB — the binary is essentially payload-independent, as the model
assumes (11–17 MB zero-plugin floor + ~2 MB/plugin; the measured 22–27 MB
sit at the top of that shape with 6–7 plugins). No stripping was applied
(default profile, no `[profile.release]`).

**The Insomnia before-range finding.** The simulator's generic "before"
bundle range is 90–160 MB for every app. The measured installers break it:
Insomnia's real installer is **463.6 MiB (~486 MB) — ~3× the top of the
range** — while its predicted "after" bundle (41.65–60.05 MB) is built on a
27.6 MB payload. VS Code's installer (280.1 MiB, ~294 MB) is ~1.8× the top
of the range; Joplin's (160.9 MiB, ~169 MB) sits just above it; Signal's
(117.3 MiB) and Mailspring's (138.9 MiB) fall inside it. The model's
before-range understates the starting point of the largest apps, which is
why the predicted bundles exceed the real Electron installer for VS Code
(380–400 MB vs 280 MiB), Joplin (277–298 vs 161 MiB), and Signal
(171–191 vs 117 MiB) — and why "after is bigger than before" in those rows
is a property of the payload (365 / 263 / 157 MB of app code that would ship
in the bundle), not evidence that Tauri made the app bigger. The honest
like-for-like bundle comparison does not exist yet, for any app.

### 5.2 Runtime metrics — predictions only, NOT measured

No generated app was launched (no built frontend exists to launch), so
**no runtime metric was measured for any app**. The table below is the
simulator's predictions, and nothing else.

| App | Memory (Linux) | Startup | Process count (Linux) — Electron (model, from window inventory) → Tauri (model prediction, v3) | CPU |
|---|---|---|---|---|
| VS Code | **unknown (refused)** | 0.8–1.3×, pt 1.0, low confidence | 7–9 (pt 8) → **6–11 (pt 7)**, low | unknown (refused) |
| Signal | **unknown (refused)** | 0.8–1.3×, pt 1.0, low confidence | 14–16 (pt 15) → **11–21 (pt 12)**, low ² | unknown (refused) |
| Joplin | **unknown (refused)** | 0.8–1.3×, pt 1.0, low confidence | 3–5 (pt 4) → **4–7 (pt 5)**, low | unknown (refused) |
| Mailspring | **unknown (refused)** | 0.8–1.3×, pt 1.0, low confidence | 9–11 (pt 10) → **10–19 (pt 11)**, low | unknown (refused) |
| Insomnia | **unknown (refused)** | 0.8–1.3×, pt 1.0, low confidence | 7–9 (pt 8) → **7–13 (pt 8)**, low | unknown (refused) |

² Signal's v2 prediction was the worst in the campaign (point 23, driven by
11 test windows counted as real windows); v3's point 12 follows the test
exclusion fix. The "before" side is the model's own v1 window inventory;
Signal's v2 "before" (22–24) was the inflated one.

`calibrate` compared only bundle size for every app (memory, startup,
process count, CPU all "not compared" — no prediction or no comparable
actual). The complaint evidence in §2 (7 GB footprints, multi-GB leaks,
4 GB startups) is the *motivation* for this work; it is not a before-figure
this campaign measured against, and no after-figure exists to compare it
with yet.

## 6. The calibration story — read this before quoting any calibration number

This is the campaign's most important honest finding, and it is stated here
prominently rather than buried: **the first calibration round was wrong, and
the campaign caught it, documented it, and removed it.**

### What happened

`e2r calibrate` ran once per app against the v2 predictions records and
appended one entry per app to `~/.cache/e2r/calibration.jsonl`. All five
came back **OUTSIDE**:

| App | Predicted bundle interval | Measured `bundle_size_mb` | Error |
|---|---|---|---|
| VS Code | 379.936–400.336 MB | 26.41 MB | −364.426 MB (**−93.243%**) |
| Joplin | 277.394–297.794 MB | 23.58 MB | −264.714 MB (**−91.821%**) |
| Signal | 170.989–191.389 MB | 27.26 MB | −154.629 MB (**−85.013%**) |
| Mailspring | 83.558–101.958 MB | 22.42 MB | −70.538 MB (**−75.882%**) |
| Insomnia | 41.625–60.025 MB | 23.82 MB | −27.205 MB (**−53.317%**) |

**0 / 5 inside.** The reason is a units mismatch, not a model failure and
not a measurement error: the simulator's `bundle_size` prediction is the
**shipped bundle** — release binary + ~2 MB/plugin + the measured app
payload — while the measurements schema's `bundle_size_mb` is the
**release binary alone**. Scoring a binary-alone number against a bundle
interval can never agree for a large-payload app. Proof that the model
itself was right: binary + predicted payload lands **inside** the bundle
interval for all five (VS Code 391.85 vs 379.94–400.34; Joplin 286.47 vs
277.39–297.79; Signal 183.75 vs 170.99–191.39; Mailspring 91.98 vs
83.56–101.96; Insomnia 51.45 vs 41.63–60.03 — Insomnia almost exactly on
the point, 51.025). The model predicts the shipped bundle well;
`calibrate` compared it against the wrong quantity. Left standing, those
five entries would have **widened the bundle interval 1.5× on future
simulates** (≥3 outside comparisons trigger widening) — the wrong lesson,
applied automatically.

### The fix (code, fix round 2)

- The simulator now emits an explicit **`binary_size` prediction**
  (zero-plugin binary 11.0–17.4 MB + ~2 MB per plugin, no payload).
- `calibrate` compares the measured binary against the binary interval
  **only**; `bundle_size` compares only against a measured shipped bundle
  (new `measurements.json` field `shipped_bundle_size_mb`) and is reported
  as **unmeasured** without one — never silently scored as a miss.
- Regression tests: `binary_measurement_compares_against_binary_size_never_against_bundle_size`,
  `shipped_bundle_compares_against_the_bundle_interval`,
  `binary_size_prediction_is_the_bundle_without_payload` (+ the updated
  `plugin_counts_and_bundle_points_match_measured_binaries`).

### The correction (log)

The five wrong entries were **removed** from
`~/.cache/e2r/calibration.jsonl` — the log is empty again, back to its
pre-campaign state. The removed lines are preserved verbatim at
`proof/calibration-correction/calibration.jsonl.before-correction`
(md5 `51a2ee3f3887946d1b0c2af9d4d395d8` at removal time); the full write-up
is `proof/calibration-correction/CORRECTION.md`. No correcting entries
were added in their place: there is no shipped-bundle measurement for any
app yet, so there is nothing true to log for `bundle_size`. The binary
comparisons will be logged the next time `calibrate` runs against a
predictions record that carries a `binary_size` prediction. The v3
re-scans ran with an empty calibration log, so their intervals are
unadjusted, exactly like v2.

### The honest story, like with like

| App | Plugins | Binary interval (v3) | Measured binary | Inside? |
|---|---|---|---|---|
| VS Code | 7 | 14.5–25.4–34.9 MB | 26.41 MB | **yes** |
| Joplin | 7 | 14.5–25.4–34.9 MB | 23.58 MB | **yes** |
| Signal | 7 | 14.5–25.4–34.9 MB | 27.26 MB | **yes** |
| Mailspring | 7³ | 14.5–25.4–34.9 MB | 22.42 MB | **yes** |
| Insomnia | 6 | 14.0–23.4–32.4 MB | 23.82 MB | **yes** |

³ Mailspring's v3 predictions record carries 7 plugins (the newly found
updater plugin); its measured binary was built from the 6-plugin
round-1 project. The measured binary falls inside either interval.

**5 / 5 inside on both comparisons** (binary-vs-binary, and binary +
predicted payload vs the bundle interval) — against 0 / 5 under the old
comparison. And the remaining honest gap, stated plainly: **no
shipped-bundle measurement exists for any app.** Every generated project
ships a placeholder `dist/` — no built frontend — so no honest
bundle-vs-bundle figure exists, and none is claimed anywhere in this
document.

## 7. Per-app notes

- **VS Code — the scale proof.** The largest app in the campaign (493 MB
  clone, 10,926 files scanned, 764 call sites) converted, checked, and
  built. Hardest blocker: `session` (126 sites in v2) has no Tauri
  equivalent, and the app is a custom workbench, not an Electron menu —
  its menu, window sizes, and build pipeline are all dynamic. Honest
  limits: the generated project is named `copilot-chat` (fixed in the v3
  scanner, not re-converted); the whole `src/` tree is copied but there is
  no build script, so nothing can rebuild the frontend; accuracy fell in
  v2 (75% → 65%) and again in v3 (55.6%): `keytar` is a mock, not a native
  (honest score ≈32, not 19), a new false `webview` ×32 group (all
  TypeScript generics, zero real tags) accounts for all of the MANUAL
  occurrence growth, and the `session` flood is still 82% false.
- **Signal — the accuracy proof.** Highest verified accuracy (90.5% in both
  v1 and v2) and the cleanest fix story: test windows 20 → 9, process
  point 23 → 12, preload 603 → 26 entry files + 466 modules. Hardest
  blocker: `@signalapp/sqlcipher` (native, MANUAL) plus `systemPreferences`
  / `desktopCapturer` with no Tauri equivalent. Honest limits: the
  generated project ships the sticker-creator mini-app as its renderer,
  not Signal's UI. v3 accuracy 95.2% (20/21) — the cleanest app — but
  window sizes are mostly wrong (only the 300×280 loading window fully
  correct; five report height 44 from a single leaked literal).
- **Joplin — the monorepo proof.** The messiest input (whole-monorepo
  payload 262.919 MB, desktop + mobile + server + CLI) and the cleanest
  window inventory (2/2 real). Hardest blocker: five native modules plus
  `@electron/remote` pervasive in the main window and plugins, with
  `nodeIntegration: true` on the main window. Honest limits: the payload
  is still the whole monorepo in the simulator, so its 277–298 MB bundle
  interval is inflated by code that would never ship; the generated
  project ships the pdf-viewer sub-package as its renderer. v3 accuracy
76.2% (16/21): `sharp` is union-only, so 4 natives live on desktop, not
5; the 7 menus are 6 logical ones (two branches of one `updateMenu()`).
- **Mailspring — the remote proof.** The heaviest manual load (206 convert
  warnings, 194 MANUAL, 450 MANUAL call sites — 60.2%) and the proof that
  "MANUAL" needs splitting: most of it is `@electron/remote` mediation,
  not unmappable APIs. Hardest blocker: remote-mediated calls at scale,
  plus spawned native binaries (`mailsync`, `mailspring-fm`) the scanner
  cannot see. Honest limits: the v2 scan lost the `autoUpdater` finding
  entirely (identifier gate); the generated project is the only one whose
  copied renderer looks like the real app — and its frontend was still
  never built. v3 accuracy 69.6% (16/23): the remote split is real but
incomplete — 7 ASSISTED groups escape the flag, so MANUAL is really 212 /
ASSISTED 34 and the de-duplicated score is ~28–29; the quickpreview
window regressed to a wrong 320×320 (real ≈414×414).
- **Insomnia — the turnaround proof.** The sharpest v2→v3 correction: IPC
  72 → 268 (wrappers counted), libcurl found, `<webview>` ×2 MANUAL,
  updater correctly split, one window now 1280×720 from constants, dev
  URL localhost:3334 kept (verified correct). Hardest blocker: the
  ipcMain wrapper layer hides ~200 real channels behind three helper
  functions, and the real renderer is a monorepo package, not the root.
  Honest limits: the generated project ships an examples directory as its
  renderer; and its 463.6 MiB installer is the finding that breaks the
  simulator's generic before-range (§5.1). v3 accuracy 94.4% (17/18): IPC
  is ~285 real (17 registrations missed, including all three `once`
  sites); without the one false positive the score is 48, not 46.

## 8. Honest limits

1. **No runtime verification.** No generated app was launched. `cargo
   check` passing means the generated project compiles, not that the app
   works. Memory, startup, process count, and CPU were predicted only —
   and memory/CPU were refused outright. The complaint figures in §2 are
   the motivation, not a measured baseline.
2. **Placeholder frontends.** All five generated projects ship a
   placeholder `dist/index.html`. Three of five ship the *wrong* renderer
   source (Signal sticker-creator, Joplin pdf-viewer, Insomnia examples
   dir); VS Code ships the whole `src/` tree with no build script;
   Mailspring ships the real `app/` source but its frontend was never
   built. The v3 scanner's renderer picks are correct (smoke-verified),
   but no full re-convert was run with the final binary.
3. **No shipped-bundle measurement.** The binary-vs-installer ratios in
   §5.1 (~4.5×–~20×) are not like-for-like. A shipped Tauri bundle would
   add the built frontend; until one exists, no bundle-vs-bundle figure
   can be given, and the calibration log's `bundle_size` entries stay
   unlogged (§6).
4. **VS Code's dynamic menu is still unresolved.** The scan still reports
   the Copilot simulation fixture as the app menu, flagged
   `is_application_menu: true` — wrong, and confidently labelled. The
   real menu is dynamic and unresolvable by the scanner. This is the one
   v2 regression that survives into v3.
5. **Spawned native binaries are invisible.** Mailspring's `mailsync` and
   `mailspring-fm` (and any app's spawned helpers) are not Electron APIs
   and do not appear in any scan, finding, or score. The compatibility
   matrix is silent about them.
6. **Score formula v3.** The v3 scores discriminate, but they are a
   summary of scanner output, not a verdict on migratability — read them
   with the group counts (§3.2). Joplin's v2 score of 0 was a formula
   artifact, and the v3 formula was designed after seeing the v2 numbers.
7. **Convert outputs are from the round-1 binary.** The round-2 changes
   affect finding counts, not generated-project compilability or binary
   size (stated in §1), but the generated projects were not re-built with
   the final binary, and Mailspring's generated project has 6 plugins
   where the v3 scan finds 7.
8. **Joplin's payload is still the whole monorepo.** Its bundle interval
   (277–298 MB) is inflated by desktop + mobile + server + CLI code that
   would never ship in the desktop bundle.
9. **v3 accuracy is hand-verified** (79/101 = 78.2%, §4) — and it
   corrects the record: VS Code's native count (3 → 2) and score (19 → ≈32
   honest), Mailspring's score (23 → ~28–29 de-duplicated), Insomnia's IPC
   inventory (268 → ~285 real) and score (46 → 48 without the one false
   positive). The v3 scores as computed are right; the honest scores differ.
10. **Samples are small.** Accuracy figures rest on 18–33 sampled findings
    per app. They are the honest headline, not a per-API census.
11. **The 60.0% UI carry-over is the scanner's floor**, not a per-app
    measurement (§4).
12. **"N1–N11 all closed" overstates three items** (§4): N5 (keytar),
    N6 (17 IPC registrations missed), N8 (7 groups escape the remote
    flag); N10 is unverifiable for Joplin.

## 9. What this proves — and what it does not

**Proves:**

- `e2r convert` generates a compiling Tauri 2 project from five large,
  real, messy Electron apps — 5/5 exit 0, 5/5 `cargo check`, 5/5 release
  build, with no hand-fixes to generated code.
- The generated release binaries are small and payload-independent
  (22–27 MB, 6–7 plugins, unstripped) and land inside the simulator's
  binary intervals 5/5, once like is compared with like.
- The scanner's finding counts are hand-verified at v3 (78.2% combined,
  §4 — 79/101 sampled findings correct), and the fix rounds moved every
  targeted count the way the fix predicted — N1–N4, N7, N9, N11 closed;
  N5, N6, N8 only partly held; N10 unverifiable for Joplin — plus the
  calibrate-units and renderer-pick additions.
- The calibration loop now compares like with like, and the campaign
  caught and corrected its own wrong calibration entries in public.

**Does not prove:**

- That any of the five apps *works* as a Tauri app. Nothing was launched.
- That Tauri uses less memory or starts faster than Electron for these
  apps. Nothing was measured at runtime; memory was refused as a
  prediction.
- That the shipped bundle will be smaller than the Electron installer.
  The binary is smaller; the bundle is unmeasured.
- That a migration is easy. The scores are 19–46/100 as computed
  (honest: ≈32 / 42 / 23 / ~28–29 / 48), the MANUAL shares run 4–60% of
  call sites, and the effort hints are "large" for three of the five apps.

The campaign's claim is narrower and, for that reason, defensible: the
mechanical part of the move — scan it, generate it, compile it — works on
apps of Discord's class. The human part — the MANUAL findings, the native
modules, the frontend build, the runtime — is measured, listed, and still
open.
