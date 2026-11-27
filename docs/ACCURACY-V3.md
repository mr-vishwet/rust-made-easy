# Proof campaign — v3 accuracy re-check (combined)

> **Note for repo readers:** this is the combined v3 accuracy summary,
> copied verbatim from the campaign check (2026-11-27). The per-app evidence
> files (`proof/<app>/ACCURACY-V3.md`, with the full file:line evidence) and
> the raw scans (`proof/<app>/scan.json`, `proof/v3-numbers.tsv`) live in the
> campaign working directory, not in this repo. Paths in this document are
> relative to that working directory.

- Scope: hand-verification of the v3 scanner outputs (fix round 2) for all five
  proof apps, against their real source code. This document synthesizes the five
  per-app checks; the full evidence lives in each app's `ACCURACY-V3.md`.
- Apps: VS Code (`microsoft/vscode` @ `036980bb`), Signal Desktop
  (`signalapp/Signal-Desktop` @ `01e59719`), Joplin (`laurent22/joplin` @
  `ce86e121`), Mailspring (`Foundry376/Mailspring` @ `7a8ab271`), Insomnia
  (`Kong/insomnia` @ `cb79b8d7`). Clones in `proof/<app>/`.
- Scan: `proof/<app>/scan.json` + `scan.txt` (top level = v3; v2 preserved in
  `<app>/v2/`). v3 numbers in `proof/v3-numbers.tsv`.
- Method: same as v2 — stratified sample per app (mix AUTO / ASSISTED /
  MANUAL, including the top-3 occurrence findings), each verified against the
  real source line; every v2 misclassification / missed API re-tested; every
  N1–N11 fix-round-2 claim checked against the v3 numbers. Sample sizes are
  stated per app. One exception: Mailspring's ASSISTED slice was deliberately
  targeted at the fix surface (see §1 caveat).
- Verdict rule (same as v2): a finding counts as **correct** only if the
  classification AND the suggested Tauri replacement are right AND the
  reported line is a genuine call site. "Correct (with caveat)" = classification
  right but the count, anchor line, or replacement hides something material.
- Score formula (v3, from `RESCAN-SUMMARY.md`): `score = 100 −
  min(10×natives, 30) − min(2×MANUAL_groups, 30) −
  min(max(ASSISTED_groups−5, 0), 15) − round(20 × MANUAL_occ / total_occ) −
  (10 if ipcRenderer is used with no preload file)`, clamped 0–100. Every
  app's score was recomputed from its own `scan.json` by its worker.

## 1. Per-app verdicts

| App | Sampled accuracy (v3) | v2 accuracy | Sample | Score (reported → recomputed) | Headline |
|---|---|---|---|---|---|
| VS Code | **10/18 = 55.6%** | 13/20 = 65.0% | 6/6/6 | 19 → **19 ✓** | `keytar` is a mock, not a native; new `webview` ×32 false group; `session` 82% false |
| Signal | **20/21 = 95.2%** | 19/21 = 90.5% | 7/7/7 | 42 → **42 ✓** | Cleanest app; window *sizes* mostly wrong (height-44 leak) |
| Joplin | **16/21 = 76.2%** strict; 17/21 = 81.0% with caveat | 19/21 = 90.5% | 7/9/5 | 23 → **23 ✓** | Score artifact fixed; `sharp` is union-only; no new v3 misclassifications |
| Mailspring | **16/23 = 69.6%** | 18/21 = 85.7% | 7/9/7 | 23 → **23 ✓** | N8 split real but incomplete (7 ASSISTED groups escape it); remote double-counting |
| Insomnia | **17/18 = 94.4%** | 16/18 = 88.9% | 9/4/5 | 46 → **46 ✓** | IPC 72 → 268 real (92.4% coverage); bridge calls still 0/98 |
| **Combined** | **79/101 = 78.2%** strict (80/101 = 79.2% with Joplin's caveat) | 85/101 = 84.2% | | 5/5 recompute exactly | |

**Read the combined 78.2% with care.** The samples are not the same size or
shape as v2's: Mailspring's ASSISTED slice was deliberately targeted at the
fix surface, so it contains all 7 of that app's wrong ASSISTED findings — the
worker audited the full ASSISTED population (41 groups) at **34/41 = 82.9%**
correct, and a V2-style random sample would have scored ~20–21/23, which
would put the combined figure at ~83–84/101 (≈82–83%). VS Code's drop is
real, not sampling: one new false group (`webview` ×32) plus the untouched
`session` flood. Signal and Insomnia both improved; Joplin's drop is
sampling (its sample caught two non-Electron false positives v2's sample
missed, while the known-false mass actually shrank).

By class across all five (strict): AUTO 31/36, ASSISTED 25/35, MANUAL 23/30.
The ASSISTED weakness is concentrated in Mailspring's remote-mediated
under-classification (7 of the 10 wrong ASSISTED findings) and VS Code's
`session`/`protocol` property matches.

## 2. Corrected numbers — what the scanner says vs what verification found

These are the numbers in `v3-numbers.tsv` / `docs/PROOF.md` that do not
survive hand-verification. Everything not listed here was confirmed as
reported (groups, occurrences, files scanned, IPC counts, windows, menus,
preload counts, bundle intervals, payloads — each worker checked its app's
row against `scan.json` / `simulate.json`).

| App | Claimed | Corrected | Evidence |
|---|---|---|---|
| VS Code | 3 native modules (@parcel/watcher, keytar, node-pty) | **2** — `keytar` is a `file:./packageMocks/keytar` stub, three no-op `Promise.resolve()`s ("OVERRIDE Keytar since we don't need the feature"); zero import/call sites in `src/` or `extensions/`; real credential storage is Electron `safeStorage` (`src/vs/platform/encryption/electron-main/encryptionMainService.ts:18`) | `extensions/microsoft-authentication/package.json:152`, root `package.json` (zero `keytar` mentions), `pnpm-lock.yaml` resolution |
| VS Code | score 19 | **≈32 honest** (keytar fix alone → 29). Method: correcting only the verified keytar error gives 29; correcting also the fully-audited false occurrences (MANUAL false 136 = 103 `session` + 32 `webview` + 1 `remote`; AUTO false 6; ASSISTED false 2) gives MANUAL groups 40 (still capped −30), ASSISTED 60 (still −15), MANUAL occ 101, total occ 620, share term `round(20×101/620)=3` → ≈32. Assumes no further FPs outside the sample + `session` audit | `proof/vscode/ACCURACY-V3.md` §6 |
| VS Code | MANUAL occ 237 (growth 205 → 237) | Growth is **one false group**: MANUAL `webview` `webviewPreloads.ts:187` ×32 — all 32 lines are TypeScript generics (`postNotebookMessage<webviewMessages.…>`); zero `<webview>` tags in the file or anywhere under `src/vs/workbench/contrib/notebook/` | `proof/vscode/ACCURACY-V3.md` §5 |
| VS Code | `session` class (57 groups, 126 occ) | **7/57 groups, 23/126 occ real (82% false)** — essentially unchanged from v2 (6/21 real); the typed-param fix found exactly one real group (`browserSessionPermissions.ts:185` ×2) | `proof/vscode/ACCURACY-V3.md` §2 |
| Signal | window sizes (9 windows) | **Only 1 of 9 fully correct** (300×280 loading window). Literal sizes missed: PDF 612×792, about 500×500, debug-log/call-diagnostic 700×500, sticker-creator 800×815 — all reported "?x44" or "?x?". Five of nine report height 44, but `height: 44` occurs exactly once in `app/main.main.ts` (screen-share :1376) — the scanner leaks one window's height into every `const options = {…}` window. Pre-existing in v2 (its META repeated "800×44"), not a fix-round-2 regression. Window *count* unaffected | `proof/signal/ACCURACY-V3.md` §6 |
| Joplin | 5 native modules | **4 live on desktop** — `sharp` is union-only: declared in lib/app-cli/app-mobile/tools (not app-desktop) and used in `lib/shim-init-node.ts` when provided, but the desktop entry `main-html.ts:120` never passes it, so `sharpEnabled()` is false on desktop. Score unaffected (native charge caps at 30 with 4 or 5 natives) | `proof/joplin/ACCURACY-V3.md` §5 |
| Joplin | 7 menus | **6 logical menus** — `:1029` and `:1046` are two branches of the same `updateMenu()`; "7 menus" double-counts one logical menu (new-in-v3 minor blemish) | `proof/joplin/ACCURACY-V3.md` §1 |
| Joplin | known-false mass (sample) | **6 occurrences / 227 (2.6%), 6 groups / 113** — smaller than v2's despite the lower sample score | `proof/joplin/ACCURACY-V3.md` §5 |
| Mailspring | MANUAL 205 / ASSISTED 41 | **MANUAL 212 / ASSISTED 34** if the 7 under-classified ASSISTED groups escalate (see §5). Score unchanged (capped terms; share term stays 12) | `proof/mailspring/ACCURACY-V3.md` §3 |
| Mailspring | score 23 | **~28–29 de-duplicated** — 71 `remote` roll-up groups (217 occ) overlap the 112 remote-mediated underlying findings (196 occ); de-duplicated, the manual share is ~31–34% | `proof/mailspring/ACCURACY-V3.md` §6 |
| Mailspring | quickpreview window 320×320 known | **≈414×414** — real size is `ThumbnailWidth = 320 * (11/8.5)`; const-resolution dropped the multiplier. REGRESSION vs v2 (which honestly reported 800×600-unknown) | `app/src/quickpreview/index.ts:336` |
| Mailspring | `remote` count `error-logger.js:6` ×2 | **×1** — +1 overcount with no new source line | `proof/mailspring/ACCURACY-V3.md` §4 |
| Mailspring | `screen` `app-env.ts:501` ×6 | **×4 genuine** (+2, pre-existing from v2) | `proof/mailspring/ACCURACY-V3.md` §3 |
| Insomnia | IPC 268 channels | **~285 real** — 207/224 main-side registrations listed (17 missing = 92.4% coverage), + 61 renderer-side entries. The 268 is a lower bound, short by the 17 in §6 | `proof/insomnia/ACCURACY-V3.md` §5 |
| Insomnia | score 46 | **48 without the one false positive** — the I-18 `session` prose group (MANUAL 14 → 13, −28 → −26) is the only score-moving error; the missed IPC/bridge items are AUTO and cannot move the score. So 46 is, if anything, 2 points too harsh | `proof/insomnia/ACCURACY-V3.md` §5 |
| Insomnia | preload "3 + 1 module" | **3 real files; the "+1 module" is `entry.preload.ts` double-counted** | `proof/insomnia/ACCURACY-V3.md` §3 |

**Score honesty, app by app:** all five reported scores recompute exactly
from `scan.json` — the formula is implemented correctly everywhere. The
*inputs* differ in honesty: VS Code 19 is the least honest (spurious −10 for
a mock native + a false-occurrence flood inflating the share term; honest
≈32); Mailspring 23 understates ease (remote double-counting; de-duplicated
~28–29); Insomnia 46 is 2 points too harsh; Signal 42 and Joplin 23 are the
most honest as reported.

## 3. Fix round 2 (N1–N11) — did each fix show up?

| Item | Claim (RESCAN-SUMMARY.md) | Verdict | Evidence |
|---|---|---|---|
| N1 phantom preloads | VS Code 7 → 3; Mailspring print-preload dropped (4 → 2+1); Insomnia 6 → 3+1 | **CLOSED, all apps** | VS Code: 3 real, phantoms confirmed absent on disk. Mailspring: `print/lib/print-preload.js` gone; 2 files + 1 module all exist, all contain `contextBridge`. Insomnia: exactly 3 real `.ts` files, 0 phantom paths |
| N2 VS Code identity | code-oss-dev 1.149.0 / Electron 43.8.10 / `out/main.js` | **CLOSED** | Verified against root `package.json` + `product.json`; Electron from `nativeBuild.target` |
| N3 test exclusion | Signal files 3,065 → 2,633, windows 20 → 9; VS Code fixture window gone (5 → 4); Joplin/Insomnia test findings gone | **CLOSED** | Signal: the 11 test windows still exist on disk (`ts/test-node/app/SystemTrayService_test.main.ts:60–257`) but contribute 0 findings; all 9 reported windows real `new BrowserWindow` in `app/main.main.ts`. VS Code: 0 findings under `test/`, `.vscode/`, `/spec`. Joplin: both v2 `testing/` findings gone. Insomnia: 0 test-path findings; Express `app.get` FPs gone |
| N4 Signal preload | 603 → 26 entry points + 466 modules | **CLOSED** | All 26 exist on disk (24 `*.sandbox.ts` = exactly the on-disk count, + `preload.preload.ts` + `preload.wrapper.ts`); all 466 modules exist and are `*.preload.*`; the 115 unlisted `*.preload.*` files on disk are all test files (82 test-electron, 32 test-node, 1 `preload_test`) — complete, not an undercount |
| N5 natives | Insomnia +libcurl; VS Code +@parcel/watcher | **CLOSED as implemented; WRONG in substance for VS Code** | Insomnia: libcurl 3.3.2 in 3 package.jsons, node-pre-gyp postinstall `--runtime=electron --target=43.2.0`, really imported/used (`main/api.protocol.ts:5`, `libcurl-promise.ts`). VS Code: @parcel/watcher real and used; `keytar` is the mock in §2 — the native list was already wrong in v2 (keytar, node-pty) and fix round 2 added a third charge without questioning the second |
| N6 Insomnia | IPC 72 → 268; `<webview>` ×2; updater split; preload phantoms; window 1280×720 | **MOSTLY CLOSED** | IPC: 268 channels real, 47 `git.*`, wrapper definitions no longer counted — but 17 registrations missing (92.4% coverage; §6). `<webview>` ×2 yes (`response-web-view.tsx:29`, `window-utils.ts:245`). Updater split yes (Electron `autoUpdater` ×7 exact, `electron-updater` ×9 exact, imports `updates.ts:6` vs `:7`). Preload phantoms yes. Window 1280×720 yes (consts `window-utils.ts:35-38`, min 500×400 recorded) |
| N7 score formula | New formula; scores 19 / 42 / 23 / 23 / 46 discriminate | **CLOSED** | All five recompute exactly from their own `scan.json` (§1). The Joplin 0 → 23 move is the formula, not the app (v2 formula on v3 counts gives 0) |
| N8 Mailspring remote split | `Finding.remote_mediated: bool`; remote-manual groups 0 → 112 | **PARTLY HELD — "CLOSED" overstates it** | Flag is real: 14/14 sampled remote-mediated MANUAL findings genuinely go through `@electron/remote`; 12/12 sampled non-remote MANUAL findings genuinely have no Tauri equivalent and are genuinely non-remote. But 7 ASSISTED groups (11 occ) escape the flag because the detector only fires on a single-line `require('@electron/remote').<Api>` pattern — the 3 missed `Menu` sites are the only two-line ones, each the sole `Menu` use in its file — plus 1 flag error (`getCurrentWindow` `app-env.ts:492` is MANUAL with `remote_mediated: false` although `:493` is literally `return require('@electron/remote').getCurrentWindow();`) |
| N9 dep-as-finding | No `package.json:1` findings; deps in data fields | **CLOSED** | Zero `package.json:1` findings in all five v3 scans |
| N10 dev-script warning | Workspace-aware search; Insomnia `start:dev-server` found | **CLOSED in code; 1 app unverifiable** | Insomnia: warning gone, `start:dev-server` found. Mailspring: gone from v3 `simulate.txt`. VS Code: no such warning among the 33 in v3 `simulate.json`. Joplin: **NOT VERIFIABLE** — `scan.json` carries no warnings field; `META.md` records warnings 20 → 18 without naming them |
| N11 dev URL | Joplin 41184 → none; Mailspring 47123 → none; VS Code 4443 → none; Insomnia keeps 3334 | **CLOSED** | Joplin: `dev_url` null; `http://localhost:41184` exists only as curl examples in `packages/app-cli/app/command-apidoc.ts:89`, not a dev server; `start` runs `electron .` and the window loads `file://…/index.html`. Mailspring: `dev_url` null; 47123 lives only in redteam example views + one spec; no real dev server (only a dynamic-port MCP loopback listener). VS Code: `dev_url` null; 4443 is the Blackbird scenario-automation server, 33418 is `DEFAULT_AUTH_FLOW_PORT` (`oauth.ts:946`) — both correctly excluded. Insomnia: `http://localhost:3334` real three ways (`pkg.dev['dev-server-port']` = 3334, vite writes `.vite-port`, `window-utils.ts:294-298` reads it with a 3334 fallback) |

## 4. Renderer picks (Addition 2) — all 5 correct now; the 60% is a floor

The v2 mis-picks are all fixed: VS Code `src/`, Signal `ts/`
(sticker-creator confirmed a separate sub-app with its own `main.tsx`/React
root, serving one window only), Joplin `packages/app-desktop/gui/` (523
files / 161 tsx; `index.html` → `main-html.bundle.js` from `main-html.ts`,
which requires `./gui/Root`; `packages/pdf-viewer` is an 18-file library
consumed by `gui/`, correctly NOT the renderer), Mailspring whole `app/`
(526 touch points), Insomnia `packages/insomnia/src/` (incl.
`entry.client.tsx`).

**But the 60.0% UI carry-over is the scanner's floor, not a measurement.**
All five v3 apps report exactly 60.0% — it carries no per-app information,
and the v2→v3 "drops" (Signal 79 → 60, Joplin 67 → 60, Insomnia 94 → 60)
are measurement corrections, not regressions (v2 measured the wrong files).
Two pick-quality caveats:

- **VS Code:** `src/` is still the whole tree; 126/1,181 touch points
  (10.7%) are `electron-main/` files, bootstrap/server entries, or root
  config/test files, and bundler "vite" is a substring artifact (no vite
  dep/config; the real build is gulp+rspack). The 60.0% is honest only as
  the touch-point factor's floor.
- **Insomnia:** "238 touch points in renderer sources" breaks down as 157
  in `src/main/`, 26 in `src/sync/`, 8 in `__mocks__/` (test mocks), and
  only **7** in true renderer files (`ui/` 5 + `routes/` 2,
  `entry.client.tsx` 0). The 60% floor itself is defensible; the 238 figure
  does not measure renderer touch.

## 5. Every wrong finding (all five apps, with file:line evidence)

Verdict rule as in §0. "Survives from v2" means the same false finding was
already listed in that app's `ACCURACY-V2.md` and is still present in v3.

### VS Code — 8 wrong in sample + 1 extra spot-check

| Finding | Evidence | Why it is wrong |
|---|---|---|
| AUTO `app` — `extensions/copilot/src/extension/chatSessions/copilotcli/node/copilotcliSession.ts:2162` | String literal "…steer from the GitHub mobile app." | String, not Electron's `app`. Survives from v2 |
| AUTO `screen` — `extensions/terminal-suggest/src/completions/upstream/less.ts:240` | Comment prose "fifth line on the screen." | Comment, not Electron's `screen`. Survives from v2 |
| AUTO `app` — `extensions/copilot/src/extension/prompts/node/agent/openai/gpt53CodexPrompt.tsx:117` | Doc-comment example `src/app.ts` | Comment, not Electron's `app`. Survives from v2 |
| ASSISTED `protocol` — `src/vs/platform/agentHost/common/imageGenerationConfiguration.ts:70` | `.protocol.description`, a config-schema property | Property name, not Electron's `protocol`. Survives from v2 |
| MANUAL `webview` — `src/vs/workbench/contrib/notebook/browser/view/renderers/webviewPreloads.ts:187` ×32 | All 32 lines are TypeScript generics (`postNotebookMessage<webviewMessages.…>`); zero `<webview>` tags in the file or anywhere under `src/vs/workbench/contrib/notebook/` | Generic type parameter, not a `<webview>` tag. **NEW in v3, the worst single item** — 100% of the v3 MANUAL occurrence growth (205 → 237) |
| MANUAL `session` — `extensions/debug-server-ready/src/extension.ts:72` ×14 | VS Code debug-session object | Not Electron session. Survives from v2 |
| MANUAL `session` — `extensions/github-authentication/src/githubServer.ts:236` ×7 | Auth-session object | Not Electron session. Survives from v2 |
| MANUAL `remote` — `src/vs/platform/browserView/electron-main/browserViewMainService.ts:388` | Internal remote-connection service; `grep @electron/remote` over src+extensions: zero hits | Not `@electron/remote`. Survives from v2 |
| ASSISTED `shell` — `extensions/copilot/src/extension/prompts/node/panel/terminal.tsx:117` (spot-check, outside the 18) | React prop `this.props.shellType` in a prompt template | Prop name, not Electron's `shell` |

Also: 2 IPC direction-label errors (`invoke` reported as `send`:
`preload.ts:85`, `window.ts:1162`); the menu is still the Copilot fixture
(`simulationWorkbenchMain.js:54`, flagged app-menu) while the real menu is
dynamic (`menubar.ts` builds `new Menu()` incrementally,
`setApplicationMenu(menu)` at `:400`; no static template exists).

### Signal — 3 wrong

| Finding | Evidence | Why it is wrong |
|---|---|---|
| AUTO `app` — `ts/util/relaunch.main.ts:12` ×2 | Both occurrences are `app.relaunch()` (:12 and :20; file's own comment at :9 says `app.relaunch()` doesn't work in AppImage) | No Tauri equivalent; classified AUTO. Pre-existing — the identical finding is in the v1 and v2 scans; this sample is the first to catch it |
| AUTO `app` — `ts/background.preload.ts:614` ×2 (outside sample, v2 re-test) | Line 614 is `window.reduxStore?.getState().app.hasInitialLoadCompleted`, a Redux state slice | Not Electron's `app`. Still wrong in v3 |
| ASSISTED `protocol` — `ts/util/signalRoutes.std.ts:90` ×1 (outside sample, v2 re-test) | Line 90 is `strictAssert(protocol.endsWith(':'), …)`, a URL-scheme string parameter | Not Electron's `protocol`. Still wrong in v3 |

Two "Correct (with caveat)": generic `app` at `app/main.main.ts:229` ×25
still swallows `app.relaunch` (:2038) and
`app.runningUnderARM64Translation` (:3102 — no member findings exist for
either); `session` at `app/permissions.std.ts:33` ×3 is real but anchored on
a type annotation (`Parameters<typeof ElectronSession…>[0]`), real call sites
at :85/:87.

### Joplin — 4 wrong in sample + 2 outside

| Finding | Evidence | Why it is wrong |
|---|---|---|
| AUTO `screen` — `packages/app-clipper/popup/src/index.js:120` | `screen.height` is the Web `window.screen` global in a browser-extension popup | Not Electron's `screen`. Survives from v2 |
| ASSISTED `dialog` — `packages/app-mobile/components/Modal.tsx:259` | `dialog => dialog.setAttribute('autofocus','true')` is a DOM `<dialog>` element parameter (Safari workaround); file is the React Native app | Not Electron's `dialog`. Survives from v2 |
| ASSISTED `nativeImage` — `packages/app-mobile/services/plugins/PlatformImplementation.ts:88` | `public get nativeImage(): any { return null; }`, a platform-shim stub | Not Electron's `nativeImage`. Present in v2 but unlisted in v2 §4 |
| MANUAL `session` — `packages/server/src/routes/index/terms.ts:22` | Prose: "This website uses cookies to save login session." | String-literal prose. Survives from v2 |
| AUTO `screen` — `packages/app-mobile/components/screens/UpgradeSyncTargetScreen.tsx:56` (outside sample) | UI string "Please press Back to exit this screen." | String, not Electron's `screen`. Survives from v2 |
| ASSISTED `nativeImage` — `packages/lib/services/plugins/BasePlatformImplementation.ts:36` (outside sample) | Base getter that throws `Not implemented: nativeImage` | Same property-name-matching bug as the PlatformImplementation stub |

One "Correct (with caveat)": MANUAL `app.setName` —
`packages/app-desktop/main.ts:25` — classification defensible for runtime,
but "no direct target" hides that the name is settable in `tauri.conf.json`
(same caveat as v2).

### Mailspring — 7 wrong, all under-classified ASSISTED → should be MANUAL

Common mechanism: the detector only fires on a single-line
`require('@electron/remote').<Api>` pattern. The 7 fixed `Menu` sites all
use that form; the 3 missed are the only two-line ones, each the sole `Menu`
use in its file.

| Finding | Evidence | Why it is wrong (classification) |
|---|---|---|
| `Menu` — `app/internal_packages/contacts/lib/AddContactToolbar.tsx:41` | `require('@electron/remote')` + newline + `.Menu.buildFromTemplate(...)` | Remote-mediated; should be MANUAL, not ASSISTED |
| `Menu` — `app/internal_packages/main-calendar/lib/core/event-panel.tsx:642` | Same two-line chained remote form | Same |
| `Menu` — `app/internal_packages/thread-list/lib/thread-list-context-menu.ts:413` | Same two-line chained remote form | Same |
| `webContents` ×2 — `app/internal_packages/print/lib/print-window.ts:92` | Window created from remote `BrowserWindow` (`:9` destructures remote; `:80` is itself MANUAL) | Same |
| `webContents` — `app/src/components/evented-iframe.tsx:318` | `AppEnv.getCurrentWindow().webContents.copy()` (wrapper is remote, `app-env.ts:493`) | Same |
| `webContents` — `app/src/app-env.ts:846` | `this.getCurrentWindow().webContents.isDevToolsOpened()` (wrapper defined same file `:492–493`) | Same |
| `webContents` ×4 — `app/src/quickpreview/index.ts:206` | All 4 sites on remote-created windows (`:233`, `:335`; menu itself remote at `:263`) | Same |

Escalating the 7 moves MANUAL 205 → 212 / ASSISTED 41 → 34 but does not
move the score (capped terms; share term stays 12).

### Insomnia — 1 wrong

| Finding | Evidence | Why it is wrong |
|---|---|---|
| MANUAL `session` — `packages/insomnia/src/ui/components/example-openapi-specs.ts:539` | Line 539 is OpenAPI example prose (`summary: Logs out current logged in user session.`), matched on `user session.` in a renderer component with no Electron session use | String-literal prose. Same FP as v2 I-18. The only score-moving error in the campaign's cleanest app (without it, 46 → 48) |

## 6. New misclassifications introduced by fix round 2

| App | New in v3 | Detail |
|---|---|---|
| VS Code | `webview` ×32 false group (§5) | The new `webview` mapping matches the bare identifier in TypeScript generics. It is 100% of the MANUAL occurrence growth |
| VS Code | `keytar` charged as a native | Not new in v3 (keytar was in v2's native list too) — but fix round 2's N5 added @parcel/watcher without questioning the existing entry, so the spurious −10 now sits inside a "CLOSED" claim |
| Mailspring | Quickpreview window size regression | 800×600-unknown (v2, honest) → 320×320-known (v3, wrong); real ≈414×414 — const-resolution dropped the `* (11/8.5)` multiplier |
| Mailspring | `remote` anchor moved (`window-event-handler.ts:186` → `:120`) | Anchor is now a derived-object definition, not a remote call (count ×26 itself decomposes exactly, 14+9+2+1) |
| Mailspring | `remote` count `error-logger.js:6` ×1 → ×2 | +1 overcount with no new source line |
| Insomnia | Multi-line wrapper calls missed (17 registrations) | The new wrapper counter extracts the channel only when it is on the same line as the call. Missing: `ipc/main.ts` handle ×8 (`:455,464,492,507,587,598,748,944`) + on ×1 (`:673` `trackAnalyticsEvent`); `git-service.ts` handle ×3 (`:4255,4350,4369`); `ipc/electron.ts` on ×2 (`:339,501`); plus the `once` items below. The two biggest findings undercount (48 vs 56 real, 47 vs 50 real) |
| Insomnia | `ipcMainOnce` / `ipcMain.once` dropped entirely (3 sites) | `entry.main.ts:272,450` (`halfSecondAfterAppStart` ×2), `mcp/oauth-client-provider.ts:90` (`mcp.authConfirmed`). No `once` direction exists in scan output |
| Insomnia | UI touch points misattributed to the renderer | 157/238 in `src/main/` (§4) |
| Joplin | Menu double-count (minor) | 7 sites, 6 logical menus (§2) |
| Signal | None | v2→v3 diff is exactly +5 restored groups (all 5 lost `session`/`protocol` groups, each verified real) / −6 removed (test findings, Node-`net` false positives, `electron-builder` dep finding) — every move correct in direction |
| Joplin | None | All 5 added/changed groups (2× `MenuItem`, `electron-updater` rename, `protocol`/`session` restore) are correct |

Net: fix round 2 introduced 3 material new error classes (VS Code `webview`
generics match, Insomnia multi-line/`once` wrapper gaps, Mailspring
quickpreview size) against a much larger set of correct fixes. The
Insomnia gaps are incompleteness inside a large net improvement (IPC 72 →
268, 92.4% of registrations); the VS Code `webview` group is the only new
error that moves a headline number (MANUAL occ +32).

## 7. v2 items — fixed vs still open

**Fixed in v3 (verified):** VS Code `net.fetch` restored (`:1195` ×3, count
exact), `protocol` `browserSession.ts:432` ×1 (real), `session`
`browserSessionPermissions.ts:185` ×2 (real); Signal's 5 lost
`session`/`protocol` groups restored; Joplin `protocol`/`session` at
`handleCustomProtocols.ts:167` ×2 each (typed-`Session` parameter gate
works); Insomnia B2 dynamic `webContents.send`, B4 updater alias, B5 all 5
known string FPs gone, B7 2nd context menu (`ipc/electron.ts:512`) now in
the 3-menu inventory, B8 identity, B10 preload phantoms, B11 window sizes,
B13 test exclusion (0 test-path findings), B14 dev-script warning, B15
webview, S7 (still ×5 `app.getVersion`), S4 (updater split), the `net.fetch`
regression (gitlab ×7 + github ×5 back — `electron/main` import now
recognized), `utilityProcess` ×4 / `MessageChannelMain` / `webUtils` /
`handleOnce` ×3 all still present; Mailspring `requestSingleInstanceLock`
now AUTO ×2, safeStorage/Menu/webContents trio escalated, session FPs still
gone, 0 test/spec findings.

**Still open from v2 (unchanged):** VS Code `session` flood (82% false),
string/comment `app`/`screen` FPs, `remote` FP, Copilot-fixture menu,
`app.relaunch` swallowed by generic AUTO `app`; Signal `app.relaunch`
(no Tauri equivalent, AUTO), Redux `app` FP, `protocol` string-param FP,
window sizes (§2); Joplin `screen`/`dialog`/`nativeImage`/`session` FPs
(§5), `app.setName` mapping caveat; Mailspring spawned native binaries
invisible (`mailsync/` submodule is **empty** in this clone — never
initialized — so the binaries could not be verified either way);
Insomnia bridge-mediated renderer calls — **all 3 v2 sites still exist**
(`editors/body/graph-ql-editor.tsx:421`,
`cloud-credential-modal/file-picker.tsx:20`,
`response-multipart-viewer.tsx:83,97`) with zero findings; total scale is
**98 bridge sites** (25 `window.dialog.*` + 13 `window.app.*` + 60
`window.shell/clipboard/path.*` in `ui/`+`routes/`), and only 2 findings
exist in `src/ui/` at all; Insomnia `partition: sessionId`
(`authorize-user-in-window.ts:41`) still unflagged, `app.relaunch` ×4 still
has no own API, `app.on` ×8 still generic, `@sentry/electron` ^7.17.0 still
has zero mentions in the scan.

## 8. Verified accuracy — summary

- **Sampled findings: 79 correct / 101 checked = 78.2%** (v2: 85/101 =
  84.2%; 80/101 = 79.2% counting Joplin's one caveat as correct). See §1 for
  why the combined figure understates the scanner: Mailspring's targeted
  sample and VS Code's single new false group.
- **Scores: 5/5 recompute exactly** from `scan.json`. The formula is right;
  the inputs are not all right (§2). Honest scores, with stated methods:
  VS Code ≈32 (not 19), Mailspring ~28–29 de-duplicated (not 23),
  Insomnia 48 (not 46), Signal 42, Joplin 23.
- **Structural wins that the samples understate:** identity fixed (N2);
  phantom preloads gone everywhere (N1); test exclusion works everywhere
  (N3); Signal preload list exact (N4); libcurl found (N5); Insomnia IPC
  inventory 72 → 268 with 47 `git.*` channels (N6); all 5 renderer picks
  correct (Addition 2); all 3 bogus dev URLs gone and Insomnia's kept one
  verified (N11); dep-as-finding gone everywhere (N9); the v2 `net.fetch`
  regression fixed; Joplin's typed-`Session` gate works.
- **Structural losses (unchanged from v2):** the `session` string/prose
  flood (VS Code 82% false, Joplin's survivors); generic `app` swallowing
  `app.relaunch` / `app.on` members (VS Code, Signal, Insomnia); the
  Copilot-fixture menu (VS Code); bridge-mediated renderer calls (Insomnia,
  98 sites, 0 findings); spawned native binaries invisible (Mailspring);
  the 60% UI carry-over is a floor, not a measurement (§4).
- **Structural losses (new in v3):** §6 — the VS Code `webview` ×32 group,
  Insomnia's multi-line/`once` wrapper gaps, Mailspring's quickpreview size
  regression.
- **What this means for `docs/PROOF.md`:** the campaign's headline claims
  survive — 5/5 convert, 5/5 cargo check, 5/5 release build, v3 scores
  19/42/23/23/46 as *computed* — but three PROOF.md-adjacent numbers need
  the corrections in §2 before they are quoted as measurements: VS Code's
  native count (3 → 2) and score (19 → ≈32 honest), Mailspring's score
  (23 → ~28–29 de-duplicated), and Insomnia's IPC inventory (268 → ~285
  real). The "N1–N11 all closed" line in `RESCAN-SUMMARY.md` overstates
  N5 (keytar), N6 (17 IPC registrations missing), and N8 (7 groups escape
  the flag); N10 is unverifiable for Joplin. The calibration story
  (5/5 binaries inside binary intervals) is untouched by this check — it
  was verified separately and no v3 accuracy finding bears on it.

## 9. Fix round 3 backlog (consolidated, prioritized; detail in each app's `ACCURACY-V3.md` §6)

1. **Native-module list must verify, not just match names.** VS Code's
   `keytar` mock (−10 spurious) and Joplin's union-only `sharp` both come
   from trusting `package.json` declarations. Fix: resolve the dep to its
   tarball/path and require a real import/call site (or a real native
   binding) before charging.
2. **Blank string-literal / comment / prose contents before non-IPC
   patterns.** Still the largest false-positive class: VS Code `session`
   (103 false occ), Joplin's `screen`/`dialog`/`nativeImage`/`session`
   survivors, Insomnia's I-18 prose FP, Signal's Redux `app` and
   `protocol` string-param FPs. The per-package Electron gate (require an
   `electron` import, or bridge attribution, for bare `app.`/`session.`
   matches) is the open fix.
3. **Insomnia IPC wrapper extraction: multi-line calls + `once`
   direction.** 17 registrations missed, 3 `once` sites dropped. Fix:
   parse the call across lines (channel argument may be on a following
   line) and add an `once` direction.
4. **Insomnia bridge-mediated renderer calls (oldest open item, largest
   by site count).** 98 sites, 0 findings. Fix: attribute
   `window.<key>.*` calls in renderer files to the preload bridge key's
   Electron API (the preload's `exposeInMainWorld` map already gives the
   key → API correspondence).
5. **Mailspring remote detection: multi-line `require` + flag audit.**
   The single-line pattern misses the only two-line `Menu` sites; the
   `getCurrentWindow` flag error shows the flag needs a second pass.
   De-duplicate the `remote` roll-up groups against the remote-mediated
   underlying findings (score would move 23 → ~28–29).
6. **`webview` matching must require a tag, not a bare identifier.**
   VS Code's ×32 generics FP. Fix: match `<webview[ />]` (or the
   `webviewTag` preference), never the bare word in type position.
7. **Window-size resolution.** Signal's height-44 leak (one window's
   height into every `const options` window) and Mailspring's dropped
   multiplier (320×320 vs ≈414×414). Fix: resolve sizes per-window from
   the literal in scope; never carry a value across windows; evaluate
   constant arithmetic (`320 * (11/8.5)`).
8. **`app` member breakout.** `app.relaunch` (VS Code, Signal, Insomnia)
   and `app.on` per-event splits (Insomnia ×8, Signal ×25 group) — generic
   AUTO `app` still hides members with no Tauri equivalent.
9. **UI touch-point scoping.** Insomnia's 238 figure counts `src/main/`
   and test mocks as renderer touch; VS Code's `src/` pick still includes
   `electron-main/` and test files. Fix: scope touch points to the
   renderer entry's reachable files, and report the 60% floor as a floor.
10. **`session` `partition:` flag + `@sentry/electron` listing +
    generated-main annotation + warnings in `scan.json`.** Insomnia
    `partition: sessionId` still unflagged; `@sentry/electron` unlisted;
    `main.bundle.js` / `entry.main.min.js` / `out/main.js` / `bundles/main.js`
    are build outputs absent from the clones — the scanner should say so;
    Joplin's N10 could not be verified because `scan.json` carries no
    warnings field.
11. **Signal META.md still describes v2** (files 3,065, score 30) despite
    v3 outputs being in place — campaign-doc housekeeping, not a scanner
    bug.
