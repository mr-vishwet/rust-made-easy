//! Scan an Electron app and report how it maps to Tauri 2.
//!
//! The scanner is a regex-based X-ray, not a full JavaScript parser: it
//! reads `package.json`, walks the source tree for `.js`/`.ts` files, and
//! pattern-matches Electron API usage. That makes it fast and dependency
//! light, at the cost of occasional false positives (e.g. a local variable
//! named `app` in a file that imports Electron). Main-process API patterns
//! are only applied to files that import `electron` or a known Electron
//! ecosystem package, which keeps renderer code from polluting the report;
//! IPC and `contextBridge` patterns run on every file, because renderer
//! and preload files rarely import Electron directly.
//!
//! Comments (`//` and `/* */`) are stripped before matching, so a sentence
//! in a comment cannot produce a finding; string literals are blanked out
//! before the non-IPC API patterns run, because channel names, titles, and
//! URLs live in them (`'app.asar'`, `"…from the provider screen."`, and
//! import paths like `'./protocol.js'` must not match `app.` / `screen.` /
//! `protocol.`). IPC patterns keep string literals — channel names live
//! in strings by design. Regex literals containing quotes are handled by
//! the stripper, so a `//` after one cannot swallow the rest of the file.
//!
//! A bare-word API pattern only fires when its identifier resolves to an
//! Electron import binding in the file (see [`ElectronBindings`]): the
//! name imported from `electron` / `@electron/remote` (destructured,
//! aliased, or `require()` forms), a namespace member (`electron.dialog`),
//! or — for dependency injection / parameters — an Electron export
//! imported somewhere else in the app. A same-named local (`this.session`,
//! Testing Library `screen`, a DOM `<dialog>`, a local `app-shell`
//! wrapper) never produces a finding. Calls reached through
//! `@electron/remote` (inline `require('@electron/remote').X`,
//! destructured imports, or the `remote` namespace) are reclassified
//! MANUAL: they have no Tauri equivalent.
//!
//! Every finding is classified through the mapping table
//! ([`crate::mappings`]) and rolled into a 0–100 feasibility score.

use crate::mappings;
use crate::model::{
    Classification, DevUrlInfo, Finding, IpcChannel, MenuInfo, PreloadInfo, PreloadMember,
    PreloadMemberKind, PreloadShape, ScanReport, WindowInfo,
};
use anyhow::Result;
use regex::Regex;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

/// Dependencies that are true native addons (compiled code): they cannot
/// run in a Tauri webview or a Rust backend without a real port, so each
/// one costs 15 feasibility points.
///
/// Pure-JavaScript packages that need a port live in
/// [`PORT_NEEDING_DEPS`] instead — calling them "native" was a bug (they
/// cost 15 points each for what is usually a 1:1 plugin swap).
///
/// Detection is list-based, from `package.json` (unioned across every
/// package in the tree — see [`read_all_package_jsons`]). A dep whose
/// published tarball carries a `binding.gyp`, `prebuilds/`, or a
/// `node-gyp`/`prebuild-install` install script is a native addon even
/// when it is not on this list, but that check needs the installed
/// package (or the registry), and source clones ship neither — so an
/// unlisted native addon is still missed. Extend this list when the
/// proof campaign (or a user report) finds one.
const NATIVE_MODULES: &[&str] = &[
    "node-pty",
    "better-sqlite3",
    "better-sqlite3-multiple-ciphers",
    "@signalapp/sqlcipher",
    "@journeyapps/sqlcipher",
    "sqlite3",
    "sqlite-vec",
    "serialport",
    "usb",
    "robotjs",
    "iohook",
    "ffi-napi",
    "ref-napi",
    "keytar",
    "sharp",
    "sqlite",
    "leveldown",
    "fs-admin",
    "registry-js",
    "node-llama-cpp",
    "llama-node",
    "onnxruntime-node",
    // Proof campaign, fix round 2: `@getinsomnia/node-libcurl` is the
    // native N-API addon behind Insomnia's network stack (N5 — it was
    // declared in three `package.json`s and still cost 0 points), and
    // `@parcel/watcher` is the native file-watching binding VS Code
    // depends on. Both are compiled code, not pure JS.
    "@getinsomnia/node-libcurl",
    "@parcel/watcher",
];

/// Pure-JavaScript dependencies that still need a port when moving to
/// Tauri — but to a Tauri plugin, not to a native rewrite. They are listed
/// separately from native modules and cost no feasibility points when a
/// 1:1 plugin equivalent exists (`electron-store`, `electron-updater`,
/// `electron-window-state`); `@electron/remote` has no equivalent, so it
/// is reported as a MANUAL finding instead (see [`scan`]).
const PORT_NEEDING_DEPS: &[&str] = &[
    "electron-store",
    "electron-updater",
    "electron-window-state",
    "@electron/remote",
];

/// Build/packaging tooling. Reported as a note, not a native module:
/// Tauri replaces these with `cargo tauri build`, so there is nothing
/// to port — only the build setup to redo.
const BUILD_TOOLING: &[&str] = &["electron-builder", "electron-packager"];

/// Directories never descended into while scanning.
const SKIP_DIRS: &[&str] = &["node_modules", "dist", "build", ".git", "out"];

/// Directory segments that mark test / spec / fixture code. Files under
/// them never ship, so they contribute no findings, IPC channels,
/// windows, menus, or preload entries — and they do not count toward
/// `files_scanned` either. (Proof campaign: GitHub Desktop had 48.3% of
/// its occurrences in test files; VS Code 52.2% of its IPC channels.)
const TEST_DIR_SEGMENTS: &[&str] = &[
    "test",
    "tests",
    "spec",
    "specs",
    "__tests__",
    "fixtures",
    "playwright",
    "e2e",
    "integration-tests",
    "test-mock",
    "test-node",
    "testing",
    // Editor config, not shipped app code: VS Code vendors a
    // self-host test provider extension under `.vscode/extensions/`,
    // whose `session` uses leaked into the report (N3).
    ".vscode",
];

/// Whether a scan-relative path (`a/b/c.test.ts`, `app/test/x.js`) is
/// test / spec / fixture code: any path segment in
/// [`TEST_DIR_SEGMENTS`], a `test-*` / `*-test` segment (Signal's
/// `ts/test-node/`, Insomnia's `insomnia-test/` and
/// `insomnia-smoke-test/` packages), or a filename matching `*.test.*`,
/// `*.spec.*`, `*_test.*` (including `*_test.main.ts`), or `test-*.*`.
pub(crate) fn is_test_path(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    let segments: Vec<&str> = lower.split('/').collect();
    if segments
        .iter()
        .any(|s| TEST_DIR_SEGMENTS.contains(s) || s.starts_with("test-") || s.ends_with("-test"))
    {
        return true;
    }
    let Some(file) = segments.last() else {
        return false;
    };
    file.contains(".test.")
        || file.contains(".spec.")
        || file.contains("_test.")
        || file.ends_with("_test.js")
        || file.ends_with("_test.ts")
        || file.ends_with("_test.jsx")
        || file.ends_with("_test.tsx")
        || file.ends_with("_test.mjs")
        || file.ends_with("_test.cjs")
        || file.starts_with("test-")
}

/// Note appended to a finding whose call site is reached through
/// `@electron/remote`: the underlying API's classification is forced to
/// MANUAL, because every remote call must become an explicit `invoke()`
/// + Rust command.
const REMOTE_MEDIATED_NOTE: &str =
    "Reached through @electron/remote — no Tauri equivalent; must become an explicit invoke() + Rust command.";

/// Source file extensions that get scanned.
const SCANNABLE_EXTENSIONS: &[&str] = &["js", "ts", "jsx", "tsx", "mjs", "cjs"];

/// Maximum template source text kept for a static menu literal (~4 KiB).
const TEMPLATE_SOURCE_CAP: usize = 4096;

/// One regex-detectable Electron API usage.
#[derive(Debug, Clone, Copy)]
struct ApiPattern {
    /// API name as it appears in the mapping table.
    api: &'static str,
    /// Coarse category for the report.
    category: &'static str,
    /// Regex matched against a single source line.
    pattern: &'static str,
    /// For generic fallbacks (e.g. `app`): skip this pattern on a line
    /// where a more specific API with this prefix (e.g. `app.`) already
    /// matched, so one call site does not produce two findings.
    skip_if_prefix_matched: Option<&'static str>,
    /// Whether the pattern must run on the comment-stripped line with
    /// string literals intact (only for patterns whose signal *is* a
    /// string: `'second-instance'`, `require('electron-window-state')`).
    /// Every other pattern runs on the string-stripped line, so a match
    /// inside `'app.asar'` or `"…from the provider screen."` cannot
    /// produce a finding.
    match_in_strings: bool,
    /// Whether the receiver identifier must resolve to an Electron
    /// import binding in the file (see [`ElectronBindings`]). `false`
    /// for patterns whose signal is not an identifier: `nodeIntegration`
    /// / `contextIsolation` (object keys) and `electron-window-state`
    /// (a `require()` call).
    needs_binding: bool,
}

/// The API pattern table. Specific patterns come before their generic
/// fallback within each family — the skip rule above depends on that order.
///
/// Bare-word patterns (`autoUpdater`, `nativeTheme`, ...) require a
/// call-site suffix (`.`, `(`, or `[`): a bare identifier also matches its
/// own import/destructure line, which is not a use — a dead import must
/// not drive a finding or a plugin inclusion.
///
/// Member-level patterns (e.g. `app.commandLine`) come before the generic
/// family fallback: the generic `app` → AUTO finding used to swallow
/// members with no Tauri equivalent (proof campaign: 18/33 `app.`
/// occurrences in VS Code's `mainImpl.ts` are `commandLine`). The same
/// split applies to `net.*` (`resolveHost`/`isOnline` are not HTTP, so
/// `reqwest` is the wrong target) and `screen.*` (`screenToDipPoint`
/// needs manual DIP math).
const API_PATTERNS: &[ApiPattern] = &[
    ApiPattern {
        api: "app.getPath",
        category: "app",
        pattern: r"\bapp\.getPath\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.quit",
        category: "app",
        pattern: r"\bapp\.quit\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.requestSingleInstanceLock",
        category: "app",
        pattern: r#"\brequestSingleInstanceLock\b|['"]second-instance['"]"#,
        skip_if_prefix_matched: None,
        match_in_strings: true,
        needs_binding: false,
    },
    ApiPattern {
        api: "app.setAsDefaultProtocolClient",
        category: "app",
        pattern: r"\bsetAsDefaultProtocolClient\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.isDefaultProtocolClient",
        category: "app",
        pattern: r"\bisDefaultProtocolClient\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.removeAsDefaultProtocolClient",
        category: "app",
        pattern: r"\bremoveAsDefaultProtocolClient\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.commandLine",
        category: "app",
        pattern: r"\bapp\.commandLine\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.dock",
        category: "app",
        pattern: r"\bapp\.dock\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.setBadgeCount",
        category: "app",
        pattern: r"\bapp\.setBadgeCount\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.setAppUserModelId",
        category: "app",
        pattern: r"\bapp\.setAppUserModelId\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.setJumpList",
        category: "app",
        pattern: r"\bapp\.setJumpList\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.enableSandbox",
        category: "app",
        pattern: r"\bapp\.enableSandbox\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.isPackaged",
        category: "app",
        pattern: r"\bapp\.isPackaged\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.runningUnderARM",
        category: "app",
        pattern: r"\bapp\.runningUnderARM\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.setLoginItemSettings",
        category: "app",
        pattern: r"\bapp\.setLoginItemSettings\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.getLoginItemSettings",
        category: "app",
        pattern: r"\bapp\.getLoginItemSettings\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.setName",
        category: "app",
        pattern: r"\bapp\.setName\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.setPath",
        category: "app",
        pattern: r"\bapp\.setPath\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.moveToApplicationsFolder",
        category: "app",
        pattern: r"\bapp\.moveToApplicationsFolder\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.isInApplicationsFolder",
        category: "app",
        pattern: r"\bapp\.isInApplicationsFolder\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.disableHardwareAcceleration",
        category: "app",
        pattern: r"\bapp\.disableHardwareAcceleration\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.getPreferredSystemLanguages",
        category: "app",
        pattern: r"\bapp\.getPreferredSystemLanguages\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.setDesktopName",
        category: "app",
        pattern: r"\bapp\.setDesktopName\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app.setAppLogsPath",
        category: "app",
        pattern: r"\bapp\.setAppLogsPath\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "app",
        category: "app",
        pattern: r"\bapp\.",
        skip_if_prefix_matched: Some("app."),
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "dialog.showOpenDialog",
        category: "dialog",
        pattern: r"\bdialog\.showOpenDialog\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "dialog.showSaveDialog",
        category: "dialog",
        pattern: r"\bdialog\.showSaveDialog\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "dialog.showMessageBox",
        category: "dialog",
        pattern: r"\bdialog\.showMessageBox\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "dialog",
        category: "dialog",
        pattern: r"\bdialog\.",
        skip_if_prefix_matched: Some("dialog."),
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "shell.openExternal",
        category: "shell",
        pattern: r"\bshell\.openExternal\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "shell.openPath",
        category: "shell",
        pattern: r"\bshell\.openPath\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "shell",
        category: "shell",
        pattern: r"\bshell\.",
        skip_if_prefix_matched: Some("shell."),
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "webContents.send",
        category: "webcontents",
        pattern: r"\bwebContents\.send\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: false,
    },
    ApiPattern {
        api: "webContents.openDevTools",
        category: "webcontents",
        pattern: r"\bwebContents\.openDevTools\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: false,
    },
    ApiPattern {
        api: "webContents.forcefullyCrashRenderer",
        category: "webcontents",
        pattern: r"\bwebContents\.forcefullyCrashRenderer\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: false,
    },
    ApiPattern {
        api: "webContents.isCrashed",
        category: "webcontents",
        pattern: r"\bwebContents\.isCrashed\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: false,
    },
    ApiPattern {
        api: "webContents",
        category: "webcontents",
        pattern: r"\bwebContents\.",
        skip_if_prefix_matched: Some("webContents."),
        match_in_strings: false,
        needs_binding: false,
    },
    ApiPattern {
        api: "Tray",
        category: "tray",
        pattern: r"\bnew\s+Tray\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "Notification",
        category: "notification",
        pattern: r"\bnew\s+Notification\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "WebContentsView",
        category: "webcontents",
        pattern: r"\bnew\s+(?:electron\.)?WebContentsView\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "MessageChannelMain",
        category: "ipc",
        pattern: r"\bnew\s+MessageChannelMain\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "MenuItem",
        category: "menu",
        pattern: r"\bnew\s+MenuItem\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "Menu.setApplicationMenu",
        category: "menu",
        pattern: r"\bMenu\.setApplicationMenu\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "globalShortcut",
        category: "shortcut",
        pattern: r"\bglobalShortcut[\.\(\[\]]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "protocol",
        category: "protocol",
        pattern: r"\bprotocol\.",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "session",
        category: "session",
        pattern: r"\bsession\.",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "safeStorage",
        category: "storage",
        pattern: r"\bsafeStorage[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "nativeTheme",
        category: "theme",
        pattern: r"\bnativeTheme[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "autoUpdater",
        category: "updater",
        pattern: r"\bautoUpdater[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "clipboard",
        category: "clipboard",
        pattern: r"\bclipboard\.",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "screen.screenToDipPoint",
        category: "screen",
        pattern: r"\bscreen\.screenToDipPoint\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "screen.screenToDipRect",
        category: "screen",
        pattern: r"\bscreen\.screenToDipRect\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "screen",
        category: "screen",
        pattern: r"\bscreen\.",
        skip_if_prefix_matched: Some("screen."),
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "powerMonitor",
        category: "power",
        pattern: r"\bpowerMonitor[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "crashReporter",
        category: "crash",
        pattern: r"\bcrashReporter[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "net.resolveHost",
        category: "net",
        pattern: r"\bnet\.resolveHost\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "net.isOnline",
        category: "net",
        pattern: r"\bnet\.isOnline\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "net.fetch",
        category: "net",
        pattern: r"\bnet\.fetch\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "net.request",
        category: "net",
        pattern: r"\bnet\.request\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "net",
        category: "net",
        pattern: r"\bnet\.",
        skip_if_prefix_matched: Some("net."),
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "nativeImage",
        category: "image",
        pattern: r"\bnativeImage[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "remote",
        category: "remote",
        pattern: r"\bremote\.",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "getGlobal",
        category: "remote",
        pattern: r"\bgetGlobal\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "getCurrentWindow",
        category: "remote",
        pattern: r"\bgetCurrentWindow\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "getCurrentWebContents",
        category: "remote",
        pattern: r"\bgetCurrentWebContents\s*\(",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "utilityProcess",
        category: "process",
        pattern: r"\butilityProcess[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "desktopCapturer",
        category: "capture",
        pattern: r"\bdesktopCapturer[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "systemPreferences",
        category: "system",
        pattern: r"\bsystemPreferences[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "webFrame",
        category: "webcontents",
        pattern: r"\bwebFrame[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "webUtils",
        category: "webcontents",
        pattern: r"\bwebUtils[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "contentTracing",
        category: "system",
        pattern: r"\bcontentTracing[\.\(\[]",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: true,
    },
    ApiPattern {
        api: "electron-window-state",
        category: "window",
        pattern: r#"require\s*\(\s*['"]electron-window-state['"]\s*\)|from\s+['"]electron-window-state['"]"#,
        skip_if_prefix_matched: None,
        match_in_strings: true,
        needs_binding: false,
    },
    ApiPattern {
        api: "nodeIntegration",
        category: "security",
        pattern: r"\bnodeIntegration\s*:\s*true\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: false,
    },
    ApiPattern {
        api: "contextIsolation",
        category: "security",
        pattern: r"\bcontextIsolation\s*:\s*false\b",
        skip_if_prefix_matched: None,
        match_in_strings: false,
        needs_binding: false,
    },
];

/// All compiled regexes for one scan, built once and reused per file.
struct Patterns {
    electron_import: Regex,
    ipc_main: Regex,
    ipc_main_dynamic: Regex,
    ipc_renderer: Regex,
    ipc_renderer_dynamic: Regex,
    webcontents_send: Regex,
    webcontents_send_dynamic: Regex,
    expose_call: Regex,
    browser_window: Regex,
    menu_template: Regex,
    preload_path: Regex,
    width: Regex,
    height: Regex,
    window_state_width: Regex,
    window_state_height: Regex,
    min_width: Regex,
    min_height: Regex,
    background_color: Regex,
    icon_direct: Regex,
    icon_join: Regex,
    icon_concat: Regex,
    title: Regex,
    resizable: Regex,
    frame: Regex,
    dev_url_literal: Regex,
    dev_url_host: Regex,
    remote_require: Regex,
    set_preload_scripts: Regex,
    window_assign: Regex,
    import_named: Regex,
    import_namespace: Regex,
    require_destructure: Regex,
    require_namespace: Regex,
    require_member: Regex,
    // Local-shadow detection (see [`local_shadow_names`]): names a file
    // declares itself, which shadow an Electron export of the same name.
    shadow_decl: Regex,
    shadow_destructure: Regex,
    shadow_fn_class: Regex,
    shadow_import: Regex,
    shadow_typed_param: Regex,
    api_patterns: Vec<(ApiPattern, Regex)>,
}

impl Patterns {
    fn new() -> Self {
        // The gate for main-process patterns: `electron` itself plus the
        // known ecosystem packages, so a file that drives updates via
        // `electron-updater` (without importing `electron`) is not
        // invisible to the API rules. The `electron/main` (and
        // `/renderer`, `/common`) submodule specifiers count as
        // `electron` too — see [`module_of`].
        const ECOSYSTEM: &str = "electron(?:/main|/renderer|/common)?|electron-updater|electron-store|electron-window-state|@electron/remote";
        Self {
            electron_import: Regex::new(&format!(
                r#"require\s*\(\s*['"]({ECOSYSTEM})['"]\s*\)|from\s+['"]({ECOSYSTEM})['"]"#
            ))
            .expect("electron import regex"),
            ipc_main: Regex::new(r#"ipcMain\.(handleOnce|handle|on)\s*\(\s*['"]([^'"]+)['"]"#)
                .expect("ipcMain regex"),
            ipc_main_dynamic: Regex::new(r#"ipcMain\.(handleOnce|handle|on)\s*\(\s*([A-Za-z_$][\w$]*)"#)
                .expect("ipcMain dynamic regex"),
            ipc_renderer: Regex::new(r#"ipcRenderer\.(invoke|send|on)\s*\(\s*['"]([^'"]+)['"]"#)
                .expect("ipcRenderer regex"),
            ipc_renderer_dynamic: Regex::new(
                r#"ipcRenderer\.(invoke|send|on)\s*\(\s*([A-Za-z_$][\w$]*)"#,
            )
            .expect("ipcRenderer dynamic regex"),
            webcontents_send: Regex::new(r#"webContents\.send\s*\(\s*['"]([^'"]+)['"]"#)
                .expect("webContents.send regex"),
            webcontents_send_dynamic: Regex::new(
                r#"webContents\.send\s*\(\s*([A-Za-z_$][\w$]*)"#,
            )
            .expect("webContents.send dynamic regex"),
            expose_call: Regex::new(r"contextBridge\.exposeInMainWorld\s*\(")
                .expect("exposeInMainWorld call regex"),
            browser_window: Regex::new(r"new\s+(?:electron\.)?BrowserWindow\s*\(")
                .expect("BrowserWindow regex"),
            menu_template: Regex::new(r"Menu\.buildFromTemplate\s*\(")
                .expect("Menu.buildFromTemplate regex"),
            // Catches both `preload: './preload.js'` and
            // `preload: path.join(__dirname, 'preload.js')`.
            preload_path: Regex::new(r#"preload\s*:.*?['"]([^'"]+\.(?:js|ts|mjs|cjs))['"]"#)
                .expect("preload path regex"),
            width: Regex::new(r"width\s*:\s*(\d+)").expect("width regex"),
            height: Regex::new(r"height\s*:\s*(\d+)").expect("height regex"),
            window_state_width: Regex::new(r"defaultWidth\s*:\s*(\d+)")
                .expect("window-state defaultWidth regex"),
            window_state_height: Regex::new(r"defaultHeight\s*:\s*(\d+)")
                .expect("window-state defaultHeight regex"),
            min_width: Regex::new(r"minWidth\s*:\s*(\d+)").expect("minWidth regex"),
            min_height: Regex::new(r"minHeight\s*:\s*(\d+)").expect("minHeight regex"),
            background_color: Regex::new(r#"backgroundColor\s*:\s*['"]([^'"]+)['"]"#)
                .expect("backgroundColor regex"),
            icon_direct: Regex::new(r#"icon\s*:\s*['"`]([^'"`]+)['"`]"#)
                .expect("icon direct regex"),
            icon_join: Regex::new(r#"icon\s*:\s*path\.join\([^,]+,\s*['"`]([^'"`]+)['"`]"#)
                .expect("icon path.join regex"),
            icon_concat: Regex::new(r#"icon\s*:\s*__dirname\s*\+\s*['"`]([^'"`]+)['"`]"#)
                .expect("icon __dirname concat regex"),
            title: Regex::new(r#"title\s*:\s*['"]([^'"]+)['"]"#).expect("title regex"),
            resizable: Regex::new(r"resizable\s*:\s*(true|false)").expect("resizable regex"),
            frame: Regex::new(r"frame\s*:\s*(true|false)").expect("frame regex"),
            dev_url_literal: Regex::new(r#"https?://(?:localhost|127\.0\.0\.1):(\d{2,5})"#)
                .expect("dev URL literal regex"),
            dev_url_host: Regex::new(r#"host\s*:\s*['"](?:localhost|127\.0\.0\.1):(\d{2,5})['"]"#)
                .expect("dev URL host regex"),
            // `@electron/remote` in either module system — the marker for
            // remote-mediated API use (see `ElectronBindings`).
            remote_require: Regex::new(
                r#"require\s*\(\s*['"]@electron/remote[^'"]*['"]\s*\)|from\s+['"]@electron/remote[^'"]*['"]"#,
            )
            .expect("remote require regex"),
            set_preload_scripts: Regex::new(r"setPreloadScripts\s*\(")
                .expect("setPreloadScripts regex"),
            window_assign: Regex::new(r"window\.([A-Za-z_$][\w$]*)\s*=\s*\{")
                .expect("window assignment regex"),
            // `import { a, b as c } from 'electron'` / `'@electron/remote'`.
            // The optional `type` covers `import type { Session } from
            // 'electron'` — a type-only import still binds the name.
            import_named: Regex::new(
                r#"import\s+(?:type\s+)?\{([^}]*)\}\s*from\s*['"]([^'"]+)['"]"#,
            )
            .expect("import named regex"),
            // `import * as electron from 'electron'` / `import electron from 'electron'`.
            import_namespace: Regex::new(
                r#"import\s+(?:type\s+)?(?:\*\s*as\s+([A-Za-z_$][\w$]*)|([A-Za-z_$][\w$]*))\s*from\s*['"]([^'"]+)['"]"#,
            )
            .expect("import namespace regex"),
            // `const { a, b: c } = require('electron')`.
            require_destructure: Regex::new(
                r#"(?:const|let|var)\s*\{([^}]*)\}\s*=\s*require\s*\(\s*['"]([^'"]+)['"]\s*\)"#,
            )
            .expect("require destructure regex"),
            // `const electron = require('electron')`.
            require_namespace: Regex::new(
                r#"(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*require\s*\(\s*['"]([^'"]+)['"]\s*\)"#,
            )
            .expect("require namespace regex"),
            // `const app = require('electron').app`.
            require_member: Regex::new(
                r#"(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*require\s*\(\s*['"]([^'"]+)['"]\s*\)\.([A-Za-z_$][\w$]*)"#,
            )
            .expect("require member regex"),
            shadow_decl: Regex::new(r#"(?:const|let|var)\s+([A-Za-z_$][\w$]*)"#)
                .expect("shadow decl regex"),
            shadow_destructure: Regex::new(r#"(?:const|let|var)\s*\{([^}]*)\}"#)
                .expect("shadow destructure regex"),
            shadow_fn_class: Regex::new(
                r#"(?:function\s+|class\s+|catch\s*\(\s*)([A-Za-z_$][\w$]*)"#,
            )
            .expect("shadow fn/class regex"),
            shadow_import: Regex::new(
                r#"import\s+(?:type\s+)?(?:\{([^}]*)\}|([A-Za-z_$][\w$]*)(?:\s*,\s*\{([^}]*)\})?|\*\s*as\s+([A-Za-z_$][\w$]*))\s*from\s*['"]([^'"]+)['"]"#,
            )
            .expect("shadow import regex"),
            // A typed parameter / property: `name: Type` where the type
            // starts uppercase or is a primitive. Group 2 is the
            // `Electron.` qualifier — an Electron type is NOT a shadow
            // (`app: Electron.App` really is Electron's app).
            shadow_typed_param: Regex::new(
                r#"\b([A-Za-z_$][\w$]*)\s*:\s*(Electron\s*\.\s*)?([A-Z][\w$.]*|string|number|boolean|any|unknown|void|null|undefined|object|symbol|bigint|never)\b"#,
            )
            .expect("shadow typed param regex"),
            api_patterns: API_PATTERNS
                .iter()
                .map(|p| (*p, Regex::new(p.pattern).expect("API pattern regex")))
                .collect(),
        }
    }
}

/// Which Electron module a `from`/`require()` specifier is: the
/// `electron` package itself, the `@electron/remote` package (whose APIs
/// are all remote-mediated), or one of the ecosystem packages that
/// re-export Electron API names (`electron-updater` et al.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModuleKind {
    Electron,
    Remote,
    Ecosystem,
}

fn module_of(spec: &str) -> Option<ModuleKind> {
    match spec {
        // The `electron/main`, `electron/renderer`, and
        // `electron/common` submodule specifiers are the same package
        // (Insomnia imports `net` from `'electron/main'`; the v2
        // identifier gate dropped those call sites).
        "electron" | "electron/main" | "electron/renderer" | "electron/common" => {
            Some(ModuleKind::Electron)
        }
        s if s.starts_with("@electron/remote") => Some(ModuleKind::Remote),
        "electron-updater" | "electron-store" | "electron-window-state" => {
            Some(ModuleKind::Ecosystem)
        }
        _ => None,
    }
}

/// The Electron import bindings of one file: which local identifiers
/// resolve to Electron exports, which to `@electron/remote` exports, and
/// which local names are the whole-module namespaces.
///
/// This is a regex/file-level approximation of identifier resolution,
/// not a type checker. It handles the forms the proof campaign actually
/// showed: ES imports (`import { x }`, `import { x as y }`,
/// `import * as electron`, default imports), destructured `require()`,
/// namespace `require()`, and member `require()` (`require('electron').app`).
/// It does NOT track re-exports (`export ... from 'electron'`), dynamic
/// `import()`, assignments through intermediaries (`const e = electron`),
/// or bindings created by a bundler. When it cannot resolve an
/// identifier, the conservative choice is to drop the finding — a
/// same-named local (`this.session`, Testing Library `screen`, a DOM
/// `<dialog>` element, a local `app-shell` wrapper) must never become an
/// Electron finding.
///
/// Two deliberate exceptions keep real usage visible:
/// * **Remote inline form.** `require('@electron/remote').dialog` on a
///   line makes every API hit on that line remote-mediated, even though
///   no local binding exists.
/// * **Dependency injection / parameters.** A file that never imports
///   Electron can still receive an Electron module as a parameter
///   (`app.requestSingleInstanceLock` in a `single-instance.js` whose
///   `app` is passed in; `globalShortcut` wired by DI). Such a file is
///   accepted when the identifier is imported from Electron *somewhere
///   else in the app* (the `global_exports` set, built in a first pass
///   over the whole tree). The identifier still has to be a real
///   Electron export name — an arbitrary local of the same name in a
///   file that never touches Electron stays dropped.
#[derive(Debug, Default, Clone)]
struct ElectronBindings {
    /// Local name → exported name, bound from `electron` (or an
    /// ecosystem package re-exporting its names).
    electron: HashMap<String, String>,
    /// Local name → exported name, bound from `@electron/remote`.
    remote: HashMap<String, String>,
    /// Local names bound to the whole `electron` module.
    electron_ns: HashSet<String>,
    /// Local names bound to the whole `@electron/remote` module
    /// (including a local literally named `remote`).
    remote_ns: HashSet<String>,
    /// Local names bound to `electron-updater`'s `autoUpdater` — a
    /// different updater from Electron's own `autoUpdater` (N6). Call
    /// sites through these names are reported as `electron-updater`.
    electron_updater_locals: HashSet<String>,
    /// Parameter / property names annotated with an Electron type
    /// (`session: Session`, `win: BrowserWindow`,
    /// `directSession: Electron.Session` — N6 follow-on from the
    /// Signal/Joplin accuracy re-checks). The identifier is an
    /// Electron object passed in, not a same-named local: the
    /// identifier gate dropped whole `session`/`protocol` findings
    /// for these. Filled by [`parse_electron_bindings`].
    typed_electron_params: HashSet<String>,
    /// The subset of [`ElectronBindings::typed_electron_params`] (plus
    /// session aliases, see [`parse_electron_bindings`]) whose type is
    /// Electron's `Session`: names a `Session` method can be called on
    /// (`directSession.setProxy(...)`). The `session.` pattern cannot
    /// see those calls — the receiver is not literally named `session`.
    typed_session_names: HashSet<String>,
}

impl ElectronBindings {
    /// Whether `name` is a local bound from Electron (as a named import
    /// or an alias whose target is `name` — `import { app as electronApp }`
    /// makes both `electronApp` and `app` resolve).
    fn resolves_electron(&self, name: &str) -> bool {
        self.electron.contains_key(name)
            || self.electron.values().any(|v| v == name)
            || self.electron_ns.contains(name)
    }

    /// Whether `name` resolves to a `@electron/remote` binding.
    fn resolves_remote(&self, name: &str) -> bool {
        self.remote.contains_key(name)
            || self.remote.values().any(|v| v == name)
            || self.remote_ns.contains(name)
    }
}

/// Names a file declares itself — locals that shadow an Electron export
/// of the same name. A bare `session` in a file whose `session` is a
/// `DraftEditingSession` parameter, a `const session = ...`, or an import
/// from a non-Electron module is the app's own session concept, not
/// Electron's `session` — even when some other file in the app imports
/// Electron's `session` (the app-wide DI fallback must not override a
/// file-local declaration).
///
/// Collected: `const`/`let`/`var` names (plain and destructured),
/// `function`/`class` names, `catch` parameters, imports from
/// non-Electron modules (named, default, namespace), and typed
/// parameters/properties (`name: Type` with an uppercase or primitive
/// type). Deliberately NOT collected: untyped function parameters — those
/// are the dependency-injection case (`setup(globalShortcut, app)` in a
/// file that never imports Electron), which must keep resolving through
/// the app-wide fallback. A type annotated `Electron.*` is likewise not
/// a shadow (`app: Electron.App` really is Electron's app).
///
/// Documented approximation: the typed-parameter regex also matches a
/// `name: Type` in an interface property or a ternary with a capitalized
/// branch — both shadow the name in that file, which can hide a real
/// bare Electron use in the same file. Rare; the false positives it
/// removes (a whole app's draft/credential/agent "sessions") dominate.
fn local_shadow_names(patterns: &Patterns, content: &str) -> HashSet<String> {
    let mut shadows = HashSet::new();
    for cap in patterns.shadow_decl.captures_iter(content) {
        shadows.insert(cap[1].to_string());
    }
    for cap in patterns.shadow_destructure.captures_iter(content) {
        for (local, _) in parse_named_list(&cap[1]) {
            shadows.insert(local);
        }
    }
    for cap in patterns.shadow_fn_class.captures_iter(content) {
        shadows.insert(cap[1].to_string());
    }
    for cap in patterns.shadow_import.captures_iter(content) {
        // Group 5 is the module (groups 1/3 are named lists, 2 the
        // default import, 4 the namespace import — see the regex).
        let module = cap.get(5).map(|m| m.as_str()).unwrap_or("");
        // Electron and @electron/remote imports are bindings, not shadows.
        if module == "electron"
            || module == "@electron/remote"
            || module.starts_with("electron/")
            || module.starts_with("@electron/remote/")
        {
            continue;
        }
        if let Some(named) = cap.get(1) {
            for (local, _) in parse_named_list(named.as_str()) {
                shadows.insert(local);
            }
        }
        if let Some(named) = cap.get(3) {
            for (local, _) in parse_named_list(named.as_str()) {
                shadows.insert(local);
            }
        }
        if let Some(default) = cap.get(2) {
            shadows.insert(default.as_str().to_string());
        }
        if let Some(ns) = cap.get(4) {
            shadows.insert(ns.as_str().to_string());
        }
    }
    for cap in patterns.shadow_typed_param.captures_iter(content) {
        // An `Electron.`-qualified type is Electron's own type, not a
        // local shadow.
        if cap.get(2).is_some() {
            continue;
        }
        shadows.insert(cap[1].to_string());
    }
    shadows
}

/// Parse one `import { a, b as c }` / `const { a, b: c } = require(...)`
/// member list into `(local, exported)` pairs.
fn parse_named_list(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for item in list.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        // ES form: `name as alias`; CJS form: `name: alias`.
        let (exported, local) = if let Some(idx) = item.find(" as ") {
            (item[..idx].trim(), item[idx + 4..].trim())
        } else if let Some(idx) = item.find(':') {
            (item[..idx].trim(), item[idx + 1..].trim())
        } else {
            (item, item)
        };
        let exported = exported.trim().trim_start_matches("type ");
        let local = local.trim();
        if !exported.is_empty()
            && !local.is_empty()
            && exported
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
            && local
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        {
            out.push((local.to_string(), exported.to_string()));
        }
    }
    out
}

/// Parse the Electron import bindings of one file's comment-stripped text.
fn parse_electron_bindings(patterns: &Patterns, content: &str) -> ElectronBindings {
    let mut bindings = ElectronBindings::default();
    for cap in patterns.import_named.captures_iter(content) {
        let Some(kind) = module_of(&cap[2]) else {
            continue;
        };
        for (local, exported) in parse_named_list(&cap[1]) {
            match kind {
                ModuleKind::Electron | ModuleKind::Ecosystem => {
                    bindings.electron.insert(local, exported);
                }
                ModuleKind::Remote => {
                    bindings.remote.insert(local, exported);
                }
            }
        }
    }
    for cap in patterns.import_namespace.captures_iter(content) {
        let local = cap
            .get(1)
            .or_else(|| cap.get(2))
            .map(|m| m.as_str().to_string());
        let Some(local) = local else { continue };
        let Some(kind) = module_of(&cap[3]) else {
            continue;
        };
        match kind {
            ModuleKind::Electron | ModuleKind::Ecosystem => {
                bindings.electron_ns.insert(local);
            }
            ModuleKind::Remote => {
                bindings.remote_ns.insert(local);
            }
        }
    }
    for cap in patterns.require_destructure.captures_iter(content) {
        let Some(kind) = module_of(&cap[2]) else {
            continue;
        };
        for (local, exported) in parse_named_list(&cap[1]) {
            match kind {
                ModuleKind::Electron | ModuleKind::Ecosystem => {
                    bindings.electron.insert(local, exported);
                }
                ModuleKind::Remote => {
                    bindings.remote.insert(local, exported);
                }
            }
        }
    }
    for cap in patterns.require_member.captures_iter(content) {
        let Some(kind) = module_of(&cap[2]) else {
            continue;
        };
        let (local, exported) = (cap[1].to_string(), cap[3].to_string());
        match kind {
            ModuleKind::Electron | ModuleKind::Ecosystem => {
                bindings.electron.insert(local, exported);
            }
            ModuleKind::Remote => {
                bindings.remote.insert(local, exported);
            }
        }
    }
    for cap in patterns.require_namespace.captures_iter(content) {
        // Skip the destructure/member forms already handled above (their
        // text also matches this regex with a `{`/member tail — the
        // namespace regex requires a bare identifier, so it does not).
        let Some(kind) = module_of(&cap[2]) else {
            continue;
        };
        match kind {
            ModuleKind::Electron | ModuleKind::Ecosystem => {
                bindings.electron_ns.insert(cap[1].to_string());
            }
            ModuleKind::Remote => {
                bindings.remote_ns.insert(cap[1].to_string());
            }
        }
    }
    // Dynamic import: `const { net } = await import('electron')` (VS
    // Code's `nativeHostMainService.ts:1195` — the v2 gate dropped the
    // `net.fetch` sites because only static imports resolved).
    if let Ok(dynamic) = Regex::new(
        r#"(?:const|let|var)\s*\{([^}]*)\}\s*=\s*await\s+import\s*\(\s*['"]([^'"]+)['"]\s*\)"#,
    ) {
        for cap in dynamic.captures_iter(content) {
            let Some(kind) = module_of(&cap[2]) else {
                continue;
            };
            for (local, exported) in parse_named_list(&cap[1]) {
                match kind {
                    ModuleKind::Electron | ModuleKind::Ecosystem => {
                        bindings.electron.insert(local, exported);
                    }
                    ModuleKind::Remote => {
                        bindings.remote.insert(local, exported);
                    }
                }
            }
        }
    }
    // Bare assignment (no declarator): `autoUpdater =
    // require('electron').autoUpdater` after `let autoUpdater = null`
    // (Mailspring's `autoupdate-manager.ts` — the conditionally-assigned
    // binding was invisible, so the whole updater API was lost).
    if let Ok(assign) = Regex::new(
        r#"(?:^|[^\w$.])([A-Za-z_$][\w$]*)\s*=\s*require\s*\(\s*['"]([^'"]+)['"]\s*\)\.([A-Za-z_$][\w$]*)"#,
    ) {
        for cap in assign.captures_iter(content) {
            let Some(kind) = module_of(&cap[2]) else {
                continue;
            };
            let (local, exported) = (cap[1].to_string(), cap[3].to_string());
            match kind {
                ModuleKind::Electron | ModuleKind::Ecosystem => {
                    bindings.electron.insert(local, exported);
                }
                ModuleKind::Remote => {
                    bindings.remote.insert(local, exported);
                }
            }
        }
    }
    // `electron-updater`'s `autoUpdater` is a different updater from
    // Electron's own: remember the local names bound to it (N6).
    for cap in patterns.import_named.captures_iter(content) {
        if &cap[2] == "electron-updater" {
            for (local, exported) in parse_named_list(&cap[1]) {
                if exported == "autoUpdater" {
                    bindings.electron_updater_locals.insert(local);
                }
            }
        }
    }
    for cap in patterns.require_destructure.captures_iter(content) {
        if &cap[2] == "electron-updater" {
            for (local, exported) in parse_named_list(&cap[1]) {
                if exported == "autoUpdater" {
                    bindings.electron_updater_locals.insert(local);
                }
            }
        }
    }
    for cap in patterns.require_member.captures_iter(content) {
        if &cap[2] == "electron-updater" && &cap[3] == "autoUpdater" {
            bindings.electron_updater_locals.insert(cap[1].to_string());
        }
    }
    // Parameters / properties annotated with an Electron type:
    // `session: Session` (with `Session` imported from `electron`),
    // `directSession: Electron.Session`. A same-named local in a file
    // that never touches Electron stays a shadow; a name typed with an
    // Electron type is an Electron object passed in.
    let electron_types: HashSet<String> = bindings
        .electron
        .values()
        .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
        .cloned()
        .collect();
    const KNOWN_ELECTRON_TYPES: &[&str] = &[
        "Session",
        "WebContents",
        "Protocol",
        "BrowserWindow",
        "BaseWindow",
        "App",
        "Menu",
        "Tray",
        "Notification",
        "IpcMainEvent",
        "IpcMainInvokeEvent",
        "IpcRendererEvent",
        "Event",
        "WebContentsView",
        "MessageChannelMain",
        "TouchBar",
        "View",
        "Dock",
        "NativeImage",
    ];
    if let Ok(typed) =
        Regex::new(r#"\b([A-Za-z_$][\w$]*)\s*:\s*(?:Electron\s*\.\s*)?([A-Z][\w$]*)"#)
    {
        for cap in typed.captures_iter(content) {
            let ty = &cap[2];
            if electron_types.contains(ty)
                || (content[cap.get(0).unwrap().start()..cap.get(0).unwrap().end()]
                    .contains("Electron")
                    && KNOWN_ELECTRON_TYPES.contains(&ty))
            {
                bindings.typed_electron_params.insert(cap[1].to_string());
                if ty == "Session" {
                    bindings.typed_session_names.insert(cap[1].to_string());
                }
            }
        }
    }
    // Session objects that arrive by destructuring / assignment rather
    // than by annotation: `const { session } = win.webContents`
    // (Signal's `spell_check.main.ts:70`), `const s =
    // win.webContents.session`, `directSession =
    // electron.session.fromPartition(...)` (Insomnia's
    // `http-client.ts:68`). Each is Electron's `Session` by
    // construction — the name resolves, and its methods count as
    // `session` use.
    if let Ok(destructured) =
        Regex::new(r#"(?:const|let|var)\s*\{([^}]*)\}\s*=\s*[^\n;]*\.webContents\b"#)
    {
        for cap in destructured.captures_iter(content) {
            for (local, _) in parse_named_list(&cap[1]) {
                if local == "session" {
                    bindings.typed_electron_params.insert(local.clone());
                    bindings.typed_session_names.insert(local);
                }
            }
        }
    }
    if let Ok(assigned) = Regex::new(
        r#"(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*[^\n;]*\.webContents\.session\b"#,
    ) {
        for cap in assigned.captures_iter(content) {
            bindings.typed_electron_params.insert(cap[1].to_string());
            bindings.typed_session_names.insert(cap[1].to_string());
        }
    }
    if let Ok(from_partition) = Regex::new(
        r#"(?:^|[^\w$.])([A-Za-z_$][\w$]*)\s*=\s*[^\n;]{0,120}?\.session\.fromPartition\b"#,
    ) {
        for cap in from_partition.captures_iter(content) {
            bindings.typed_electron_params.insert(cap[1].to_string());
            bindings.typed_session_names.insert(cap[1].to_string());
        }
    }
    bindings
}

/// Session methods callable on a `Session` object. Used by
/// [`session_method_call`]: a `directSession.setProxy(...)` call is a
/// `session` finding even though the receiver is not literally named
/// `session`.
const SESSION_METHODS: &[&str] = &[
    "setProxy",
    "resolveProxy",
    "setPermissionRequestHandler",
    "setPermissionCheckHandler",
    "setSpellCheckerDictionaryDownloadURL",
    "setSpellCheckerLanguages",
    "setDisplayMediaRequestHandler",
    "setCertificateVerifyProc",
    "setPreloads",
    "getPreloads",
];

/// The receiver of a `Session`-method call on one line, when the
/// receiver is a Session-typed name from `bindings` (see
/// [`ElectronBindings::typed_session_names`]). Returns the receiver
/// name, e.g. `Some("directSession")` for
/// `directSession.setProxy({...})`.
fn session_method_call(line: &str, session_names: &HashSet<String>) -> Option<String> {
    for name in session_names {
        let mut search_from = 0usize;
        while let Some(idx) = line[search_from..].find(&format!("{name}.")) {
            let after_dot = search_from + idx + name.len() + 1;
            // Word boundary before the name: it must be the receiver,
            // not the tail of a longer identifier (`mydirectSession.`).
            if idx > 0 || search_from > 0 {
                let before_idx = search_from + idx;
                if before_idx > 0 {
                    let prev = line.as_bytes()[before_idx - 1];
                    if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'$' {
                        search_from = after_dot;
                        continue;
                    }
                }
            }
            let method: String = line[after_dot..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            if SESSION_METHODS.contains(&method.as_str()) {
                return Some(name.clone());
            }
            search_from = after_dot;
        }
    }
    None
}

/// Identifiers in one file whose value derives from
/// `@electron/remote` (or from `getCurrentWindow()` /
/// `getCurrentWebContents()`, which are remote accessors in renderer
/// code): `const webContents =
/// AppEnv.getCurrentWindow().webContents` (Mailspring's
/// `window-event-handler.ts:110` — every `webContents.*` call on that
/// object is remote-mediated, but the generic `webContents` pattern
/// runs without binding resolution, so it never saw the mediation).
/// Used to force MANUAL on otherwise-ungated patterns (N6/N8).
fn remote_derived_names(content: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let marker = |rhs: &str| {
        rhs.contains("@electron/remote")
            || rhs.contains("getCurrentWindow")
            || rhs.contains("getCurrentWebContents")
    };
    if let Ok(assign) =
        Regex::new(r#"(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*([^\n;]{0,200})"#)
    {
        for cap in assign.captures_iter(content) {
            if marker(&cap[2]) {
                out.insert(cap[1].to_string());
            }
        }
    }
    if let Ok(destructure) = Regex::new(r#"(?:const|let|var)\s*\{([^}]*)\}\s*=\s*([^\n;]{0,200})"#)
    {
        for cap in destructure.captures_iter(content) {
            if marker(&cap[2]) {
                for (local, _) in parse_named_list(&cap[1]) {
                    out.insert(local);
                }
            }
        }
    }
    if let Ok(bare) = Regex::new(r#"(?:^|[^\w$.])([A-Za-z_$][\w$]*)\s*=\s*([^\n;]{0,200})"#) {
        for cap in bare.captures_iter(content) {
            if marker(&cap[2]) {
                out.insert(cap[1].to_string());
            }
        }
    }
    out
}

/// One-line `ipcMain` wrapper definitions in a file, e.g. Insomnia's
/// `export const ipcMainHandle = (channel: HandleChannels, listener)
/// => ipcMain.handle(channel, listener)` (N6 — 212 handler
/// registrations across 18 files were invisible because only the
/// literal `ipcMain.handle('...')` form was collected).
///
/// Returns the wrapper name → (`ipcMain` method, finding API) map, and
/// the line numbers of the `ipcMain.*` calls *inside* the definitions
/// (those are not handler registrations and must not be counted — the
/// v2 scan counted the definitions themselves as findings).
fn find_ipc_wrappers(
    content: &str,
) -> (
    HashMap<String, (&'static str, &'static str)>,
    HashSet<usize>,
) {
    let mut wrappers: HashMap<String, (&'static str, &'static str)> = HashMap::new();
    let mut def_lines: HashSet<usize> = HashSet::new();
    // Arrow assignment: `export const NAME = (firstParam: Type, ...) => ...`
    // Function declaration: `export function NAME(firstParam: Type, ...) ...`
    let def_res = [
        Regex::new(
            r#"(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?\(\s*([A-Za-z_$][\w$]*)\s*(?::[^,)]*)?[,)]"#,
        ),
        Regex::new(
            r#"(?:export\s+)?(?:async\s+)?function\s+([A-Za-z_$][\w$]*)\s*\(\s*([A-Za-z_$][\w$]*)\s*(?::[^,)]*)?[,)]"#,
        ),
    ];
    for def_re in def_res.iter().flatten() {
        for cap in def_re.captures_iter(content) {
            let name = cap[1].to_string();
            let param = cap[2].to_string();
            // The `ipcMain.*` call lives in the body after the
            // signature (up to ~600 chars ahead for a multi-line
            // arrow); its first argument must be the channel param.
            // Snap the end down to a char boundary: a byte offset can
            // land inside a multi-byte character (Joplin's sources
            // contain non-ASCII, e.g. 'Ř') and slicing there panics.
            let tail_start = cap.get(0).unwrap().end();
            let mut tail_end = (tail_start + 600).min(content.len());
            while tail_end > tail_start && !content.is_char_boundary(tail_end) {
                tail_end -= 1;
            }
            let tail = &content[tail_start..tail_end];
            let call_re = Regex::new(&format!(
                r#"ipcMain\.(handleOnce|handle|on)\s*\(\s*{}\b"#,
                regex::escape(&param)
            ));
            let Ok(call_re) = call_re else {
                continue;
            };
            if let Some(call) = call_re.find(tail) {
                let (method, api): (&'static str, &'static str) = match call.as_str() {
                    s if s.starts_with("ipcMain.handleOnce") => {
                        ("handleOnce", "ipcMain.handleOnce")
                    }
                    s if s.starts_with("ipcMain.handle") => ("handle", "ipcMain.handle"),
                    _ => ("on", "ipcMain.on"),
                };
                wrappers.insert(name, (method, api));
                def_lines.insert(line_number(content, tail_start + call.start()));
            }
        }
    }
    (wrappers, def_lines)
}

/// Replace alias locals with their exported names on one line, so
/// `electronApp.setName(...)` (from `import { app as electronApp }`)
/// matches the `app.*` patterns. Word-boundary replacement only; lines
/// without aliases pass through unchanged.
fn normalize_aliases(line: &str, bindings: &ElectronBindings) -> String {
    let aliases: Vec<(&String, &String)> = bindings
        .electron
        .iter()
        .chain(bindings.remote.iter())
        .filter(|(local, exported)| *local != *exported)
        .collect();
    if aliases.is_empty() {
        return line.to_string();
    }
    let mut out = line.to_string();
    for (local, exported) in aliases {
        let re = Regex::new(&format!(r"\b{}\b", regex::escape(local))).expect("alias regex");
        out = re.replace_all(&out, exported.as_str()).into_owned();
    }
    out
}

/// Blank the contents of string and template literals on one line,
/// replacing each literal's characters with spaces (newlines are
/// preserved by the caller — this runs per line, so there are none).
/// `${...}` interpolations inside template literals are kept as code:
/// an `app.` inside an interpolation is real code, not a string.
///
/// This is what the non-IPC API patterns run against. IPC channel names
/// live in strings by design, so IPC patterns never see this text.
fn strip_string_literals(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\'' || b == b'"' {
            // Single/double-quoted string: blank until the closing quote.
            out.push(b);
            i += 1;
            let mut escaped = false;
            while i < bytes.len() {
                let c = bytes[i];
                if escaped {
                    escaped = false;
                    out.push(b' ');
                } else if c == b'\\' {
                    escaped = true;
                    out.push(b' ');
                } else if c == b {
                    out.push(b);
                    i += 1;
                    break;
                } else {
                    out.push(b' ');
                }
                i += 1;
            }
            continue;
        }
        if b == b'`' {
            // Template literal: blank literal text, keep `${...}` code.
            out.push(b);
            i += 1;
            let mut escaped = false;
            while i < bytes.len() {
                let c = bytes[i];
                if escaped {
                    escaped = false;
                    out.push(b' ');
                    i += 1;
                    continue;
                }
                if c == b'\\' {
                    escaped = true;
                    out.push(b' ');
                    i += 1;
                    continue;
                }
                if c == b'`' {
                    out.push(b);
                    i += 1;
                    break;
                }
                if c == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                    // Interpolation: copy the code verbatim (including
                    // nested braces) until the matching `}`.
                    out.push(b'$');
                    out.push(b'{');
                    i += 2;
                    let mut depth = 1usize;
                    let mut str: Option<u8> = None;
                    let mut esc = false;
                    while i < bytes.len() && depth > 0 {
                        let d = bytes[i];
                        if let Some(q) = str {
                            out.push(d);
                            if esc {
                                esc = false;
                            } else if d == b'\\' {
                                esc = true;
                            } else if d == q {
                                str = None;
                            }
                            i += 1;
                            continue;
                        }
                        match d {
                            b'\'' | b'"' | b'`' => {
                                str = Some(d);
                                out.push(d);
                            }
                            b'{' => {
                                depth += 1;
                                out.push(d);
                            }
                            b'}' => {
                                depth -= 1;
                                out.push(d);
                            }
                            _ => out.push(d),
                        }
                        i += 1;
                    }
                    continue;
                }
                out.push(b' ');
                i += 1;
            }
            continue;
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| line.to_string())
}

/// The identifier a pattern match starts with (`app` for `app.quit`,
/// `Tray` for `new Tray(`, `autoUpdater` for `autoUpdater.`), or `None`
/// when the match has no leading identifier (object keys, `require()`
/// patterns).
fn match_head(m: &regex::Match) -> Option<String> {
    let mut text = m.as_str().trim_start();
    if let Some(rest) = text.strip_prefix("new") {
        text = rest.trim_start();
    }
    let head: String = text
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if head.is_empty() {
        None
    } else {
        Some(head)
    }
}

/// The identifier immediately before a pattern match, when the match is
/// a member access (`this.session.` → `this`, `window.reduxActions.app.`
/// → `reduxActions`, `electron.dialog.` → `electron`). `None` when the
/// match is a bare identifier (no `.` directly before it).
fn match_qualifier(line: &str, m: &regex::Match) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = m.start();
    while i > 0 && bytes[i - 1] == b' ' {
        i -= 1;
    }
    if i == 0 || bytes[i - 1] != b'.' {
        return None;
    }
    let mut j = i - 1;
    while j > 0
        && (bytes[j - 1].is_ascii_alphanumeric() || bytes[j - 1] == b'_' || bytes[j - 1] == b'$')
    {
        j -= 1;
    }
    let qualifier = &line[j..i - 1];
    if qualifier.is_empty() {
        None
    } else {
        Some(qualifier.to_string())
    }
}

/// Whether the text before a match ends with a `function` keyword — a
/// local wrapper *definition* (`function setAsDefaultProtocolClient(...)`)
/// is not a call site.
fn is_function_definition(line: &str, m: &regex::Match) -> bool {
    line[..m.start()].trim_end().ends_with("function")
}

/// Decide whether one API-pattern match on one line is a real Electron
/// call site, and whether it is remote-mediated.
///
/// Returns `None` when the match must be dropped (the identifier does
/// not resolve to Electron), `Some(false)` for a direct Electron call,
/// `Some(true)` for a remote-mediated one (classification forced to
/// MANUAL — see [`REMOTE_MEDIATED_NOTE`]).
///
/// `global_exports` is the app-wide set of Electron export names
/// imported anywhere in the app (the DI / parameter second chance).
/// `shadows` is the set of names this file declares itself (see
/// [`local_shadow_names`]) — a file-local declaration beats the
/// app-wide fallback. `line_has_remote_inline` / `line_has_electron_inline`
/// say whether the original line contains an inline
/// `require('@electron/remote').` / `require('electron').` access.
#[allow(clippy::too_many_arguments)]
fn resolve_match(
    line: &str,
    m: &regex::Match,
    bindings: &ElectronBindings,
    global_exports: &HashSet<String>,
    shadows: &HashSet<String>,
    line_has_remote_inline: bool,
    line_has_electron_inline: bool,
) -> Option<bool> {
    if is_function_definition(line, m) {
        return None;
    }
    let Some(head) = match_head(m) else {
        // No leading identifier (object key / require pattern): the
        // pattern itself is the signal; accept it.
        return Some(line_has_remote_inline);
    };
    // Object-literal property key (`{ session: imageProxySession() }`):
    // a bare identifier followed by `:` inside `{...}` is a key, not a
    // use. A ternary (`? session :`) is preceded by `?`, not `{`/`,`,
    // so it still counts.
    {
        let after = line[m.end()..].trim_start();
        if after.starts_with(':') {
            let before = line[..m.start()].trim_end();
            if before.ends_with('{') || before.ends_with(',') {
                return None;
            }
        }
    }
    if line_has_remote_inline {
        // `require('@electron/remote').X` — the identifier after the
        // require is a remote export by construction.
        return Some(true);
    }
    let qualifier = match_qualifier(line, m);
    // Namespace member access: `electron.dialog`, `remote.app`, or a
    // local namespace binding. The member is an Electron API when the
    // namespace is.
    if let Some(q) = &qualifier {
        if bindings.electron_ns.contains(q) || q == "electron" {
            return Some(false);
        }
        if bindings.remote_ns.contains(q) || q == "remote" {
            return Some(true);
        }
        // Member access through a destructured / aliased binding:
        // `const { app } = require('electron')` then `app.quit()`, or
        // `const { app: electronApp } = require('electron')` then
        // `electronApp.quit()`. The qualifier *is* the binding here.
        if bindings.electron.contains_key(q) {
            return Some(false);
        }
        if bindings.remote.contains_key(q) {
            return Some(true);
        }
        // A file-local declaration of the qualifier name shadows the
        // Electron object (`const session = getLocalSession()` then
        // `session.cookies` is the app's own session, not Electron's).
        // Exception: a qualifier annotated with an Electron type
        // (`session: Session`) is an Electron object passed in.
        if bindings.typed_electron_params.contains(q) {
            return Some(false);
        }
        if shadows.contains(q) {
            return None;
        }
        // DI / parameter second chance: the qualifier is an Electron
        // export imported in another file of the same app and passed in
        // (`app` as a function parameter in single-instance.js). This is
        // a deliberate approximation — a same-named local in a file that
        // also references a real Electron export would false-positive —
        // and it only fires when the file-level gate already opened.
        if global_exports.contains(q) {
            return Some(false);
        }
        // Property chains through a known Electron object: only these
        // are real Electron access. Everything else is a same-named
        // local (`this.session`, `view.session`, `initData.remote`,
        // `window.reduxActions.app`, `navigator.clipboard`,
        // `connection.protocol`, `state.remote`, `document`-derived
        // `dialog`, a local `app-shell` wrapper).
        let allowed = match head.as_str() {
            "session" => q == "webContents",
            "protocol" => q == "session" || q == "defaultSession" || q == "webContents",
            "webContents" => true,
            _ => false,
        };
        return if allowed { Some(false) } else { None };
    }
    // Bare identifier: it must resolve to an Electron binding, a remote
    // binding, an inline `require('electron').` access, or an Electron
    // export imported elsewhere in the app (DI / parameter case).
    if bindings.resolves_remote(&head) {
        return Some(true);
    }
    if bindings.resolves_electron(&head) {
        return Some(false);
    }
    if line_has_electron_inline {
        return Some(false);
    }
    // A parameter / property annotated with an Electron type
    // (`session: Session`, `win: BrowserWindow`) is an Electron object
    // passed in — it resolves even though the file never imports the
    // name itself, and it beats the file-local shadow the typed-param
    // regex would otherwise record for it.
    if bindings.typed_electron_params.contains(&head) {
        return Some(false);
    }
    // A file-local declaration shadows the app-wide fallback: a
    // `session: DraftEditingSession` parameter or a
    // `const session = ...` in this file is the app's own session, not
    // Electron's — even though another file imports Electron's session.
    if shadows.contains(&head) {
        return None;
    }
    if global_exports.contains(&head) {
        return Some(false);
    }
    None
}

/// What `package.json` (and the electron-builder config) told us about
/// the app.
#[derive(Debug, Default)]
struct PackageInfo {
    name: Option<String>,
    version: Option<String>,
    product_name: Option<String>,
    app_id: Option<String>,
    electron_version: Option<String>,
    main_entry: Option<String>,
    native_modules: Vec<String>,
    port_needing_deps: Vec<String>,
    build_tooling: Vec<String>,
}

/// Every `package.json` in the tree, as `(directory relative to the app
/// root, parsed value)`, root first. Bounded depth (6), skipping
/// `node_modules`, build output, version control, and test dirs — a
/// `package.json` under a fixture is not an app package.
fn read_all_package_jsons(app_dir: &Path) -> Vec<(String, serde_json::Value)> {
    let mut out = Vec::new();
    for entry in WalkDir::new(app_dir)
        .max_depth(6)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            if !e.file_type().is_dir() {
                return true;
            }
            let Some(name) = e.file_name().to_str() else {
                return false;
            };
            !(SKIP_DIRS.contains(&name)
                || name == "node_modules"
                || name == "target"
                // Vendored third-party code (`vendor/printenvz` in GitHub
                // Desktop) is not an app package: its `main` + existing
                // target used to outscore the real app package.
                || name == "vendor"
                || TEST_DIR_SEGMENTS.contains(&name.to_ascii_lowercase().as_str()))
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name().to_str() != Some("package.json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let dir = entry
            .path()
            .parent()
            .and_then(|p| p.strip_prefix(app_dir).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        out.push((dir, pkg));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Whether a `package.json` `main` target resolves to a real file under
/// the app root: the literal path, or the same path with a `.js` / `.ts`
/// extension, or an `index.js` / `index.ts` inside it.
fn main_target_exists(app_dir: &Path, pkg_dir: &str, main: &str) -> bool {
    let base = if pkg_dir.is_empty() {
        app_dir.join(main)
    } else {
        app_dir.join(pkg_dir).join(main)
    };
    if base.is_file() {
        return true;
    }
    for ext in ["js", "ts"] {
        if base.with_extension(ext).is_file() {
            return true;
        }
    }
    for index in ["index.js", "index.ts"] {
        if base.join(index).is_file() {
            return true;
        }
    }
    false
}

/// Score one package as "the Electron app package": the one with the
/// `electron` dependency and a `main` entry wins; an orchestrator root
/// (`github-desktop-build`, `mailspring-root`, Joplin's `"root"`) loses
/// points for its name, so the real app package (`app/`,
/// `packages/app-desktop/`) wins even when the root pins the Electron
/// version.
///
/// Two fix-round-2 rules (N2 — the picker chose VS Code's Copilot
/// extension package over the root app package):
/// * the root package gets a small depth bonus: when nothing else
///   clearly wins, the app is the root, not a nested workspace
///   package (the bonus is deliberately small — Mailspring's root
///   pins Electron but has no `main`, so `app/` must still beat it);
/// * a package under an `extensions/` directory is an editor/plugin
///   extension, not the app, and is penalised out of contention
///   (its dependencies still count toward native modules — see
///   [`read_package_json`]).
fn score_app_package(dir: &str, pkg: &serde_json::Value, app_dir: &Path) -> i32 {
    let mut score = 0i32;
    if dir.is_empty() {
        score += 5;
    }
    if dir == "extensions" || dir.starts_with("extensions/") || dir.contains("/extensions/") {
        score -= 50;
    }
    for section in ["dependencies", "devDependencies"] {
        if pkg.get(section).and_then(|d| d.get("electron")).is_some() {
            score += 10;
            break;
        }
    }
    if let Some(main) = pkg.get("main").and_then(|v| v.as_str()) {
        score += 8;
        if main_target_exists(app_dir, dir, main) {
            score += 5;
        }
    }
    if let Some(name) = pkg.get("name").and_then(|v| v.as_str()) {
        let lower = name.to_ascii_lowercase();
        if lower.contains("build") || lower.contains("root") || lower == "root" {
            score -= 5;
        }
    }
    score
}

/// Read `package.json` for the app name, version, product name, bundle
/// id, Electron version, main entry and dependency-based native-module /
/// port-needing-dep detection.
///
/// Monorepos: every `package.json` in the tree is read (see
/// [`read_all_package_jsons`]). Metadata (name, version, main entry,
/// Electron version) comes from the Electron app package — the one that
/// actually depends on `electron` and has a `main` entry — not from an
/// orchestrator root. Native modules, port-needing deps, and build
/// tooling are unioned across *all* packages: a native addon declared
/// only in a workspace package (`sqlite3` in Joplin's
/// `packages/app-desktop/`, `keytar` in GitHub Desktop's `app/`,
/// `node-llama-cpp` in Mailspring's `app/`) still counts, and still
/// costs its 15 feasibility points.
///
/// The Electron version falls back to any package's `electron` dep, then
/// to the app package's `nativeBuild.target` (VS Code keeps its version
/// there, not in a dependency).
///
/// A missing or unparsable `package.json` is not an error — the source
/// scan still runs and the report just has less metadata.
fn read_package_json(app_dir: &Path) -> PackageInfo {
    let packages = read_all_package_jsons(app_dir);
    if packages.is_empty() {
        return PackageInfo::default();
    }
    // Pick the app package: highest score; ties go to the deeper
    // (more specific) directory, then the lexicographically first.
    let mut best_idx = 0usize;
    let mut best_score = i32::MIN;
    let mut best_depth = 0usize;
    for (idx, (dir, pkg)) in packages.iter().enumerate() {
        let score = score_app_package(dir, pkg, app_dir);
        let depth = dir.split('/').filter(|s| !s.is_empty()).count();
        if score > best_score
            || (score == best_score
                // Ties go to the SHALLOWER package: the app package sits
                // near the root, internal packages sit deeper — the old
                // deeper-first tie-break picked Mailspring's
                // `account-sidebar` internal package over `app/`.
                && (depth < best_depth || (depth == best_depth && dir < &packages[best_idx].0)))
        {
            best_idx = idx;
            best_score = score;
            best_depth = depth;
        }
    }
    let (app_dir_rel, pkg) = &packages[best_idx];
    let mut info = PackageInfo {
        name: pkg.get("name").and_then(|v| v.as_str()).map(str::to_string),
        version: pkg
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        product_name: pkg
            .get("productName")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        app_id: pkg
            .get("build")
            .and_then(|b| b.get("appId"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        main_entry: pkg
            .get("main")
            .and_then(|v| v.as_str())
            .map(|main| {
                if app_dir_rel.is_empty() {
                    main.trim_start_matches("./").to_string()
                } else {
                    format!("{app_dir_rel}/{}", main.trim_start_matches("./"))
                }
            })
            .or_else(|| Some("main.js".to_string())),
        ..Default::default()
    };

    // electron-builder.json is the other home of productName / appId
    // (app root first, then the app package's directory).
    if info.product_name.is_none() || info.app_id.is_none() {
        for builder_path in [
            app_dir.join("electron-builder.json"),
            if app_dir_rel.is_empty() {
                app_dir.join("electron-builder.json")
            } else {
                app_dir.join(app_dir_rel).join("electron-builder.json")
            },
        ] {
            if let Ok(builder_text) = fs::read_to_string(builder_path) {
                if let Ok(builder) = serde_json::from_str::<serde_json::Value>(&builder_text) {
                    if info.product_name.is_none() {
                        info.product_name = builder
                            .get("productName")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                    }
                    if info.app_id.is_none() {
                        info.app_id = builder
                            .get("appId")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                    }
                }
            }
            if info.product_name.is_some() && info.app_id.is_some() {
                break;
            }
        }
    }

    // Electron version: the app package's dep first, then the app
    // package's `nativeBuild.target` (VS Code keeps its version there,
    // not in a dependency — and the nested extension packages that DO
    // pin an Electron version must not supply the app's version), then
    // any other non-extension package's dep.
    for section in ["dependencies", "devDependencies"] {
        if let Some(version) = pkg
            .get(section)
            .and_then(|deps| deps.get("electron"))
            .and_then(|v| v.as_str())
        {
            info.electron_version = Some(version.to_string());
            break;
        }
    }
    if info.electron_version.is_none() {
        if let Some(target) = pkg
            .get("nativeBuild")
            .and_then(|b| b.get("target"))
            .and_then(|v| v.as_str())
        {
            if target.contains('.') && target.chars().any(|c| c.is_ascii_digit()) {
                info.electron_version = Some(target.to_string());
            }
        }
    }
    if info.electron_version.is_none() {
        for (dir, other) in &packages {
            if dir == "extensions" || dir.starts_with("extensions/") || dir.contains("/extensions/")
            {
                continue;
            }
            let mut found = None;
            for section in ["dependencies", "devDependencies"] {
                if let Some(version) = other
                    .get(section)
                    .and_then(|deps| deps.get("electron"))
                    .and_then(|v| v.as_str())
                {
                    found = Some(version.to_string());
                    break;
                }
            }
            if let Some(version) = found {
                info.electron_version = Some(version);
                break;
            }
        }
    }

    // Native modules / port-needing deps / build tooling: unioned across
    // every package in the tree.
    for (_, other) in &packages {
        for section in ["dependencies", "devDependencies"] {
            if let Some(deps) = other.get(section).and_then(|d| d.as_object()) {
                for name in deps.keys() {
                    if NATIVE_MODULES.contains(&name.as_str()) {
                        info.native_modules.push(name.clone());
                    } else if PORT_NEEDING_DEPS.contains(&name.as_str()) {
                        info.port_needing_deps.push(name.clone());
                    } else if BUILD_TOOLING.contains(&name.as_str()) {
                        info.build_tooling.push(name.clone());
                    }
                }
            }
        }
    }
    info.native_modules.sort();
    info.native_modules.dedup();
    info.port_needing_deps.sort();
    info.port_needing_deps.dedup();
    info.build_tooling.sort();
    info.build_tooling.dedup();
    info
}

/// Classify one API through the mapping table.
///
/// Unknown APIs are [`Classification::Assisted`] with a note — the table
/// is a living document, and an unknown API is a review item, not a
/// failure.
pub fn classify(api: &str) -> (Classification, Option<String>, String) {
    match mappings::lookup(api) {
        Some(mapping) => {
            let classification = match mapping.status.as_str() {
                "auto" => Classification::Auto,
                "manual" => Classification::Manual,
                _ => Classification::Assisted,
            };
            (
                classification,
                mapping.tauri_target.clone(),
                mapping.notes.clone(),
            )
        }
        None => (
            Classification::Assisted,
            None,
            format!("No mapping for `{api}` yet — review manually against the Tauri API."),
        ),
    }
}

/// Feasibility score, 0–100, from the scan's counts.
///
/// Starts at 100 and deducts:
/// * 10 per native module, capped at −30 (a native addon is real port
///   work, but the fifth one does not make the app five times less
///   migratable than the first);
/// * 2 per Manual finding group, capped at −30 (the old 10-per-group
///   charge capped at −40 saturated at 4 groups — every app in the
///   proof campaign hit it, so the score stopped discriminating);
/// * 1 per Assisted finding beyond the first 5, capped at −15;
/// * a Manual-share term: 20 × (Manual call sites ÷ all call sites),
///   rounded — two apps with the same capped group counts still score
///   differently when one does most of its Electron work through
///   unmappable APIs and the other does not;
/// * 10 when `ipcRenderer` is used but no preload script was found
///   (the bridge has to be invented, not translated).
///
/// The caps are what make the old formula saturate (`45 − 15×natives`
/// once both caps hit — Joplin scored 0/100 for 5 natives, not because
/// it is unmigratable). This formula keeps every term below its cap
/// for the campaign apps, so the score orders them by natives, Manual
/// breadth, *and* Manual share at once. Pure-JS port-needing deps
/// (e.g. `electron-updater`) cost nothing here: they have 1:1 plugin
/// equivalents, so they are AUTO conversions, not native-module
/// penalties.
pub fn feasibility_score(
    native_modules: usize,
    manual_findings: usize,
    assisted_findings: usize,
    manual_occurrences: usize,
    total_occurrences: usize,
    has_preload: bool,
    uses_ipc_renderer: bool,
) -> u8 {
    let mut score: i32 = 100;
    score -= (10 * native_modules as i32).min(30);
    score -= (2 * manual_findings as i32).min(30);
    let assisted_excess = assisted_findings.saturating_sub(5);
    score -= (assisted_excess as i32).min(15);
    if total_occurrences > 0 {
        let share = manual_occurrences as f64 / total_occurrences as f64;
        score -= (20.0 * share).round() as i32;
    }
    if !has_preload && uses_ipc_renderer {
        score -= 10;
    }
    score.clamp(0, 100) as u8
}

/// Human-readable verdict for a feasibility score.
pub fn verdict(score: u8) -> &'static str {
    if score >= 80 {
        "Good fit — mostly automatic"
    } else if score >= 50 {
        "Feasible with assisted steps"
    } else {
        "Manual effort needed — see MANUAL items"
    }
}

/// 1-based line number of a byte offset in `content`.
fn line_number(content: &str, byte_offset: usize) -> usize {
    content[..byte_offset]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
        + 1
}

/// Whether a `/` at this position starts a regex literal rather than a
/// comment or a division. Heuristic, from the previous significant
/// token: a regex follows an operator, an opening delimiter, a comma /
/// semicolon / colon, or a keyword that takes an expression (`return`,
/// `typeof`, `case`, `delete`, `void`, `in`, `of`, `new`, `await`,
/// `yield`, `instanceof`); it does NOT follow an identifier, a number,
/// a string, or a closing `)` / `]` (those end an expression, so `/`
/// there is division) — except `}`, which can close a block (regex) or
/// an object literal (division); `}` is treated as regex, the common
/// case in `if (...) { /re/.test(x) }`.
fn slash_starts_regex(content: &[u8], slash_idx: usize) -> bool {
    // JSX closing tag `</Name>` / `</>`: the `/` sits immediately after
    // `<` and is followed by a tag name or `>`. Real JS `< /regex/` has
    // a space between `<` and `/` (or a non-tag character after the
    // slash), so adjacency plus a tag-like next character is a reliable
    // JSX signal — without it, every `</div>` starts a phantom "regex"
    // that swallows code until the next `/`.
    if slash_idx > 0 && content[slash_idx - 1] == b'<' {
        if let Some(&next) = content.get(slash_idx + 1) {
            if next.is_ascii_alphabetic() || next == b'>' {
                return false;
            }
        }
    }
    let mut j = slash_idx;
    while j > 0 && (content[j - 1] == b' ' || content[j - 1] == b'\t') {
        j -= 1;
    }
    if j == 0 {
        return true;
    }
    let prev = content[j - 1];
    if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'$' {
        // Identifier/keyword (or number) — check the keyword list.
        let mut k = j;
        while k > 0
            && (content[k - 1].is_ascii_alphanumeric()
                || content[k - 1] == b'_'
                || content[k - 1] == b'$')
        {
            k -= 1;
        }
        let word = std::str::from_utf8(&content[k..j]).unwrap_or("");
        return matches!(
            word,
            "return"
                | "typeof"
                | "case"
                | "delete"
                | "void"
                | "in"
                | "of"
                | "new"
                | "await"
                | "yield"
                | "instanceof"
                | "else"
                | "do"
        );
    }
    // A quote ends a string literal, `)` / `]` end an expression.
    !matches!(prev, b'\'' | b'"' | b'`' | b')' | b']')
}

/// Strip `//` and `/* */` comments from JavaScript/TypeScript source,
/// replacing every comment character with a space (newlines are kept, so
/// line numbers are unchanged) and leaving string literals untouched.
///
/// The stripper is string-aware (`'...'`, `"..."`, `` `...` `` with
/// backslash escapes), so a `//` inside a URL string is not treated as a
/// comment. It is also regex-aware: a regex literal containing quotes
/// (e.g. `/(\\balt\\s*=\\s*)(["\\'])([\\s\\S]*?)\\2/gi` — Joplin's
/// `resourceHandling.ts:291`) must not open a phantom string that swallows
/// the rest of the file, and a `//` inside a regex must not start a
/// comment. JSX closing tags (`</div>`) are not regex starts either —
/// without that guard, every closing tag started a phantom "regex" that
/// swallowed code until the next `/`. Known blind spots, accepted for a
/// heuristic scanner: `${...}` interpolations inside template literals
/// (a `//` there is treated as a comment start), and `/` after `}` in an
/// object literal (treated as a regex start).
fn strip_comments(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut string: Option<u8> = None;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(quote) = string {
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == quote {
                string = None;
            }
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            // Line comment: blank everything up to (not including) the newline.
            out.push(b' ');
            out.push(b' ');
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            // Block comment: blank everything up to and including `*/`,
            // preserving newlines so line numbers survive.
            out.push(b' ');
            out.push(b' ');
            i += 2;
            while i < bytes.len() {
                if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                    break;
                }
                if bytes[i] == b'\n' {
                    out.push(b'\n');
                } else {
                    out.push(b' ');
                }
                i += 1;
            }
            continue;
        }
        if b == b'/' && slash_starts_regex(bytes, i) {
            // Regex literal: copy it verbatim (quotes and `//` inside
            // it are literal, not string/comment starts). A character
            // class `[...]` can contain `/` and `]`; escapes apply.
            out.push(b);
            i += 1;
            let mut in_class = false;
            let mut esc = false;
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'\n' {
                    // Unterminated regex (a syntax error in the source):
                    // stop here and let the outer loop handle the newline
                    // — pushing it here AND letting the outer loop push it
                    // again would duplicate the newline and shift every
                    // later line number (this corrupted .tsx window lines
                    // in the proof campaign: each JSX `</div>` misfire
                    // added one phantom line).
                    break;
                }
                out.push(c);
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'[' {
                    in_class = true;
                } else if c == b']' {
                    in_class = false;
                } else if c == b'/' && !in_class {
                    i += 1;
                    break;
                }
                i += 1;
            }
            // Regex flags (`/gi`).
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                out.push(bytes[i]);
                i += 1;
            }
            continue;
        }
        if b == b'\'' || b == b'"' || b == b'`' {
            string = Some(b);
        }
        out.push(b);
        i += 1;
    }
    // The transform only ever replaces bytes with spaces or newlines, so
    // the result is valid UTF-8 whenever the input was.
    String::from_utf8(out).unwrap_or_else(|_| content.to_string())
}

/// Extract the text inside the balanced delimiter pair whose opening
/// character sits at `open_idx` (e.g. the `{` of a `BrowserWindow`
/// options object or the `(` of a `buildFromTemplate` call).
///
/// String literals (`'...'`, `"..."`, `` `...` ``) are skipped, so a
/// brace or paren inside a string cannot unbalance the count. Braces
/// inside `${...}` template interpolations and `//` comments are not
/// handled — an accepted limitation for a v0.1 heuristic scanner.
/// (Callers pass comment-stripped text, so the comment half is moot.)
///
/// Returns the inner text and the index just past the closing character.
fn extract_balanced(
    text: &str,
    open_idx: usize,
    open: char,
    close: char,
) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut start: Option<usize> = None;
    let mut string: Option<u8> = None;
    let mut escaped = false;
    let mut i = open_idx;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(quote) = string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == quote {
                string = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' | b'`' => string = Some(b),
            b if (b as char) == open => {
                depth += 1;
                if start.is_none() {
                    start = Some(i + 1);
                }
            }
            b if (b as char) == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((text[start?..i].to_string(), i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Resolve a path referenced from `base_file` (e.g. a preload path from a
/// `BrowserWindow` in `desktop/app.js`) against the referencing file's
/// directory, normalizing `.` and `..` segments. Both paths use `/`
/// separators and are relative to the app directory.
fn resolve_relative(base_file: &str, reference: &str) -> String {
    let reference = reference.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    if !reference.starts_with('/') {
        if let Some(dir) = base_file.rfind('/').map(|i| &base_file[..i]) {
            parts.extend(dir.split('/').filter(|s| !s.is_empty()));
        }
    }
    for segment in reference.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            seg => parts.push(seg),
        }
    }
    parts.join("/")
}

/// Clean a raw `icon:` option value into a path: template-literal
/// `${__dirname}/assets/icon.png` and `__dirname + '/...'` forms lose
/// their `__dirname` prefix; the remainder resolves against the window's
/// file directory.
fn clean_icon_value(raw: &str) -> String {
    let mut value = raw.trim().to_string();
    for prefix in ["${__dirname}/", "${__dirname}", "__dirname/"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            value = rest.to_string();
            break;
        }
    }
    while let Some(rest) = value.strip_prefix("./") {
        value = rest.to_string();
    }
    value.trim_start_matches('/').to_string()
}

/// Parse one `BrowserWindow` options object into a [`WindowInfo`].
///
/// Width/height come from integer literals when present. When they are
/// dynamic (e.g. `width: mainWindowState.width`), the
/// `electron-window-state` `defaultWidth`/`defaultHeight` in the same
/// file are used instead when present; otherwise the dimension is
/// reported as unknown (`width_known`/`height_known` false) with
/// Electron's default as a placeholder value — never as a confident
/// measurement. Literal `minWidth`/`minHeight`/`backgroundColor`/`icon`
/// are carried through when present.
fn extract_window_options(
    patterns: &Patterns,
    options: &str,
    file: &str,
    line: usize,
    file_content: &str,
) -> WindowInfo {
    // A top-level `const NAME = <digits>` in the same file
    // (Insomnia's `DEFAULT_WIDTH = 1280` / `DEFAULT_HEIGHT = 720` in
    // `window-utils.ts:35-38`): `width: DEFAULT_WIDTH` in the options
    // resolves to the constant instead of reporting unknown.
    fn const_number(content: &str, name: &str) -> Option<u32> {
        let re = Regex::new(&format!(
            r"(?:const|let|var)\s+{}\s*=\s*(\d+)",
            regex::escape(name)
        ))
        .ok()?;
        re.captures(content).and_then(|c| c[1].parse().ok())
    }
    fn ident_value(options: &str, key: &str) -> Option<String> {
        let re = Regex::new(&format!(r"{}\s*:\s*([A-Za-z_$][\w$]*)", regex::escape(key))).ok()?;
        re.captures(options).map(|c| c[1].to_string())
    }
    let (width, width_known, mut from_window_state) = match patterns
        .width
        .captures(options)
        .and_then(|c| c[1].parse().ok())
    {
        Some(w) => (w, true, false),
        None => {
            match ident_value(options, "width").and_then(|name| const_number(file_content, &name)) {
                Some(w) => (w, true, false),
                None => match patterns
                    .window_state_width
                    .captures(file_content)
                    .and_then(|c| c[1].parse().ok())
                {
                    Some(w) => (w, true, true),
                    None => (800, false, false),
                },
            }
        }
    };
    let (height, height_known, from_ws_height) = match patterns
        .height
        .captures(options)
        .and_then(|c| c[1].parse().ok())
    {
        Some(h) => (h, true, false),
        None => match ident_value(options, "height")
            .and_then(|name| const_number(file_content, &name))
        {
            Some(h) => (h, true, false),
            None => match patterns
                .window_state_height
                .captures(file_content)
                .and_then(|c| c[1].parse().ok())
            {
                Some(h) => (h, true, true),
                None => (600, false, false),
            },
        },
    };
    from_window_state = from_window_state || from_ws_height;
    let min_width = patterns
        .min_width
        .captures(options)
        .and_then(|c| c[1].parse().ok())
        .or_else(|| ident_value(options, "minWidth").and_then(|n| const_number(file_content, &n)));
    let min_height = patterns
        .min_height
        .captures(options)
        .and_then(|c| c[1].parse().ok())
        .or_else(|| ident_value(options, "minHeight").and_then(|n| const_number(file_content, &n)));
    let background_color = patterns
        .background_color
        .captures(options)
        .map(|c| c[1].to_string());
    let icon = patterns
        .icon_direct
        .captures(options)
        .map(|c| c[1].to_string())
        .or_else(|| {
            patterns
                .icon_join
                .captures(options)
                .map(|c| c[1].to_string())
        })
        .or_else(|| {
            patterns
                .icon_concat
                .captures(options)
                .map(|c| c[1].to_string())
        })
        .map(|raw| resolve_relative(file, &clean_icon_value(&raw)))
        .filter(|p| !p.is_empty());
    let title = patterns.title.captures(options).map(|c| c[1].to_string());
    let resizable = patterns
        .resizable
        .captures(options)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(true);
    let frame = patterns
        .frame
        .captures(options)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(true);
    WindowInfo {
        width,
        height,
        width_known,
        height_known,
        from_window_state,
        min_width,
        min_height,
        background_color,
        icon,
        title,
        resizable,
        frame,
        file: file.to_string(),
        line,
    }
}

/// Find the object literal assigned to a variable in one file:
/// `const NAME = { ... }` / `let NAME = { ... }` / `NAME = { ... }`.
/// Prefers a definition before `before_idx` (options are usually built
/// above the `new BrowserWindow` call); falls back to any definition in
/// the file. Returns the literal's inner text.
fn find_variable_object_literal(content: &str, name: &str, before_idx: usize) -> Option<String> {
    let assign = Regex::new(&format!(
        r"(?:\b(?:const|let|var)\s+|\b){}\s*=\s*\{{",
        regex::escape(name)
    ))
    .ok()?;
    let mut fallback: Option<String> = None;
    for m in assign.find_iter(content) {
        let brace_rel = content[m.start()..].find('{')?;
        let brace_idx = m.start() + brace_rel;
        let (inner, _) = extract_balanced(content, brace_idx, '{', '}')?;
        if m.start() < before_idx {
            return Some(inner);
        }
        if fallback.is_none() {
            fallback = Some(inner);
        }
    }
    fallback
}

/// Find every `new BrowserWindow(...)` in one file, returning the
/// windows plus any `webPreferences.preload` paths found in their options.
/// `content` must be comment-stripped (see [`strip_comments`]).
///
/// The argument may be an inline object literal, a variable holding the
/// options (`new BrowserWindow(windowOptions)` — how every real main
/// window in the proof campaign is written), or empty. A variable is
/// resolved by finding its object literal in the same file; when it
/// cannot be resolved statically, the window is still recorded, with
/// unknown dimensions — a missed main window is worse than an
/// approximate one. The `electron.BrowserWindow` namespace form is
/// accepted too.
fn extract_windows(
    patterns: &Patterns,
    content: &str,
    file: &str,
) -> (Vec<WindowInfo>, Vec<String>) {
    let mut windows = Vec::new();
    let mut preload_paths = Vec::new();
    for m in patterns.browser_window.find_iter(content) {
        let line = line_number(content, m.start());
        let rest = &content[m.end()..];
        // Skip whitespace after `(`.
        let mut arg_start = 0usize;
        while arg_start < rest.len() && rest.as_bytes()[arg_start].is_ascii_whitespace() {
            arg_start += 1;
        }
        let options: Option<String> = match rest.as_bytes().get(arg_start) {
            Some(b'{') => {
                let brace_idx = m.end() + arg_start;
                extract_balanced(content, brace_idx, '{', '}').map(|(o, _)| o)
            }
            Some(b')') | None => None,
            Some(_) => {
                // An identifier (or expression) holding the options.
                let ident: String = rest[arg_start..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
                    .collect();
                if ident.is_empty() {
                    None
                } else {
                    find_variable_object_literal(content, &ident, m.start())
                }
            }
        };
        let options_text = options.unwrap_or_default();
        windows.push(extract_window_options(
            patterns,
            &options_text,
            file,
            line,
            content,
        ));
        if let Some(cap) = patterns.preload_path.captures(&options_text) {
            preload_paths.push(cap[1].to_string());
        }
    }
    (windows, preload_paths)
}

/// Resolve a `preload:` value that is a variable, not a string literal:
/// `preload: preloadPath` where `preloadPath` is assigned a path (or a
/// `path.join(...)` / `path.resolve(...)` call) in the same file.
/// Returns the referenced path (the last quoted `.js`/`.ts` segment of
/// the assignment, or the resolved join), if one can be found.
fn resolve_preload_variable(content: &str, ident: &str, file: &str) -> Option<String> {
    let assign = Regex::new(&format!(
        r"(?:const|let|var)?\s*{}\s*=\s*([^\n;]+)",
        regex::escape(ident)
    ))
    .ok()?;
    let cap = assign.captures(content)?;
    let rhs = cap[1].trim();
    // A `path.join(__dirname, '..', 'static', 'print-preload.js')`
    // assignment resolves segment by segment — the last quoted
    // segment alone (`print-preload.js`) loses the `..` (N1/N6).
    if rhs.contains("path.join(") || rhs.contains("path.resolve(") {
        if let Some(joined) = resolve_path_join_expr(file, rhs) {
            return Some(joined);
        }
    }
    // Take the last quoted segment ending in a script extension.
    let quoted = Regex::new(r#"['"`]([^'"`]+?\.(?:js|ts|mjs|cjs))['"`]"#).ok()?;
    let mut found: Option<String> = None;
    for c in quoted.captures_iter(rhs) {
        found = Some(c[1].to_string());
    }
    found
}

/// Find every preload path referenced in one file, beyond the ones tied
/// to an extracted window: any `preload:` value (literal or variable),
/// and any `setPreloadScripts({ filePath: ... })` registration (literal
/// or `path.join(...)` form). `content` must be comment-stripped.
fn collect_preload_paths(patterns: &Patterns, content: &str, file: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let preload_key = Regex::new(r"preload\s*:\s*").expect("preload key regex");
    for m in preload_key.find_iter(content) {
        // The value lives on the same line (a `path.join(...)` can span
        // lines, but its first quoted segment is on this line).
        let line_end = content[m.end()..]
            .find('\n')
            .map(|i| m.end() + i)
            .unwrap_or(content.len());
        let line_text = &content[m.start()..line_end];
        // A `path.join(...)` value resolves segment by segment (N1/N6).
        if line_text.contains("path.join(") || line_text.contains("path.resolve(") {
            if let Some(joined) = resolve_path_join_expr(file, line_text) {
                paths.push(joined);
                continue;
            }
        }
        // A quoted literal directly after the key.
        if let Some(cap) = patterns
            .preload_path
            .captures(&content[m.start()..line_end])
        {
            paths.push(cap[1].to_string());
            continue;
        }
        // Otherwise a variable: `preload: preloadPath`.
        let ident: String = content[m.end()..]
            .chars()
            .skip_while(|c| c.is_whitespace())
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if !ident.is_empty() {
            if let Some(resolved) = resolve_preload_variable(content, &ident, file) {
                paths.push(resolved);
            }
        }
    }
    // `session.setPreloadScripts([{ filePath: '...' }])` — the
    // registration form `webPreferences.preload` never sees.
    let quoted =
        Regex::new(r#"['"`]([^'"`]+?\.(?:js|ts|mjs|cjs))['"`]"#).expect("quoted path regex");
    for m in patterns.set_preload_scripts.find_iter(content) {
        let open_idx = m.end() - 1;
        let Some((args, _)) = extract_balanced(content, open_idx, '(', ')') else {
            continue;
        };
        for cap in quoted.captures_iter(&args) {
            // `filePath` values only: skip the `id` / label strings that
            // sometimes sit next to them by requiring a path separator
            // or a `preload` in the name.
            let path = &cap[1];
            if path.contains('/')
                || path.contains('\\')
                || path.to_ascii_lowercase().contains("preload")
            {
                paths.push(path.to_string());
            }
        }
    }
    paths
}

/// Find every `Menu.buildFromTemplate(...)` in one file, plus whether
/// the file calls `Menu.setApplicationMenu(...)` and `new Menu()`.
///
/// A static array literal argument is kept as template source (capped at
/// ~4 KiB) so the converter can translate it; a variable or expression
/// argument is recorded as non-static and left for human review.
fn extract_menus(patterns: &Patterns, content: &str, file: &str) -> Vec<MenuInfo> {
    let mut menus = Vec::new();
    for m in patterns.menu_template.find_iter(content) {
        // The match ends right after `(`, so the paren itself is at end - 1.
        let open_idx = m.end() - 1;
        let Some((arg, _end)) = extract_balanced(content, open_idx, '(', ')') else {
            continue;
        };
        let trimmed = arg.trim();
        let is_static_literal = trimmed.starts_with('[');
        let template_source = if is_static_literal {
            Some(trimmed.chars().take(TEMPLATE_SOURCE_CAP).collect())
        } else {
            None
        };
        menus.push(MenuInfo {
            is_static_literal,
            template_source,
            file: file.to_string(),
            line: line_number(content, m.start()),
            is_application_menu: false,
        });
    }
    menus
}

/// Find the first `Menu.buildFromTemplate(...)` in one file.
///
/// Kept for the unit tests and for callers that only need one menu;
/// [`scan`] collects all of them with [`extract_menus`].
#[cfg(test)]
fn extract_menu(patterns: &Patterns, content: &str, file: &str) -> Option<MenuInfo> {
    extract_menus(patterns, content, file).into_iter().next()
}

/// Whether `content` calls `Menu.setApplicationMenu(...)`, and the
/// argument text of each call (a template variable name, or a nested
/// `Menu.buildFromTemplate(...)` call).
fn collect_set_application_menu(content: &str) -> Vec<String> {
    let re = Regex::new(r"Menu\.setApplicationMenu\s*\(").expect("setApplicationMenu regex");
    let mut args = Vec::new();
    for m in re.find_iter(content) {
        let open_idx = m.end() - 1;
        if let Some((arg, _)) = extract_balanced(content, open_idx, '(', ')') {
            args.push(arg.trim().to_string());
        }
    }
    args
}

/// Find every IPC channel in one file, paired with the API name that
/// produced it (e.g. `"ipcMain.handle"`), for finding classification.
///
/// `ipcRenderer.invoke` and `ipcRenderer.send` both report direction
/// `"send"` (renderer → main); `ipcMain.handle` reports `"handle"`,
/// `ipcMain.on` reports `"on"`, and `ipcRenderer.on` reports `"listen"`
/// (the renderer subscribing to a main → renderer event — a different
/// direction from `ipcMain.on`, which the Rust side must receive).
/// `ipcMain.handleOnce` is the one-shot variant of `handle`.
///
/// A call whose channel is a variable, not a string literal
/// (`ipcRenderer.invoke(channel, ...args)` — the typed invoke wrappers
/// whole renderers are built on), is recorded with channel `"dynamic"`
/// instead of being dropped: the call site is real even though the
/// channel name is not statically known. `content` must be
/// comment-stripped.
///
/// `wrappers` are the app's one-line `ipcMain` wrapper functions
/// (see [`find_ipc_wrappers`]): a call through one
/// (`ipcMainHandle('git.db-synced', …)`) is a handler registration
/// for the wrapped method. `wrapper_def_lines` are the lines of the
/// `ipcMain.*` calls *inside* the wrapper definitions — those are not
/// registrations and are skipped.
fn collect_ipc_channels(
    patterns: &Patterns,
    content: &str,
    file: &str,
    wrappers: &[(Regex, Regex, &'static str, &'static str)],
    wrapper_def_lines: &HashSet<usize>,
) -> Vec<(IpcChannel, &'static str)> {
    let mut channels = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_no = idx + 1;
        // A wrapper definition's own `ipcMain.handle(channel, …)` call
        // registers nothing — skip it (N6).
        if wrapper_def_lines.contains(&line_no) {
            continue;
        }
        for cap in patterns.ipc_main.captures_iter(line) {
            let (direction, api) = match &cap[1] {
                "handleOnce" => ("handle", "ipcMain.handleOnce"),
                "handle" => ("handle", "ipcMain.handle"),
                _ => ("on", "ipcMain.on"),
            };
            channels.push((
                IpcChannel {
                    name: cap[2].to_string(),
                    direction: direction.to_string(),
                    file: file.to_string(),
                    line: line_no,
                },
                api,
            ));
        }
        for cap in patterns.ipc_main_dynamic.captures_iter(line) {
            // The literal form above cannot match this line's call: the
            // dynamic regex needs an identifier where the literal form
            // needs a quote, so the two are disjoint by construction.
            let (direction, api) = match &cap[1] {
                "handleOnce" => ("handle", "ipcMain.handleOnce"),
                "handle" => ("handle", "ipcMain.handle"),
                _ => ("on", "ipcMain.on"),
            };
            channels.push((
                IpcChannel {
                    name: "dynamic".to_string(),
                    direction: direction.to_string(),
                    file: file.to_string(),
                    line: line_no,
                },
                api,
            ));
        }
        // Calls through a one-line `ipcMain` wrapper function.
        for (literal_re, dynamic_re, direction, api) in wrappers {
            for cap in literal_re.captures_iter(line) {
                channels.push((
                    IpcChannel {
                        name: cap[1].to_string(),
                        direction: (*direction).to_string(),
                        file: file.to_string(),
                        line: line_no,
                    },
                    api,
                ));
            }
            if !literal_re.is_match(line) {
                for _cap in dynamic_re.captures_iter(line) {
                    channels.push((
                        IpcChannel {
                            name: "dynamic".to_string(),
                            direction: (*direction).to_string(),
                            file: file.to_string(),
                            line: line_no,
                        },
                        api,
                    ));
                }
            }
        }
        for cap in patterns.ipc_renderer.captures_iter(line) {
            let (direction, api) = match &cap[1] {
                "on" => ("listen", "ipcRenderer.on"),
                "send" => ("send", "ipcRenderer.send"),
                _ => ("send", "ipcRenderer.invoke"),
            };
            channels.push((
                IpcChannel {
                    name: cap[2].to_string(),
                    direction: direction.to_string(),
                    file: file.to_string(),
                    line: line_no,
                },
                api,
            ));
        }
        for cap in patterns.ipc_renderer_dynamic.captures_iter(line) {
            // Disjoint from the literal form by construction (see above).
            let (direction, api) = match &cap[1] {
                "on" => ("listen", "ipcRenderer.on"),
                "send" => ("send", "ipcRenderer.send"),
                _ => ("send", "ipcRenderer.invoke"),
            };
            channels.push((
                IpcChannel {
                    name: "dynamic".to_string(),
                    direction: direction.to_string(),
                    file: file.to_string(),
                    line: line_no,
                },
                api,
            ));
        }
    }
    channels
}

/// Find every main → renderer `webContents.send('channel', ...)` call
/// site in one file. The pattern is unambiguous (only Electron has
/// `webContents`), so this runs on every file, gated or not.
/// A variable channel (`webContents.send(commandName, ...)`) is recorded
/// as `"dynamic"`. `content` must be comment-stripped.
///
/// Each channel is paired with whether the send is remote-mediated:
/// the `webContents` receiver is a remote-derived name, or the
/// receiver's owner (`win.webContents.send(...)`) is — those sends
/// are MANUAL, not AUTO (N8).
fn collect_send_channels(
    patterns: &Patterns,
    content: &str,
    file: &str,
    remote_derived: &HashSet<String>,
) -> Vec<(IpcChannel, bool)> {
    let mut channels = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        // The receiver of this line's `webContents.send`, when it is
        // qualified (`win.webContents.send(...)`): the identifier
        // before `.webContents`.
        let qualified_owner = line.find(".webContents.send").and_then(|pos| {
            let before = &line[..pos];
            let ident: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if ident.is_empty() {
                None
            } else {
                Some(ident)
            }
        });
        let is_remote = remote_derived.contains("webContents")
            || qualified_owner
                .as_ref()
                .is_some_and(|owner| remote_derived.contains(owner))
            || patterns.remote_require.is_match(line);
        for cap in patterns.webcontents_send.captures_iter(line) {
            channels.push((
                IpcChannel {
                    name: cap[1].to_string(),
                    direction: "emit".to_string(),
                    file: file.to_string(),
                    line: idx + 1,
                },
                is_remote,
            ));
        }
        if !patterns.webcontents_send.is_match(line) {
            for _cap in patterns.webcontents_send_dynamic.captures_iter(line) {
                channels.push((
                    IpcChannel {
                        name: "dynamic".to_string(),
                        direction: "emit".to_string(),
                        file: file.to_string(),
                        line: idx + 1,
                    },
                    is_remote,
                ));
            }
        }
    }
    channels
}

/// Split an object literal's inner text into top-level members (commas
/// inside nested braces/brackets/parens or strings do not split).
/// Returns the raw member texts in source order.
fn split_top_level_members(inner: &str) -> Vec<String> {
    let bytes = inner.as_bytes();
    let mut members = Vec::new();
    let mut depth_curly = 0i32;
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    let mut string: Option<u8> = None;
    let mut escaped = false;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(quote) = string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == quote {
                string = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' | b'`' => string = Some(b),
            b'{' => depth_curly += 1,
            b'}' => depth_curly -= 1,
            b'(' => depth_paren += 1,
            b')' => depth_paren -= 1,
            b'[' => depth_bracket += 1,
            b']' => depth_bracket -= 1,
            b',' if depth_curly == 0 && depth_paren == 0 && depth_bracket == 0 => {
                members.push(inner[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let tail = inner[start..].trim();
    if !tail.is_empty() {
        members.push(tail.to_string());
    }
    members
}

/// Classify one preload object member's value text.
fn classify_preload_value(value: &str) -> PreloadMemberKind {
    let value = value.trim();
    if value.starts_with("function") || value.contains("=>") {
        return PreloadMemberKind::Function;
    }
    // Platform checks: `process.platform === 'darwin'` and friends.
    if value.contains("process.platform") {
        if value.contains("'darwin'") || value.contains("\"darwin\"") {
            return PreloadMemberKind::PlatformMac;
        }
        if value.contains("'linux'") || value.contains("\"linux\"") {
            return PreloadMemberKind::PlatformLinux;
        }
        if value.contains("'win32'") || value.contains("\"win32\"") {
            return PreloadMemberKind::PlatformWindows;
        }
    }
    PreloadMemberKind::Value
}

/// Parse the top-level members of an object literal's inner text into
/// [`PreloadMember`]s. Handles `key: value` pairs (quoted or unquoted
/// keys) and method shorthand (`key(...) {...}`).
fn parse_object_members(inner: &str) -> Vec<PreloadMember> {
    let mut members = Vec::new();
    for chunk in split_top_level_members(inner) {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        // Method shorthand: `name(...) { ... }` — no colon before the paren.
        let colon = chunk.find(':');
        let paren = chunk.find('(');
        let name: Option<String>;
        let kind: PreloadMemberKind;
        match (colon, paren) {
            (Some(c), Some(p)) if p < c => {
                name = Some(
                    chunk[..p]
                        .trim()
                        .trim_matches(|c| c == '\'' || c == '"')
                        .to_string(),
                );
                kind = PreloadMemberKind::Function;
            }
            (None, Some(p)) => {
                name = Some(
                    chunk[..p]
                        .trim()
                        .trim_matches(|c| c == '\'' || c == '"')
                        .to_string(),
                );
                kind = PreloadMemberKind::Function;
            }
            (Some(c), _) => {
                let raw_name = chunk[..c].trim().trim_matches(|c| c == '\'' || c == '"');
                name = Some(raw_name.to_string());
                kind = classify_preload_value(&chunk[c + 1..]);
            }
            (None, None) => {
                // Not a `key: value` pair (e.g. a spread `...other`) — skip.
                continue;
            }
        }
        if let Some(name) = name {
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
            {
                members.push(PreloadMember { name, kind });
            }
        }
    }
    members
}

/// Recover the shape of one object exposed via
/// `contextBridge.exposeInMainWorld('key', EXPR)`.
///
/// `EXPR` is either an inline object literal or an identifier naming an
/// object literal assigned in the same file (`const api = {...}`).
/// Returns `None` when the value cannot be resolved statically (computed
/// expressions, imports from another file) — the bridge then emits a
/// `TODO`, not a guessed proxy. `content` must be comment-stripped.
fn extract_preload_shape(
    content: &str,
    file: &str,
    key: &str,
    call_open_idx: usize,
) -> Option<PreloadShape> {
    let (args, _) = extract_balanced(content, call_open_idx, '(', ')')?;
    // The first top-level comma separates the key from the value expression.
    let members_text = split_top_level_members(&args);
    if members_text.len() < 2 {
        return None;
    }
    let expr = members_text[1].trim();
    // A function-valued exposure (`exposeInMainWorld('printToPDF', () => {...})`):
    // the exposed value is one function, so the shape is that single
    // function member, named after the key.
    if !expr.starts_with('{') {
        let ident: String = expr
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if ident != expr && (expr.starts_with("function") || expr.contains("=>")) {
            return Some(PreloadShape {
                key: key.to_string(),
                members: vec![PreloadMember {
                    name: key.to_string(),
                    kind: PreloadMemberKind::Function,
                }],
                file: file.to_string(),
                line: line_number(content, call_open_idx),
            });
        }
    }
    let inner = if expr.starts_with('{') {
        // Inline literal: find the `{` inside the args and re-extract.
        let brace_rel = args.find('{')?;
        // The args text starts just after the call's `(`, so the brace's
        // index in `content` is call_open_idx + 1 + brace_rel.
        let (inner, _) = extract_balanced(content, call_open_idx + 1 + brace_rel, '{', '}')?;
        inner
    } else {
        // Identifier: find `NAME = {` in the same file.
        let ident: String = expr
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if ident.is_empty() || ident != expr {
            return None;
        }
        let assign = Regex::new(&format!(
            r"(?:const|let|var)\s+{}\s*=\s*\{{",
            regex::escape(&ident)
        ))
        .ok()?;
        let m = assign.find(content)?;
        let brace_idx = content[m.start()..].find('{')? + m.start();
        let (inner, _) = extract_balanced(content, brace_idx, '{', '}')?;
        inner
    };
    Some(PreloadShape {
        key: key.to_string(),
        members: parse_object_members(&inner),
        file: file.to_string(),
        line: line_number(content, call_open_idx),
    })
}

/// Find dev-server URL candidates in one main-process file: literal
/// `http://localhost:<port>` URLs (e.g. a `loadURL` argument) and
/// `url.format({ host: 'localhost:<port>' })` forms. Returns
/// `(url, file, line, score)` — the score ranks candidates (N11):
/// the URL's own line loads it (`loadURL`/`loadFile`, +30); the file
/// loads URLs somewhere (+10); the file is main-process code (+5);
/// paths that cannot be the app's dev server (extension code,
/// examples, CLI tools, smoke tests) are penalised (−20). The
/// first-match rule used to pick a Copilot service URL
/// (`localhost:4443`) and a CLI docs server (`localhost:41184`)
/// over the real dev server; a candidate must score above 0 to win.
/// Minimum score for a dev-URL candidate to be accepted as the app's
/// dev server: the file-loads-URLs signal (+10) or a `loadURL` line
/// (+30). The main-process path bonus (+5) alone is not evidence the
/// URL is served — VS Code's OAuth loopback port scores 5 on it and
/// must be rejected (see the gate comment in [`scan`]).
const DEV_URL_MIN_SCORE: i32 = 10;

fn find_dev_url_candidates(
    patterns: &Patterns,
    content: &str,
    file: &str,
) -> Vec<(DevUrlInfo, i32)> {
    let file_lower = file.to_ascii_lowercase();
    let penalised = file_lower.contains("extensions/")
        || file_lower.contains("examples")
        || file_lower.contains("example")
        || file_lower.contains("app-cli")
        || file_lower.contains("smoke-test")
        || file_lower.contains("fixtures")
        || file_lower.contains("/test/")
        || file_lower.contains("test-");
    let in_load_url_file = content.contains("loadURL");
    let main_area = file_lower.contains("main")
        || file_lower.contains("electron-main")
        || file_lower.contains("entry.main")
        || file_lower.contains("browser/");
    let mut out = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let mut score = if penalised { -20 } else { 0 };
        if line.contains("loadURL") || line.contains("loadFile") {
            score += 30;
        } else if in_load_url_file {
            score += 10;
        }
        if main_area {
            score += 5;
        }
        for cap in patterns.dev_url_literal.captures_iter(line) {
            out.push((
                DevUrlInfo {
                    url: cap[0].to_string(),
                    file: file.to_string(),
                    line: idx + 1,
                },
                score,
            ));
        }
        for cap in patterns.dev_url_host.captures_iter(line) {
            // Skip lines that already matched the literal form.
            if patterns.dev_url_literal.is_match(line) {
                continue;
            }
            out.push((
                DevUrlInfo {
                    url: format!("http://localhost:{}", &cap[1]),
                    file: file.to_string(),
                    line: idx + 1,
                },
                score,
            ));
        }
    }
    out
}

/// Push a finding, or bump the occurrence count when this (API, file)
/// pair was already recorded — findings display once per file, keeping
/// the first occurrence's line, but the true call-site count is kept.
fn push_finding(
    findings: &mut Vec<Finding>,
    seen: &mut HashMap<(String, String), usize>,
    finding: Finding,
) {
    let key = (finding.api.clone(), finding.file.clone());
    if let Some(&idx) = seen.get(&key) {
        findings[idx].occurrences += 1;
    } else {
        seen.insert(key, findings.len());
        findings.push(finding);
    }
}

/// Classify `api` through the mapping table and record the finding.
///
/// When `force_manual` is set (a call reached through
/// `@electron/remote`), the classification is forced to MANUAL and the
/// remote note is appended, whatever the mapping table says for the
/// underlying API — the underlying call is real, but it has to become
/// an explicit `invoke()` + Rust command.
fn add_finding(
    findings: &mut Vec<Finding>,
    seen: &mut HashMap<(String, String), usize>,
    api: &str,
    category: &str,
    file: &str,
    line: usize,
) {
    add_finding_override(findings, seen, api, category, file, line, false);
}

fn add_finding_override(
    findings: &mut Vec<Finding>,
    seen: &mut HashMap<(String, String), usize>,
    api: &str,
    category: &str,
    file: &str,
    line: usize,
    force_manual: bool,
) {
    let (mut classification, tauri_target, mut note) = classify(api);
    if force_manual {
        classification = Classification::Manual;
        if !note.is_empty() {
            note.push(' ');
        }
        note.push_str(REMOTE_MEDIATED_NOTE);
    }
    push_finding(
        findings,
        seen,
        Finding {
            api: api.to_string(),
            category: category.to_string(),
            file: file.to_string(),
            line,
            classification,
            tauri_target,
            note,
            occurrences: 1,
            // The remote-mediation split (N8): a finding forced to
            // MANUAL because the call goes through
            // `@electron/remote` is marked so the accuracy matrix can
            // separate "no Tauri equivalent" from "reached through
            // remote". A finding that is MANUAL on its own merits is
            // not flagged.
            remote_mediated: force_manual,
        },
    );
}

fn is_skipped_dir(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return false;
    }
    entry
        .file_name()
        .to_str()
        .map(|n| SKIP_DIRS.contains(&n))
        .unwrap_or(false)
}

fn is_scannable(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SCANNABLE_EXTENSIONS.contains(&e))
        .unwrap_or(false)
}

/// Recover the shape of an object assigned directly to the window
/// (`window.electronWindow = { onSetWindowId: ... }` — Joplin's
/// secondary-window preload, which uses no `contextBridge` at all).
/// Returns `None` when the value cannot be resolved statically.
fn extract_window_assign_shape(
    patterns: &Patterns,
    content: &str,
    file: &str,
) -> Vec<PreloadShape> {
    let mut shapes = Vec::new();
    for cap in patterns.window_assign.captures_iter(content) {
        let key = cap[1].to_string();
        let brace_rel = content[cap.get(0).unwrap().start()..].find('{');
        let Some(brace_rel) = brace_rel else {
            continue;
        };
        let brace_idx = cap.get(0).unwrap().start() + brace_rel;
        let Some((inner, _)) = extract_balanced(content, brace_idx, '{', '}') else {
            continue;
        };
        shapes.push(PreloadShape {
            key,
            members: parse_object_members(&inner),
            file: file.to_string(),
            line: line_number(content, cap.get(0).unwrap().start()),
        });
    }
    shapes
}

/// Whether a filename is a preload *entry point* (N4): the file a
/// window actually loads as its preload script.
///
/// Entry-point names: `preload.*` (including `preload.wrapper.ts`
/// and `preload.preload.ts`), `*.sandbox.*` (Signal's
/// `ts/windows/*/*.sandbox.ts`), and `*Preload.js` / `*Preload.ts`
/// (Joplin's `secondaryWindowPreload.js`). Excluded on purpose:
/// declaration files (`*.d.ts`), `webviewPreloads.ts` (a renderer
/// module that *builds* preload helpers — its stem ends in `Preloads`,
/// plural, not `Preload`), and `*.preload.ts` renderer modules
/// (see [`is_preload_module_filename`]).
///
/// A file can also become an entry point by being *referenced* from a
/// `webPreferences.preload` — `bridge.preload.js`,
/// `print-preload.js`, and Insomnia's `entry.preload.ts` are entries
/// by reference, not by name (see [`resolve_preload_reference`]).
fn is_preload_entry_filename(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name.ends_with(".d.ts") {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("preload") {
        return true;
    }
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    if stem.ends_with(".sandbox") {
        return true;
    }
    // `secondaryWindowPreload.js` — but NOT `webviewPreloads.ts`
    // (stem ends in `Preloads`, plural).
    if stem.ends_with("Preload") {
        return true;
    }
    false
}

/// Whether a filename is a module that merely *runs in* preload
/// context: `*.preload.ts` / `*.preload.tsx` / `*.preload.js`
/// (Signal names ~600 renderer modules this way; they are bundled
/// *into* the preload script by the app's bundler). Not entry points
/// — reported separately in [`PreloadInfo::modules`], never in
/// [`PreloadInfo::files`].
fn is_preload_module_filename(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name.ends_with(".d.ts") {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    lower.contains(".preload.")
        || lower.ends_with(".preload.js")
        || lower.ends_with(".preload.ts")
        || lower.ends_with(".preload.jsx")
        || lower.ends_with(".preload.tsx")
        || lower.ends_with(".preload.mjs")
        || lower.ends_with(".preload.cjs")
}

/// Resolve a `path.join(...)` / `path.resolve(...)` expression to a
/// path relative to the app root: `__dirname` anchors at the
/// referencing file's directory, `'..'` pops a segment, `'.'` is
/// skipped, and the remaining quoted segments append. Returns `None`
/// when the call cannot be resolved statically (a variable segment) —
/// callers fall back to the last quoted `.js`/`.ts` segment.
///
/// This is what makes Mailspring's `preloadPath`
/// (`path.join(__dirname, '..', 'static', 'print-preload.js')`)
/// resolve to `print/static/print-preload.js` instead of the phantom
/// `print/lib/print-preload.js` the old last-segment rule produced
/// (same path-join class as VS Code's N1).
fn resolve_path_join_expr(file_rel: &str, expr: &str) -> Option<String> {
    let start = expr
        .find("path.join(")
        .or_else(|| expr.find("path.resolve("))?;
    let open = expr[start..].find('(')? + start;
    let (args, _) = extract_balanced(expr, open, '(', ')')?;
    let members = split_top_level_members(&args);
    let mut parts: Vec<&str> = Vec::new();
    let mut anchored = false;
    for arg in &members {
        let arg = arg.trim();
        if arg == "__dirname" {
            if let Some(dir) = file_rel.rfind('/').map(|i| &file_rel[..i]) {
                parts = dir.split('/').filter(|s| !s.is_empty()).collect();
            }
            anchored = true;
        } else if arg.starts_with('\'') || arg.starts_with('"') || arg.starts_with('`') {
            let seg = arg.trim_matches(|c| c == '\'' || c == '"' || c == '`');
            // A segment may itself contain slashes (`'./preload.js'`,
            // `'static/print-preload.js'`): split and process piece by
            // piece, so `.` never survives into the joined path.
            for piece in seg.split('/') {
                match piece {
                    "" | "." => {}
                    ".." => {
                        parts.pop();
                    }
                    s => parts.push(s),
                }
            }
        } else {
            // A variable / expression segment: not statically resolvable.
            return None;
        }
    }
    if !anchored && parts.is_empty() {
        return None;
    }
    // `path.resolve` with an absolute first segment is app-rooted;
    // `path.join` without `__dirname` is relative to the file.
    if !anchored {
        if let Some(dir) = file_rel.rfind('/').map(|i| &file_rel[..i]) {
            let mut prefixed: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
            prefixed.extend(parts);
            parts = prefixed;
        }
    }
    Some(parts.join("/"))
}

/// Find the source file for a build-output preload reference: the
/// file in the tree whose stem matches the reference's stem (minus a
/// `.min` infix), e.g. `entry.preload.min.js` →
/// `src/entry.preload.ts`, or a root-relative
/// `FileAccess.asFileUri('vs/base/.../preload-aux.js')` →
/// `src/vs/base/.../preload-aux.ts` (N1). When several files share
/// the stem, the one sharing the longest directory prefix with the
/// referencing file wins, then the shortest path.
fn find_preload_source(app_dir: &Path, referencing_rel: &str, raw: &str) -> Option<String> {
    let file_name = raw.rsplit('/').next().unwrap_or(raw);
    let mut stem = file_name
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(file_name)
        .to_string();
    if let Some(stripped) = stem.strip_suffix(".min") {
        stem = stripped.to_string();
    }
    if stem.is_empty() {
        return None;
    }
    let ref_dir: Vec<&str> = referencing_rel
        .rfind('/')
        .map(|i| referencing_rel[..i].split('/').collect())
        .unwrap_or_default();
    let mut best: Option<(usize, usize, String)> = None;
    for entry in WalkDir::new(app_dir)
        .into_iter()
        .filter_entry(|e| !is_skipped_dir(e))
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_scannable(path) {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let candidate_stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
        if candidate_stem != stem {
            continue;
        }
        let rel = relative_path(app_dir, path);
        let dir_parts: Vec<&str> = rel
            .rfind('/')
            .map(|i| rel[..i].split('/').collect())
            .unwrap_or_default();
        let shared = ref_dir
            .iter()
            .zip(dir_parts.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let better = match &best {
            None => true,
            Some((best_shared, best_len, _)) => {
                shared > *best_shared || (shared == *best_shared && rel.len() < *best_len)
            }
        };
        if better {
            best = Some((shared, rel.len(), rel));
        }
    }
    best.map(|(_, _, rel)| rel)
}

/// Resolve one referenced preload path to a real file in the app, or
/// `None` when it names nothing on disk (N1/N6 phantoms).
///
/// Candidates, in order: resolved against the referencing file's
/// directory; resolved against the app root (a root-relative
/// `FileAccess.asFileUri('vs/base/...')` path); each with a `.min.js`
/// → `.ts` / `.js` → `.ts` source fallback, because build-output
/// paths (`.min.js`) do not exist in a source clone. When none of
/// those exists, the stem search ([`find_preload_source`]) looks for
/// the source file by name. A path that exists nowhere is dropped,
/// not listed — the v2 report listed phantom paths as preload files.
/// Normalize a `/`-separated relative path: drop `.` / empty segments
/// and resolve `..` by popping (`desktop/./preload.js` →
/// `desktop/preload.js`). Preload references must be stored normalized
/// — the OS accepts `./` in a file check, so an unnormalized path
/// would pass `is_file` and duplicate the filename match.
fn normalize_rel_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn resolve_preload_reference(app_dir: &Path, referencing_rel: &str, raw: &str) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    for base in [
        resolve_relative(referencing_rel, raw),
        // A root-relative reference (`/vs/.../preload.js`) is
        // app-rooted; normalize it so `.` segments cannot survive
        // (`desktop/./preload.js` would otherwise pass the `is_file`
        // check and duplicate the filename match).
        normalize_rel_path(raw.trim_start_matches('/')),
    ] {
        candidates.push(base.clone());
        if let Some(ts) = base.strip_suffix(".min.js").map(|s| format!("{s}.ts")) {
            candidates.push(ts);
        } else if let Some(ts) = base.strip_suffix(".js").map(|s| format!("{s}.ts")) {
            candidates.push(ts);
        }
        if let Some(js) = base.strip_suffix(".min.js").map(|s| format!("{s}.js")) {
            candidates.push(js);
        }
    }
    candidates.dedup();
    for candidate in &candidates {
        if app_dir.join(candidate).is_file() {
            return Some(candidate.clone());
        }
    }
    find_preload_source(app_dir, referencing_rel, raw)
}

/// Path relative to the app dir, with `/` separators, for reports.
fn relative_path(app_dir: &Path, path: &Path) -> String {
    path.strip_prefix(app_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Scan the Electron app at `app_dir` and build its [`ScanReport`].
///
/// Reads `package.json`, walks the source tree (skipping `node_modules`,
/// `dist`, `.git` and `out`), regex-scans every `.js`/`.ts` file for
/// Electron API usage, IPC channels, windows, menus and preload scripts,
/// then scores feasibility. Output lists are sorted for deterministic
/// reports regardless of filesystem walk order.
pub fn scan(app_dir: &Path) -> Result<ScanReport> {
    if !app_dir.is_dir() {
        anyhow::bail!("{} is not a directory", app_dir.display());
    }
    let patterns = Patterns::new();
    let pkg = read_package_json(app_dir);

    // Pass 1 — collect every scannable, non-test file (test / spec /
    // fixture files never ship, so they are skipped entirely and do not
    // count toward `files_scanned`), and build the app-wide set of
    // Electron export names imported anywhere in the app. That set is
    // the DI / parameter second chance: a file that never imports
    // Electron can still use an Electron module passed in as a
    // parameter, but only when the identifier is a real Electron export
    // imported somewhere in the app.
    let mut collected: Vec<(String, String)> = Vec::new();
    for entry in WalkDir::new(app_dir)
        .into_iter()
        .filter_entry(|e| !is_skipped_dir(e))
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_scannable(path) {
            continue;
        }
        let rel = relative_path(app_dir, path);
        if is_test_path(&rel) {
            continue;
        }
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // unreadable (binary, permissions) — skip
        };
        collected.push((rel, strip_comments(&content)));
    }
    collected.sort_by(|a, b| a.0.cmp(&b.0));

    let mut global_exports: HashSet<String> = HashSet::new();
    let mut bindings_by_file: HashMap<String, ElectronBindings> = HashMap::new();
    let mut shadows_by_file: HashMap<String, HashSet<String>> = HashMap::new();
    let mut remote_derived_by_file: HashMap<String, HashSet<String>> = HashMap::new();
    // One-line `ipcMain` wrapper functions, app-wide: a wrapper defined
    // in one file is imported and called from others (N6).
    let mut ipc_wrappers: HashMap<String, (&'static str, &'static str)> = HashMap::new();
    let mut wrapper_def_lines_by_file: HashMap<String, HashSet<usize>> = HashMap::new();
    for (rel, stripped) in &collected {
        let bindings = parse_electron_bindings(&patterns, stripped);
        global_exports.extend(bindings.electron.values().cloned());
        global_exports.extend(bindings.remote.values().cloned());
        // A name annotated with an Electron type is an Electron object
        // passed in — it must not stay a file-local shadow (the
        // typed-parameter regex records it as one).
        let mut shadows = local_shadow_names(&patterns, stripped);
        for name in &bindings.typed_electron_params {
            shadows.remove(name);
        }
        bindings_by_file.insert(rel.clone(), bindings);
        shadows_by_file.insert(rel.clone(), shadows);
        remote_derived_by_file.insert(rel.clone(), remote_derived_names(stripped));
        let (wrappers, def_lines) = find_ipc_wrappers(stripped);
        for (name, method_api) in wrappers {
            ipc_wrappers.insert(name, method_api);
        }
        wrapper_def_lines_by_file.insert(rel.clone(), def_lines);
    }
    // Precompiled wrapper call matchers: (literal-channel, dynamic-channel,
    // direction, finding API).
    let mut wrapper_res: Vec<(Regex, Regex, &'static str, &'static str)> = Vec::new();
    {
        let mut names: Vec<(&String, &(&'static str, &'static str))> =
            ipc_wrappers.iter().collect();
        names.sort();
        for (name, (method, api)) in names {
            let direction = if *method == "on" { "on" } else { "handle" };
            let literal = Regex::new(&format!(
                r#"\b{}\s*\(\s*['"]([^'"]+)['"]"#,
                regex::escape(name)
            ))
            .expect("wrapper literal regex");
            let dynamic = Regex::new(&format!(
                r"\b{}\s*\(\s*([A-Za-z_$][\w$]*)",
                regex::escape(name)
            ))
            .expect("wrapper dynamic regex");
            wrapper_res.push((literal, dynamic, direction, api));
        }
    }
    // A file references an Electron export when a global export name is
    // followed by a call-site suffix — the DI gate.
    let di_gate: Option<Regex> = if global_exports.is_empty() {
        None
    } else {
        let mut names: Vec<String> = global_exports.iter().cloned().collect();
        names.sort();
        Regex::new(&format!(
            r"\b(?:{})\s*[\.\(\[\]]",
            names
                .iter()
                .map(|n| regex::escape(n))
                .collect::<Vec<_>>()
                .join("|")
        ))
        .ok()
    };

    let mut files_scanned = 0usize;
    let mut ipc_channels = Vec::new();
    let mut main_to_renderer = Vec::new();
    let mut findings: Vec<Finding> = Vec::new();
    let mut seen_findings: HashMap<(String, String), usize> = HashMap::new();
    let mut windows = Vec::new();
    let mut all_menus: Vec<MenuInfo> = Vec::new();
    let mut menu_files: Vec<(String, String, Vec<String>)> = Vec::new();
    let mut preload_files: BTreeSet<String> = BTreeSet::new();
    let mut preload_modules: BTreeSet<String> = BTreeSet::new();
    let mut exposed_keys: BTreeSet<String> = BTreeSet::new();
    let mut preload_shapes: Vec<PreloadShape> = Vec::new();
    let mut uses_context_bridge = false;
    let mut uses_ipc_renderer = false;
    let mut dev_url_candidates: Vec<(DevUrlInfo, i32)> = Vec::new();

    // Pass 2 — scan each collected file.
    for (rel, stripped) in &collected {
        let rel = rel.as_str();
        let stripped = stripped.as_str();
        let bindings = bindings_by_file.get(rel).cloned().unwrap_or_default();
        let shadows = shadows_by_file.get(rel).cloned().unwrap_or_default();
        files_scanned += 1;

        let path = app_dir.join(rel);
        // Preload entry points only (N4): a file named as an entry
        // point. A `*.preload.ts` module is recorded separately —
        // it runs in preload context but no window loads it.
        if is_preload_entry_filename(&path) {
            preload_files.insert(rel.to_string());
        } else if is_preload_module_filename(&path) {
            preload_modules.insert(rel.to_string());
        }

        let imports_electron = patterns.electron_import.is_match(stripped);
        let gate_open =
            imports_electron || di_gate.as_ref().is_some_and(|re| re.is_match(stripped));
        let remote_derived = remote_derived_by_file.get(rel).cloned().unwrap_or_default();

        // IPC channels: every file, including renderer and preload code
        // that never imports Electron directly.
        let wrapper_def_lines = wrapper_def_lines_by_file
            .get(rel)
            .cloned()
            .unwrap_or_default();
        for (channel, api) in
            collect_ipc_channels(&patterns, stripped, rel, &wrapper_res, &wrapper_def_lines)
        {
            if api.starts_with("ipcRenderer") {
                uses_ipc_renderer = true;
            }
            add_finding(
                &mut findings,
                &mut seen_findings,
                api,
                "ipc",
                rel,
                channel.line,
            );
            ipc_channels.push(channel);
        }

        // Main → renderer channels: every file (the pattern is unambiguous).
        for (channel, is_remote) in collect_send_channels(&patterns, stripped, rel, &remote_derived)
        {
            add_finding_override(
                &mut findings,
                &mut seen_findings,
                "webContents.send",
                "webcontents",
                rel,
                channel.line,
                is_remote,
            );
            main_to_renderer.push(channel);
        }

        // `<webview>` tags and the `webviewTag` webPreference (N6):
        // renderer code never imports Electron for these, so they run
        // ungated on the string-stripped line (a `<webview` inside a
        // string literal is not a tag).
        for (idx, line) in stripped.lines().enumerate() {
            let clean = strip_string_literals(line);
            if clean.contains("<webview")
                || clean.contains("webviewTag: true")
                || clean.contains("webviewTag:true")
            {
                add_finding(
                    &mut findings,
                    &mut seen_findings,
                    "webview",
                    "webcontents",
                    rel,
                    idx + 1,
                );
            }
        }

        // contextBridge: every file (preload scripts live here). The
        // call-based scan also recovers each exposed object's shape.
        for m in patterns.expose_call.find_iter(stripped) {
            // The match ends right after `(`, so the paren is at end - 1.
            let open_idx = m.end() - 1;
            let Some((args, _)) = extract_balanced(stripped, open_idx, '(', ')') else {
                continue;
            };
            let parts = split_top_level_members(&args);
            let Some(key_raw) = parts.first() else {
                continue;
            };
            let key = key_raw
                .trim()
                .trim_matches(|c| c == '\'' || c == '"')
                .to_string();
            if key.is_empty() {
                continue;
            }
            uses_context_bridge = true;
            exposed_keys.insert(key.clone());
            let line = line_number(stripped, m.start());
            add_finding(
                &mut findings,
                &mut seen_findings,
                "contextBridge",
                "preload",
                rel,
                line,
            );
            if let Some(shape) = extract_preload_shape(stripped, rel, &key, open_idx) {
                preload_shapes.push(shape);
            }
        }

        // `window.<key> = {...}` assignments in preload entry files
        // (Joplin's secondary-window preload exposes its API without
        // contextBridge).
        if is_preload_entry_filename(&path) {
            preload_shapes.extend(extract_window_assign_shape(&patterns, stripped, rel));
        }

        // Main-process APIs, windows, menus, and dev URLs: only files
        // that import Electron (or an ecosystem package), or that
        // reference an Electron export passed in by DI / as a parameter
        // — so unrelated code with a variable named `app` or `net` does
        // not pollute the report, while DI-wired files are not dropped.
        if gate_open {
            for (idx, line) in stripped.lines().enumerate() {
                let line_no = idx + 1;
                // Non-IPC API patterns run on the string-stripped line
                // (aliases normalized), so `'app.asar'`, `"…provider
                // screen."`, and import paths cannot match. Patterns
                // whose signal *is* a string run on the intact line.
                let clean = strip_string_literals(line);
                let normalized = normalize_aliases(&clean, &bindings);
                let line_has_remote_inline = patterns.remote_require.is_match(line);
                let line_has_electron_inline = line.contains("require('electron').")
                    || line.contains("require(\"electron\").");
                // `electron-updater`'s `autoUpdater` is not Electron's
                // (N6): when this line calls one through a local bound
                // from `'electron-updater'` (`import { autoUpdater as
                // electronUpdater }`), the finding is `electron-updater`
                // and the `autoUpdater` pattern must not also fire
                // (alias normalisation rewrites the local to
                // `autoUpdater` first). Checked on the pre-normalised
                // line, where the local name still exists.
                let uses_updater_alias = !bindings.electron_updater_locals.is_empty()
                    && bindings.electron_updater_locals.iter().any(|local| {
                        let mut start = 0usize;
                        while let Some(idx) = clean[start..].find(local.as_str()) {
                            let pos = start + idx;
                            let before_ok = if pos == 0 {
                                true
                            } else {
                                let prev = clean.as_bytes()[pos - 1];
                                !(prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'$')
                            };
                            let after = clean[pos + local.len()..].chars().next().unwrap_or(' ');
                            if before_ok && (after == '.' || after == '(' || after == '[') {
                                return true;
                            }
                            start = pos + local.len();
                        }
                        false
                    });
                if uses_updater_alias {
                    add_finding(
                        &mut findings,
                        &mut seen_findings,
                        "electron-updater",
                        "updater",
                        rel,
                        line_no,
                    );
                }
                let mut matched_on_line: Vec<&str> = Vec::new();
                let mut remote_on_line = false;
                for (pat, re) in &patterns.api_patterns {
                    // The aliased updater is already filed above as
                    // `electron-updater`; do not also file it as
                    // Electron's `autoUpdater`.
                    if pat.api == "autoUpdater" && uses_updater_alias {
                        continue;
                    }
                    if let Some(prefix) = pat.skip_if_prefix_matched {
                        if matched_on_line.iter().any(|a| a.starts_with(prefix)) {
                            continue;
                        }
                    }
                    let target = if pat.match_in_strings {
                        line
                    } else {
                        normalized.as_str()
                    };
                    let Some(m) = re.find(target) else {
                        continue;
                    };
                    let mut force_manual = false;
                    if pat.needs_binding {
                        match resolve_match(
                            target,
                            &m,
                            &bindings,
                            &global_exports,
                            &shadows,
                            line_has_remote_inline,
                            line_has_electron_inline,
                        ) {
                            None => continue,
                            Some(is_remote) => {
                                force_manual = is_remote;
                                remote_on_line |= is_remote;
                            }
                        }
                    } else {
                        // Ungated patterns (`webContents.*`) never see
                        // `resolve_match`, so remote mediation has to
                        // be detected from the line itself: an inline
                        // remote require, or a receiver that derives
                        // from remote (`webContents` assigned from
                        // `getCurrentWindow().webContents` — N8).
                        let head = match_head(&m);
                        let qualifier = match_qualifier(target, &m);
                        if line_has_remote_inline
                            || head.as_ref().is_some_and(|h| remote_derived.contains(h))
                            || qualifier
                                .as_ref()
                                .is_some_and(|q| remote_derived.contains(q))
                        {
                            force_manual = true;
                            remote_on_line = true;
                        }
                    }
                    matched_on_line.push(pat.api);
                    // `webContents.send` findings come from the
                    // dedicated send-channel pass above (which also
                    // covers files the gate does not open); recording
                    // them here too would double-count occurrences.
                    // The pattern still participates in
                    // `matched_on_line` so the generic `webContents`
                    // fallback skips the same line.
                    if pat.api == "webContents.send" {
                        continue;
                    }
                    add_finding_override(
                        &mut findings,
                        &mut seen_findings,
                        pat.api,
                        pat.category,
                        rel,
                        line_no,
                        force_manual,
                    );
                }
                // A remote-mediated call site needs a `remote` finding
                // of its own — once per line, and not when the `remote`
                // pattern itself already recorded one on this line.
                // This also catches inline-require member access with
                // no dedicated pattern (`require('@electron/remote').process`,
                // `.require`, `.getGlobal` is covered by its own pattern).
                let mut needs_remote_finding = remote_on_line;
                if line_has_remote_inline {
                    if let Some(m) = patterns.remote_require.find(line) {
                        if line[m.end()..].trim_start().starts_with('.') {
                            needs_remote_finding = true;
                        }
                    }
                }
                if needs_remote_finding && !matched_on_line.contains(&"remote") {
                    add_finding(
                        &mut findings,
                        &mut seen_findings,
                        "remote",
                        "remote",
                        rel,
                        line_no,
                    );
                }

                // Session methods on a typed `Electron.Session`
                // parameter (or a session alias) — `setProxy`,
                // `resolveProxy`, `setPermissionRequestHandler` and
                // friends — are real `session` API sites even when the
                // receiver is a parameter that never appears in a
                // binding map (Signal's protocol handler, Joplin's
                // `handleCustomProtocols`). Skip when the line already
                // matched `session` (the `session.*` pattern fired).
                if !matched_on_line.iter().any(|a| a.starts_with("session")) {
                    if let Some(receiver) = session_method_call(line, &bindings.typed_session_names)
                    {
                        let is_remote =
                            remote_derived.contains(&receiver) || line_has_remote_inline;
                        add_finding_override(
                            &mut findings,
                            &mut seen_findings,
                            "session",
                            "session",
                            rel,
                            line_no,
                            is_remote,
                        );
                    }
                }
            }

            let (mut found_windows, mut preload_paths) = extract_windows(&patterns, stripped, rel);
            // Fixture windows in simulation scenes (`simulation/
            // *.electron.ts` — VS Code's virtual-workbench fixture) are
            // test doubles, not app windows; `is_test_path` does not
            // exclude them because they are not under a test directory.
            if rel.to_ascii_lowercase().contains("simulation") {
                found_windows.clear();
                preload_paths.clear();
            }
            for w in &found_windows {
                // A window created through `@electron/remote` is a
                // MANUAL remote call, not an AUTO WebviewWindow.
                let remote_window = bindings.remote.contains_key("BrowserWindow")
                    || stripped
                        .lines()
                        .nth(w.line.saturating_sub(1))
                        .is_some_and(|l| patterns.remote_require.is_match(l));
                add_finding_override(
                    &mut findings,
                    &mut seen_findings,
                    "BrowserWindow",
                    "window",
                    rel,
                    w.line,
                    remote_window,
                );
            }
            windows.append(&mut found_windows);
            preload_paths.extend(collect_preload_paths(&patterns, stripped, rel));
            for p in preload_paths {
                // Resolve the preload path against the app tree and
                // verify it exists (N1): root-relative
                // `FileAccess.asFileUri` paths, `path.join` expressions
                // and `.min.js` bundles that have no source file must
                // not become phantom preload files.
                if let Some(resolved) = resolve_preload_reference(app_dir, rel, &p) {
                    preload_files.insert(resolved);
                }
            }

            // Menus: collect ALL buildFromTemplate sites (the old code
            // kept only the first one in walk order, which was often a
            // context/tray menu or a test fixture). The application menu
            // is picked after the walk (see below).
            let file_menus = extract_menus(&patterns, stripped, rel);
            if !file_menus.is_empty() {
                // A menu built through `@electron/remote` (its `Menu`
                // binding, or a remote require on the template line)
                // is a MANUAL remote call, not an AUTO menu (N8).
                let remote_menu = bindings.remote.contains_key("Menu")
                    || stripped
                        .lines()
                        .nth(file_menus[0].line.saturating_sub(1))
                        .is_some_and(|l| patterns.remote_require.is_match(l));
                add_finding_override(
                    &mut findings,
                    &mut seen_findings,
                    "Menu",
                    "menu",
                    rel,
                    file_menus[0].line,
                    remote_menu,
                );
                all_menus.extend(file_menus);
            }
            let set_app_menu_args = collect_set_application_menu(stripped);
            if !set_app_menu_args.is_empty() {
                menu_files.push((rel.to_string(), stripped.to_string(), set_app_menu_args));
            }

            dev_url_candidates.extend(find_dev_url_candidates(&patterns, stripped, rel));
        }
    }

    // Pick the application menu from all collected sites: a template
    // passed to `Menu.setApplicationMenu` wins (nested call, or a
    // variable assigned from `buildFromTemplate` in the same file);
    // otherwise a menu in a file whose name says application menu
    // (`application-menu`, `menubar`, `build-default-menu`); otherwise
    // the first site in (file, line) order. Tray / context / spell-check
    // menus stay in `menus` — they are real menus, just not the app menu.
    all_menus.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));
    let mut menu: Option<MenuInfo> = None;
    if !all_menus.is_empty() {
        let mut best_idx = 0usize;
        let mut best_score = i32::MIN;
        for (idx, m) in all_menus.iter().enumerate() {
            let mut score = 0i32;
            let file_lower = m.file.to_ascii_lowercase();
            if file_lower.contains("application-menu")
                || file_lower.contains("menubar")
                || file_lower.contains("build-default-menu")
                || file_lower.contains("app-menu")
            {
                score += 5;
            }
            if file_lower.contains("context")
                || file_lower.contains("tray")
                || file_lower.contains("spell")
            {
                score -= 5;
            }
            // A menu under an extension, a fixture or a simulation
            // scene is test scaffolding, not the app menu — VS Code's
            // virtual-workbench fixture has a full menu template.
            if file_lower.contains("extensions/")
                || file_lower.contains("fixture")
                || file_lower.contains("simulation")
            {
                score -= 20;
            }
            for (menu_file, menu_content, args) in &menu_files {
                if menu_file != &m.file {
                    continue;
                }
                score += 3;
                for arg in args {
                    if arg.contains("buildFromTemplate") {
                        // Nested: `setApplicationMenu(buildFromTemplate(...))`
                        // on the same line as this menu site.
                        if let Some(line_text) = menu_content.lines().nth(m.line - 1) {
                            if line_text.contains("setApplicationMenu")
                                && line_text.contains("buildFromTemplate")
                            {
                                score += 10;
                            }
                        }
                    } else {
                        // Variable: `const menu = Menu.buildFromTemplate(...)`
                        // then `setApplicationMenu(menu)`.
                        let arg_ident: String = arg
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
                            .collect();
                        if !arg_ident.is_empty() {
                            if let Some(line_text) = menu_content.lines().nth(m.line - 1) {
                                if line_text.contains(&format!("{arg_ident} ="))
                                    || line_text.contains(&format!("{arg_ident}="))
                                {
                                    score += 10;
                                }
                            }
                        }
                    }
                }
            }
            if score > best_score {
                best_score = score;
                best_idx = idx;
            }
        }
        let mut chosen = all_menus[best_idx].clone();
        chosen.is_application_menu = true;
        all_menus[best_idx].is_application_menu = true;
        menu = Some(chosen);
    }

    // Build tooling and port-needing JS dependencies are data, not
    // findings (N9): `package.json:1` dependency declarations used to
    // appear as findings and skewed the group counts. They still show
    // in the report — build tooling in `ScanReport.build_tooling`, port
    // deps in `ScanReport.port_needing_deps` — but they no longer count
    // toward MANUAL/ASSISTED groups or the feasibility score.

    // Deterministic order, independent of filesystem walk order.
    findings.sort_by(|a, b| {
        (a.file.as_str(), a.line, a.api.as_str()).cmp(&(b.file.as_str(), b.line, b.api.as_str()))
    });
    ipc_channels.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));
    main_to_renderer.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));
    windows.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));
    preload_shapes.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));

    // Dev URL: rank candidates by score (the app's own dev server —
    // a `loadURL` in main-process code — wins over a service URL in an
    // extension or a CLI docs server); a candidate must score at least
    // `DEV_URL_MIN_SCORE`, so a low-ranked URL is reported as no dev
    // URL rather than a wrong one (N11). The bar is the file-loads-
    // URLs signal (+10), not just the main-process path bonus (+5):
    // VS Code's OAuth loopback port (127.0.0.1:33418, a redirect URI in
    // `mainThreadAuthentication.ts`) scores 5 on the path bonus alone
    // and is not a dev server — VS Code has none — so it must be
    // rejected, while Insomnia's real dev server (localhost:3334 in a
    // window-loading file, score 15) is kept. Deterministic across
    // walks.
    dev_url_candidates.sort_by(|a, b| {
        (std::cmp::Reverse(a.1), a.0.file.as_str(), a.0.line).cmp(&(
            std::cmp::Reverse(b.1),
            b.0.file.as_str(),
            b.0.line,
        ))
    });
    let dev_url = dev_url_candidates
        .into_iter()
        .next()
        .filter(|(_, score)| *score >= DEV_URL_MIN_SCORE)
        .map(|(d, _)| d);

    let manual = findings
        .iter()
        .filter(|f| f.classification == Classification::Manual)
        .count();
    let assisted = findings
        .iter()
        .filter(|f| f.classification == Classification::Assisted)
        .count();
    let manual_occurrences = findings
        .iter()
        .filter(|f| f.classification == Classification::Manual)
        .map(|f| f.occurrences)
        .sum();
    let total_occurrences = findings.iter().map(|f| f.occurrences).sum();
    let has_preload = !preload_files.is_empty();
    let score = feasibility_score(
        pkg.native_modules.len(),
        manual,
        assisted,
        manual_occurrences,
        total_occurrences,
        has_preload,
        uses_ipc_renderer,
    );

    let app_name = pkg.name.or_else(|| {
        app_dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
    });

    Ok(ScanReport {
        app_name: app_name.unwrap_or_else(|| "unknown-app".to_string()),
        app_version: pkg.version,
        product_name: pkg.product_name,
        app_id: pkg.app_id,
        electron_version: pkg.electron_version,
        main_entry: pkg.main_entry,
        files_scanned,
        ipc_channels,
        main_to_renderer,
        findings,
        preload: PreloadInfo {
            files: preload_files.into_iter().collect(),
            modules: preload_modules.into_iter().collect(),
            uses_context_bridge,
            exposed_keys: exposed_keys.into_iter().collect(),
            shapes: preload_shapes,
        },
        native_modules: pkg.native_modules,
        port_needing_deps: pkg.port_needing_deps,
        build_tooling: pkg.build_tooling,
        dev_url,
        feasibility_score: score,
        verdict: verdict(score).to_string(),
        windows,
        menu,
        menus: all_menus,
    })
}

/// One window's size as printed in the human report: `1024x768` when
/// known, `?` per unknown dimension — never a confident-looking default
/// for a size the scanner could not resolve.
fn format_window_size(w: &WindowInfo) -> String {
    let width = if w.width_known {
        w.width.to_string()
    } else {
        "?".to_string()
    };
    let height = if w.height_known {
        w.height.to_string()
    } else {
        "?".to_string()
    };
    format!("{width}x{height}")
}

/// Render a [`ScanReport`] as a human-readable summary for the terminal.
pub fn format_human_report(report: &ScanReport) -> String {
    let mut out = String::new();

    let title = format!("Electron → Tauri 2 scan: {}", report.app_name);
    out.push_str(&title);
    out.push('\n');
    out.push_str(&"=".repeat(title.chars().count()));
    out.push_str("\n\n");
    out.push_str(&format!(
        "Electron version: {}\n",
        report.electron_version.as_deref().unwrap_or("not declared")
    ));
    if let Some(version) = &report.app_version {
        out.push_str(&format!("App version:      {version}\n"));
    }
    if let Some(product) = &report.product_name {
        out.push_str(&format!("Product name:     {product}\n"));
    }
    if let Some(app_id) = &report.app_id {
        out.push_str(&format!("App ID:           {app_id}\n"));
    }
    out.push_str(&format!(
        "Main entry:       {}\n",
        report.main_entry.as_deref().unwrap_or("not found")
    ));
    out.push_str(&format!("Files scanned:    {}\n", report.files_scanned));
    if let Some(dev) = &report.dev_url {
        out.push_str(&format!(
            "Dev URL:          {} (from {}:{})\n",
            dev.url, dev.file, dev.line
        ));
    }

    out.push_str(&format!("\nWindows ({})\n", report.windows.len()));
    if report.windows.is_empty() {
        out.push_str("  (none found)\n");
    }
    for w in &report.windows {
        let title = w.title.as_deref().unwrap_or("(no title)");
        let resizable = if w.resizable {
            "resizable"
        } else {
            "fixed size"
        };
        let frame = if w.frame { "framed" } else { "frameless" };
        let mut extra = String::new();
        if let (Some(min_w), Some(min_h)) = (w.min_width, w.min_height) {
            extra.push_str(&format!(", min {min_w}x{min_h}"));
        } else if let Some(min_w) = w.min_width {
            extra.push_str(&format!(", min width {min_w}"));
        } else if let Some(min_h) = w.min_height {
            extra.push_str(&format!(", min height {min_h}"));
        }
        if let Some(bg) = &w.background_color {
            extra.push_str(&format!(", bg {bg}"));
        }
        if let Some(icon) = &w.icon {
            extra.push_str(&format!(", icon {icon}"));
        }
        let mut size_note = String::new();
        if w.from_window_state {
            size_note.push_str(" (size from electron-window-state defaults)");
        } else if !w.width_known || !w.height_known {
            size_note.push_str(" (size unknown — dynamic dimensions, not resolved)");
        }
        out.push_str(&format!(
            "  - {} \"{}\" ({}, {}{}) — {}:{}{}\n",
            format_window_size(w),
            title,
            resizable,
            frame,
            extra,
            w.file,
            w.line,
            size_note
        ));
    }

    out.push_str(&format!("\nIPC channels ({})\n", report.ipc_channels.len()));
    if report.ipc_channels.is_empty() {
        out.push_str("  (none found)\n");
    }
    for ch in &report.ipc_channels {
        out.push_str(&format!(
            "  - {:<6} '{}' — {}:{}\n",
            ch.direction, ch.name, ch.file, ch.line
        ));
    }

    out.push_str(&format!(
        "\nMain → renderer events ({})\n",
        report.main_to_renderer.len()
    ));
    if report.main_to_renderer.is_empty() {
        out.push_str("  (none found)\n");
    }
    for ch in &report.main_to_renderer {
        out.push_str(&format!(
            "  - '{}' (webContents.send) — {}:{}\n",
            ch.name, ch.file, ch.line
        ));
    }

    out.push_str(&format!(
        "\nPreload ({} file{}, contextBridge: {})\n",
        report.preload.files.len(),
        if report.preload.files.len() == 1 {
            ""
        } else {
            "s"
        },
        if report.preload.uses_context_bridge {
            "yes"
        } else {
            "no"
        }
    ));
    if report.preload.files.is_empty() {
        out.push_str("  (none found)\n");
    }
    for f in &report.preload.files {
        out.push_str(&format!("  - {f}\n"));
    }
    // Preload-context modules (N4): files named `*.preload.ts` that
    // run in the preload script but no window loads — listed, not
    // counted as preload entry points.
    if !report.preload.modules.is_empty() {
        out.push_str(&format!(
            "  Preload-context modules ({} — bundled into the preload script, not entry points)\n",
            report.preload.modules.len()
        ));
        for f in &report.preload.modules {
            out.push_str(&format!("  - {f}\n"));
        }
    }
    if !report.preload.exposed_keys.is_empty() {
        out.push_str(&format!(
            "  Exposed keys: {}\n",
            report.preload.exposed_keys.join(", ")
        ));
    }

    if !report.menus.is_empty() {
        out.push_str(&format!("\nMenus ({})\n", report.menus.len()));
        for m in &report.menus {
            let kind = if m.is_application_menu {
                "application menu"
            } else if m.is_static_literal {
                "static template literal"
            } else {
                "built from a variable/expression (needs review)"
            };
            out.push_str(&format!("  - {} — {}:{}\n", kind, m.file, m.line));
        }
    } else if let Some(menu) = &report.menu {
        out.push_str("\nMenu\n");
        if menu.is_static_literal {
            out.push_str(&format!(
                "  Static template literal — {}:{}\n",
                menu.file, menu.line
            ));
        } else {
            out.push_str(&format!(
                "  Built from a variable/expression (needs review) — {}:{}\n",
                menu.file, menu.line
            ));
        }
    }

    out.push_str(&format!("\nFindings ({})\n", report.findings.len()));
    if report.findings.is_empty() {
        out.push_str("  No Electron API usage detected.\n");
    }
    for (label, class) in [
        ("AUTO", Classification::Auto),
        ("ASSISTED", Classification::Assisted),
        ("MANUAL", Classification::Manual),
    ] {
        let group: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.classification == class)
            .collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("  {label} ({})\n", group.len()));
        for f in group {
            let target = f.tauri_target.as_deref().unwrap_or("no direct target");
            let count = if f.occurrences > 1 {
                format!(" (×{})", f.occurrences)
            } else {
                String::new()
            };
            // Remote-mediated MANUAL findings (N8): the API itself
            // may have a Tauri equivalent — the port is hard because
            // the call goes through `@electron/remote`.
            let remote = if f.remote_mediated {
                " [remote-mediated]"
            } else {
                ""
            };
            out.push_str(&format!(
                "    - {} — {}:{} → {}{}{}\n",
                f.api, f.file, f.line, target, count, remote
            ));
            if class == Classification::Manual {
                out.push_str(&format!("      {}\n", f.note));
            }
        }
    }

    out.push_str(&format!(
        "\nNative modules ({})\n",
        report.native_modules.len()
    ));
    if report.native_modules.is_empty() {
        out.push_str("  (none)\n");
    }
    for m in &report.native_modules {
        let hint = mappings::lookup(m)
            .and_then(|mapping| mapping.tauri_target.clone())
            .map(|t| format!(" → {t}"))
            .unwrap_or_default();
        out.push_str(&format!("  - {m}{hint}\n"));
    }

    out.push_str(&format!(
        "\nPort-needing JS dependencies ({})\n",
        report.port_needing_deps.len()
    ));
    if report.port_needing_deps.is_empty() {
        out.push_str("  (none)\n");
    }
    for m in &report.port_needing_deps {
        let hint = mappings::lookup(m)
            .and_then(|mapping| mapping.tauri_target.clone())
            .map(|t| format!(" → {t}"))
            .unwrap_or_else(|| " → no Tauri equivalent".to_string());
        out.push_str(&format!("  - {m}{hint}\n"));
    }

    // Build tooling is a note, not a finding (N9): it shows here so
    // the report still records what the app builds with.
    out.push_str(&format!(
        "\nBuild tooling ({})\n",
        report.build_tooling.len()
    ));
    if report.build_tooling.is_empty() {
        out.push_str("  (none)\n");
    }
    for m in &report.build_tooling {
        out.push_str(&format!("  - {m} → cargo tauri build\n"));
    }

    out.push_str(&format!(
        "\nFeasibility: {}/100 — {}\n",
        report.feasibility_score, report.verdict
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_window_options_are_extracted() {
        let patterns = Patterns::new();
        let options = "{\n    width: 1280,\n    height: 720,\n    title: 'My App',\n    resizable: false,\n    frame: false,\n  }";
        let win = extract_window_options(&patterns, options, "main.js", 7, options);
        assert_eq!(win.width, 1280);
        assert_eq!(win.height, 720);
        assert!(win.width_known);
        assert!(win.height_known);
        assert!(!win.from_window_state);
        assert_eq!(win.title.as_deref(), Some("My App"));
        assert!(!win.resizable);
        assert!(!win.frame);
        assert_eq!(win.file, "main.js");
        assert_eq!(win.line, 7);
    }

    #[test]
    fn browser_window_options_default_when_missing() {
        let patterns = Patterns::new();
        // No literals and no window-state defaults: the size is unknown,
        // not a confident 800x600.
        let win = extract_window_options(
            &patterns,
            "{ webPreferences: {} }",
            "main.js",
            1,
            "{ webPreferences: {} }",
        );
        assert_eq!(win.width, 800);
        assert_eq!(win.height, 600);
        assert!(!win.width_known);
        assert!(!win.height_known);
        assert_eq!(win.title, None);
        assert!(win.resizable);
        assert!(win.frame);
    }

    #[test]
    fn window_state_defaults_resolve_dynamic_dimensions() {
        let patterns = Patterns::new();
        // Simplenote's shape: dynamic width/height, defaults in the same file.
        let content = "const mainWindowState = windowStateKeeper({\n  defaultWidth: 1024,\n  defaultHeight: 768,\n});\nnew BrowserWindow({\n  width: mainWindowState.width,\n  height: mainWindowState.height,\n});";
        let options = "{\n  width: mainWindowState.width,\n  height: mainWindowState.height,\n}";
        let win = extract_window_options(&patterns, options, "desktop/app.js", 5, content);
        assert_eq!(win.width, 1024);
        assert_eq!(win.height, 768);
        assert!(win.width_known);
        assert!(win.height_known);
        assert!(win.from_window_state);
    }

    #[test]
    fn window_options_carry_min_size_background_and_icon() {
        let patterns = Patterns::new();
        let options = "{\n  width: 1100,\n  height: 800,\n  minWidth: 370,\n  minHeight: 520,\n  backgroundColor: '#fff',\n  icon: `${__dirname}/assets/icon.png`,\n}";
        let win = extract_window_options(&patterns, options, "main.js", 1, options);
        assert_eq!(win.min_width, Some(370));
        assert_eq!(win.min_height, Some(520));
        assert_eq!(win.background_color.as_deref(), Some("#fff"));
        assert_eq!(win.icon.as_deref(), Some("assets/icon.png"));
    }

    #[test]
    fn window_icon_from_nested_file_resolves_against_its_dir() {
        let patterns = Patterns::new();
        let options = "{ width: 800, height: 600, icon: path.join(__dirname, 'icon.png') }";
        let win = extract_window_options(&patterns, options, "desktop/app.js", 1, options);
        assert_eq!(win.icon.as_deref(), Some("desktop/icon.png"));
    }

    #[test]
    fn ipc_channel_regex_matches() {
        let patterns = Patterns::new();
        let content = "ipcMain.handle('read-file', async () => {});\n\
             ipcMain.on(\"log\", () => {});\n\
             ipcRenderer.invoke('get-version');\n\
             ipcRenderer.send('ping');\n\
             ipcRenderer.on('reply', () => {});\n";
        let channels = collect_ipc_channels(&patterns, content, "main.js", &[], &HashSet::new());
        assert_eq!(channels.len(), 5);
        assert_eq!(channels[0].0.name, "read-file");
        assert_eq!(channels[0].0.direction, "handle");
        assert_eq!(channels[0].1, "ipcMain.handle");
        assert_eq!(channels[1].0.name, "log");
        assert_eq!(channels[1].0.direction, "on");
        assert_eq!(channels[1].1, "ipcMain.on");
        assert_eq!(channels[2].0.direction, "send");
        assert_eq!(channels[2].1, "ipcRenderer.invoke");
        assert_eq!(channels[3].1, "ipcRenderer.send");
        assert_eq!(channels[4].0.name, "reply");
        // ipcRenderer.on is a *listen* (main → renderer), not an `on` the
        // Rust side must receive.
        assert_eq!(channels[4].0.direction, "listen");
        assert_eq!(channels[4].0.line, 5);
    }

    #[test]
    fn webcontents_send_channels_are_collected() {
        let patterns = Patterns::new();
        let content = "mainWindow.webContents.send('wpLogin', url);\n\
             win.webContents.send('appCommand', { action: 'close' });\n\
             focusedWindow.webContents.send(commandName, arg);\n";
        let channels = collect_send_channels(&patterns, content, "app.js", &HashSet::new());
        // The variable channel (`commandName`) cannot be resolved
        // statically, but it is still a send site: it is recorded as
        // "dynamic" so the channel count is honest.
        assert_eq!(channels.len(), 3);
        assert_eq!(channels[0].0.name, "wpLogin");
        assert_eq!(channels[0].0.direction, "emit");
        assert_eq!(channels[1].0.name, "appCommand");
        assert_eq!(channels[1].0.line, 2);
        assert_eq!(channels[2].0.name, "dynamic");
        assert_eq!(channels[2].0.line, 3);
        // None of these sends is remote-mediated.
        assert!(channels.iter().all(|(_, remote)| !remote));
    }

    #[test]
    fn classification_lookup_uses_mappings() {
        let (class, target, _) = classify("ipcMain.handle");
        assert_eq!(class, Classification::Auto);
        assert_eq!(target.as_deref(), Some("#[tauri::command]"));

        let (class, _, _) = classify("session");
        assert_eq!(class, Classification::Manual);

        let (class, _, _) = classify("Menu");
        assert_eq!(class, Classification::Assisted);

        // B2: app.getPath is the core path API, not the opener plugin.
        let (class, target, _) = classify("app.getPath");
        assert_eq!(class, Classification::Auto);
        assert_eq!(target.as_deref(), Some("tauri path API (app.path())"));
        assert_eq!(mappings::lookup("app.getPath").unwrap().plugin, None);

        // B5: nodeIntegration is MANUAL; B13: remote is MANUAL.
        let (class, _, _) = classify("nodeIntegration");
        assert_eq!(class, Classification::Manual);
        let (class, _, _) = classify("remote");
        assert_eq!(class, Classification::Manual);

        let (class, target, note) = classify("some-unknown-api");
        assert_eq!(class, Classification::Assisted);
        assert_eq!(target, None);
        assert!(note.contains("No mapping"));
    }

    #[test]
    fn feasibility_scoring() {
        // Signature: (natives, manual groups, assisted groups, manual
        // occurrences, total occurrences, has_preload, uses_ipc_renderer).
        assert_eq!(feasibility_score(0, 0, 0, 0, 0, true, false), 100);
        // One native module costs 10, not 15.
        assert_eq!(feasibility_score(1, 0, 0, 0, 0, true, false), 90);
        // Manual groups cost 2 each.
        assert_eq!(feasibility_score(0, 2, 0, 0, 0, true, false), 96);
        // Manual penalty caps at -30 (15 groups).
        assert_eq!(feasibility_score(0, 10, 0, 0, 0, true, false), 80);
        assert_eq!(feasibility_score(0, 40, 0, 0, 0, true, false), 70);
        // Native penalty caps at -30 (3 modules).
        assert_eq!(feasibility_score(5, 0, 0, 0, 0, true, false), 70);
        // Assisted: only the ones beyond the first 5 count, capped at -15.
        assert_eq!(feasibility_score(0, 0, 5, 0, 0, true, false), 100);
        assert_eq!(feasibility_score(0, 0, 8, 0, 0, true, false), 97);
        assert_eq!(feasibility_score(0, 0, 20, 0, 0, true, false), 85);
        // Manual-share term: same group counts, different shares.
        assert_eq!(
            feasibility_score(1, 10, 10, 10, 100, true, false),
            63,
            "10% manual share costs 2"
        );
        assert_eq!(
            feasibility_score(1, 10, 10, 60, 100, true, false),
            53,
            "60% manual share costs 12 — the score must discriminate \
             beyond the group caps (N7)"
        );
        // IPC without a preload script costs 10.
        assert_eq!(feasibility_score(0, 0, 0, 0, 0, false, true), 90);
        assert_eq!(feasibility_score(0, 0, 0, 0, 0, false, false), 100);
        // Score floors at 0.
        assert_eq!(feasibility_score(10, 30, 30, 100, 100, false, true), 0);
    }

    #[test]
    fn verdict_thresholds() {
        assert_eq!(verdict(100), "Good fit — mostly automatic");
        assert_eq!(verdict(80), "Good fit — mostly automatic");
        assert_eq!(verdict(79), "Feasible with assisted steps");
        assert_eq!(verdict(50), "Feasible with assisted steps");
        assert_eq!(verdict(49), "Manual effort needed — see MANUAL items");
        assert_eq!(verdict(0), "Manual effort needed — see MANUAL items");
    }

    #[test]
    fn menu_static_literal_is_detected() {
        let patterns = Patterns::new();
        let content = "const m = Menu.buildFromTemplate([\n  { label: 'Quit', role: 'quit' },\n]);";
        let menu = extract_menu(&patterns, content, "main.js").expect("menu found");
        assert!(menu.is_static_literal);
        assert!(menu.template_source.as_deref().unwrap().contains("Quit"));
        assert_eq!(menu.line, 1);
    }

    #[test]
    fn menu_variable_template_is_not_static() {
        let patterns = Patterns::new();
        let content = "Menu.setApplicationMenu(Menu.buildFromTemplate(appMenuTemplate));";
        let menu = extract_menu(&patterns, content, "main.js").expect("menu found");
        assert!(!menu.is_static_literal);
        assert_eq!(menu.template_source, None);
    }

    #[test]
    fn balanced_extraction_skips_strings() {
        let text = "f({ label: 'a}b', x: 1 })";
        let (inner, _) = extract_balanced(text, 2, '{', '}').expect("balanced");
        assert!(inner.contains("'a}b'"));
        assert!(inner.contains("x: 1"));
    }

    #[test]
    fn preload_path_from_path_join_is_found() {
        let patterns = Patterns::new();
        let options = "{\n  width: 800,\n  webPreferences: {\n    preload: path.join(__dirname, 'preload.js'),\n  },\n}";
        let cap = patterns
            .preload_path
            .captures(options)
            .expect("preload path matched");
        assert_eq!(&cap[1], "preload.js");
    }

    #[test]
    fn comments_are_stripped_but_strings_and_lines_survive() {
        let content = "const a = 1; // a comment ending in dialog.\n\
             /* block\n\
             comment */ const b = 'http://localhost:4000';\n\
             const c = \"not // a comment\";\n";
        let stripped = strip_comments(content);
        assert!(!stripped.contains("dialog."));
        assert!(!stripped.contains("block"));
        assert!(!stripped.contains("comment */"));
        // String literals survive (URLs, channel names live in them).
        assert!(stripped.contains("'http://localhost:4000'"));
        assert!(stripped.contains("\"not // a comment\""));
        // Line numbers survive: same newline count, same line count.
        assert_eq!(
            stripped.lines().count(),
            content.lines().count(),
            "stripping must preserve line numbers"
        );
        assert_eq!(
            stripped.matches('\n').count(),
            content.matches('\n').count()
        );
    }

    #[test]
    fn relative_paths_resolve_against_the_referencing_file() {
        assert_eq!(
            resolve_relative("desktop/app.js", "./preload.js"),
            "desktop/preload.js"
        );
        assert_eq!(
            resolve_relative("desktop/app.js", "preload.js"),
            "desktop/preload.js"
        );
        assert_eq!(
            resolve_relative("desktop/nested/app.js", "../preload.js"),
            "desktop/preload.js"
        );
        assert_eq!(resolve_relative("main.js", "./preload.js"), "preload.js");
        assert_eq!(
            resolve_relative("desktop/app.js", "assets/icon.png"),
            "desktop/assets/icon.png"
        );
    }

    #[test]
    fn preload_shape_from_inline_literal() {
        let content = "contextBridge.exposeInMainWorld('versions', {\n  node: () => process.versions.node,\n  chrome: () => process.versions.chrome,\n});";
        let patterns = Patterns::new();
        let m = patterns
            .expose_call
            .find(content)
            .expect("expose call found");
        let shape = extract_preload_shape(content, "preload.js", "versions", m.end() - 1)
            .expect("shape recovered");
        assert_eq!(shape.key, "versions");
        assert_eq!(shape.members.len(), 2);
        assert_eq!(shape.members[0].name, "node");
        assert_eq!(shape.members[0].kind, PreloadMemberKind::Function);
        assert_eq!(shape.members[1].name, "chrome");
    }

    #[test]
    fn preload_shape_from_variable_with_platform_checks() {
        // Simplenote's preload shape: a variable holding the object, with
        // functions, generic IPC plumbing, and platform booleans.
        let content = "const electronAPI = {\n  confirmLogout: (changes) => { return 'logout'; },\n  send: (channel, data) => { ipcRenderer.send(channel, data); },\n  receive: (channel, callback) => { ipcRenderer.on(channel, callback); },\n  isMac: process.platform === 'darwin',\n  isLinux: process.platform === 'linux',\n};\ncontextBridge.exposeInMainWorld('electron', electronAPI);";
        let patterns = Patterns::new();
        let m = patterns
            .expose_call
            .find(content)
            .expect("expose call found");
        let shape = extract_preload_shape(content, "desktop/preload.js", "electron", m.end() - 1)
            .expect("shape recovered");
        assert_eq!(shape.members.len(), 5);
        let kinds: Vec<(String, PreloadMemberKind)> = shape
            .members
            .iter()
            .map(|m| (m.name.clone(), m.kind))
            .collect();
        assert!(kinds.contains(&("confirmLogout".to_string(), PreloadMemberKind::Function)));
        assert!(kinds.contains(&("send".to_string(), PreloadMemberKind::Function)));
        assert!(kinds.contains(&("isMac".to_string(), PreloadMemberKind::PlatformMac)));
        assert!(kinds.contains(&("isLinux".to_string(), PreloadMemberKind::PlatformLinux)));
    }

    #[test]
    fn preload_shape_is_none_for_unresolvable_expressions() {
        let content = "contextBridge.exposeInMainWorld('api', buildApi(someArg));";
        let patterns = Patterns::new();
        let m = patterns
            .expose_call
            .find(content)
            .expect("expose call found");
        assert!(extract_preload_shape(content, "preload.js", "api", m.end() - 1).is_none());
    }

    #[test]
    fn dev_url_candidates_cover_literal_and_host_forms() {
        let patterns = Patterns::new();
        let literal = "mainWindow.loadURL('http://localhost:4000');";
        let found = find_dev_url_candidates(&patterns, literal, "desktop/app.js");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0.url, "http://localhost:4000");
        // `loadURL` on the URL's own line scores +30; `desktop/app.js`
        // earns no main-area bonus (no `main`/`browser` segment), so
        // the score is exactly 30 — still well above 0.
        assert!(
            found[0].1 >= 30,
            "loadURL line must rank high: {}",
            found[0].1
        );

        let host = "indexPath = url.format({\n  protocol: 'http:',\n  host: 'localhost:8080',\n});";
        let found = find_dev_url_candidates(&patterns, host, "main.js");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0.url, "http://localhost:8080");
        assert_eq!(found[0].0.line, 3);
    }

    #[test]
    fn dev_url_candidates_rank_the_apps_own_server_first() {
        let patterns = Patterns::new();
        // An extension service URL (Copilot, `localhost:4443`) must
        // lose to the app's own dev server loaded in main-process code.
        let copilot = "const serviceUrl = 'http://localhost:4443/copilot';";
        let found = find_dev_url_candidates(&patterns, copilot, "extensions/copilot/service.js");
        assert_eq!(found.len(), 1);
        assert!(
            found[0].1 <= 0,
            "extension URL must not win: score {}",
            found[0].1
        );
        let own = "mainWindow.loadURL('http://localhost:5173');";
        let found = find_dev_url_candidates(&patterns, own, "src/main.ts");
        assert_eq!(found.len(), 1);
        assert!(
            found[0].1 >= DEV_URL_MIN_SCORE,
            "own dev server must clear the gate: {}",
            found[0].1
        );
        // A CLI docs server in an `app-cli` package must lose too.
        let cli = "const url = 'http://localhost:41184/docs';";
        let found = find_dev_url_candidates(&patterns, cli, "packages/app-cli/server.js");
        assert_eq!(found.len(), 1);
        assert!(found[0].1 <= 0, "CLI URL must not win: {}", found[0].1);
        // An OAuth loopback port in a browser-pathed file scores only
        // the +5 path bonus (no loadURL anywhere in the file) — below
        // the gate, so an app with no dev server reports none rather
        // than its auth redirect URI (VS Code's 127.0.0.1:33418).
        let oauth = "const redirectUrls = 'http://127.0.0.1:33418\\nhttps://vscode.dev/redirect';";
        let found = find_dev_url_candidates(
            &patterns,
            oauth,
            "src/vs/workbench/api/browser/mainThreadAuthentication.ts",
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, 5, "OAuth URL must score only the path bonus");
        assert!(
            found[0].1 < DEV_URL_MIN_SCORE,
            "OAuth URL must not clear the gate"
        );
    }

    // ---- Directory-skipping tests (need a real directory tree) ----

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A fresh empty directory under the system temp dir, unique per call.
    /// Test fixtures here are tiny text files, so the 512 MB tmpfs is fine.
    /// Callers remove it when done.
    fn fresh_test_dir(tag: &str) -> PathBuf {
        let n = TEST_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("e2r-scan-test-{tag}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn write_test_file(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create test parent dir");
        }
        fs::write(&path, contents).expect("write test file");
    }

    #[test]
    fn build_output_dirs_are_not_scanned() {
        let app = fresh_test_dir("build-skip");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app } = require('electron');\napp.whenReady().then(() => {});\n",
        );
        // Bundled output in build/ (Create React App) and dist/ (Vite):
        // generated code that must not produce findings, windows, or IPC
        // channels of its own.
        write_test_file(
            &app,
            "build/bundle.js",
            "const { app, BrowserWindow, dialog } = require('electron');\nnew BrowserWindow({ width: 1, height: 1 });\ndialog.showOpenDialog();\n",
        );
        write_test_file(
            &app,
            "dist/bundle.js",
            "const { app, BrowserWindow } = require('electron');\nnew BrowserWindow({ width: 2, height: 2 });\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(
            report.files_scanned, 1,
            "only main.js should be scanned; build/ and dist/ are skipped"
        );
        assert!(
            report
                .findings
                .iter()
                .all(|f| !f.file.starts_with("build/") && !f.file.starts_with("dist/")),
            "no finding may come from a build-output dir; findings: {:?}",
            report.findings
        );
        assert!(
            report.windows.is_empty(),
            "bundled BrowserWindows must not be counted; windows: {:?}",
            report.windows
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn comment_false_positive_is_not_a_finding() {
        let app = fresh_test_dir("comment-fp");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        // The Simplenote shape: a comment ending in "dialog." must not
        // produce a `dialog` finding; the real `shell.` call still does.
        write_test_file(
            &app,
            "main.js",
            "const { app, shell } = require('electron');\n\
             // Will check and display a progress dialog.\n\
             /* Another note about the app. in a block comment. */\n\
             shell.openExternal('https://example.com');\n",
        );
        let report = scan(&app).expect("scan");
        assert!(
            report.findings.iter().all(|f| f.api != "dialog"),
            "comment must not produce a dialog finding; findings: {:?}",
            report.findings
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.api == "shell.openExternal"),
            "the real shell call must still be found; findings: {:?}",
            report.findings
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn electron_updater_alias_is_not_electrons_autoupdater() {
        let app = fresh_test_dir("dead-updater");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        // Electro-dash shape: a renderer file imports autoUpdater and never
        // uses it; the main file really calls it — but through
        // `electron-updater`, so the finding must be `electron-updater`
        // (AUTO, via the electron-updater mapping), not Electron's
        // `autoUpdater`.
        write_test_file(
            &app,
            "renderer.js",
            "import { autoUpdater } from \"electron\";\nconsole.log('renderer');\n",
        );
        write_test_file(
            &app,
            "main.js",
            "const { app } = require('electron');\n\
             const { autoUpdater } = require('electron-updater');\n\
             autoUpdater.checkForUpdates();\n",
        );
        let report = scan(&app).expect("scan");
        assert!(
            report.findings.iter().all(|f| f.api != "autoUpdater"),
            "the aliased updater must not file as Electron's autoUpdater; findings: {:?}",
            report.findings
        );
        let updater: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.api == "electron-updater")
            .collect();
        assert_eq!(
            updater.len(),
            1,
            "only the real call site counts; findings: {:?}",
            report.findings
        );
        assert_eq!(updater[0].file, "main.js");
        assert_eq!(
            updater[0].line, 3,
            "finding must point at the call, not the import"
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn node_integration_true_is_a_manual_finding() {
        let app = fresh_test_dir("node-integration");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             new BrowserWindow({ width: 800, height: 600, webPreferences: { nodeIntegration: true, contextIsolation: false } });\n",
        );
        let report = scan(&app).expect("scan");
        let node = report
            .findings
            .iter()
            .find(|f| f.api == "nodeIntegration")
            .expect("nodeIntegration finding");
        assert_eq!(node.classification, Classification::Manual);
        let ctx = report
            .findings
            .iter()
            .find(|f| f.api == "contextIsolation")
            .expect("contextIsolation finding");
        assert_eq!(ctx.classification, Classification::Assisted);
        assert!(
            report.feasibility_score < 100,
            "a nodeIntegration app must not score 100; score: {}",
            report.feasibility_score
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn pure_js_port_deps_are_not_native_modules() {
        let app = fresh_test_dir("port-deps");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0","dependencies":{"electron-updater":"^6.0.0","electron-store":"^8.0.0","node-pty":"^1.0.0"}}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app } = require('electron');\napp.whenReady().then(() => {});\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(report.native_modules, vec!["node-pty"]);
        assert_eq!(
            report.port_needing_deps,
            vec!["electron-store", "electron-updater"]
        );
        // Only the true native module costs points (10 under the v3
        // formula); the two pure-JS deps cost nothing — and they are
        // no longer findings at all (N9), so no `package.json:1`
        // finding may appear.
        assert_eq!(report.feasibility_score, 90);
        assert!(
            report.findings.iter().all(|f| f.file != "package.json"),
            "dependency declarations must not be findings; findings: {:?}",
            report.findings
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn preload_reference_dedups_with_filename_match() {
        let app = fresh_test_dir("preload-dedup");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"desktop/index.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "desktop/index.js",
            "const { app, BrowserWindow } = require('electron');\n\
             new BrowserWindow({ width: 800, height: 600, webPreferences: { preload: path.join(__dirname, './preload.js') } });\n",
        );
        write_test_file(&app, "desktop/preload.js", "console.log('preload');\n");
        let report = scan(&app).expect("scan");
        assert_eq!(
            report.preload.files,
            vec!["desktop/preload.js"],
            "the ./preload.js reference and the filename match are one file"
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn app_identity_is_captured_from_package_and_builder_config() {
        let app = fresh_test_dir("identity");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"simplenote","productName":"Simplenote","version":"2.27.1","main":"desktop/index.js","build":{"appId":"com.example.app"}}"#,
        );
        write_test_file(
            &app,
            "desktop/index.js",
            "const { app } = require('electron');\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(report.app_version.as_deref(), Some("2.27.1"));
        assert_eq!(report.product_name.as_deref(), Some("Simplenote"));
        assert_eq!(report.app_id.as_deref(), Some("com.example.app"));

        // Without a build.appId, electron-builder.json is the fallback.
        let app2 = fresh_test_dir("identity-builder");
        write_test_file(
            &app2,
            "package.json",
            r#"{"name":"t","version":"1.0.0","main":"main.js"}"#,
        );
        write_test_file(
            &app2,
            "electron-builder.json",
            r#"{"productName":"Built App","appId":"com.builder.app"}"#,
        );
        write_test_file(&app2, "main.js", "const { app } = require('electron');\n");
        let report2 = scan(&app2).expect("scan");
        assert_eq!(report2.product_name.as_deref(), Some("Built App"));
        assert_eq!(report2.app_id.as_deref(), Some("com.builder.app"));
        fs::remove_dir_all(&app).ok();
        fs::remove_dir_all(&app2).ok();
    }

    #[test]
    fn ecosystem_package_import_opens_the_gate() {
        let app = fresh_test_dir("gate-ecosystem");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        // A file importing only electron-updater (never electron) must
        // still get its updater call sites flagged — as
        // `electron-updater`, not Electron's `autoUpdater` (N6 alias
        // rule: the two updaters are different libraries).
        write_test_file(
            &app,
            "updater.js",
            "const { autoUpdater } = require('electron-updater');\n\
             autoUpdater.checkForUpdates();\n\
             autoUpdater.on('update-downloaded', () => {});\n",
        );
        write_test_file(&app, "main.js", "const { app } = require('electron');\n");
        let report = scan(&app).expect("scan");
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.api == "electron-updater" && f.file == "updater.js"),
            "updater.js must produce an electron-updater finding; findings: {:?}",
            report.findings
        );
        assert!(
            report.findings.iter().all(|f| f.api != "autoUpdater"),
            "no finding may be filed as Electron's autoUpdater; findings: {:?}",
            report.findings
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn finding_occurrences_are_counted() {
        let app = fresh_test_dir("occurrences");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, shell } = require('electron');\n\
             shell.openExternal('https://a.example');\n\
             shell.openExternal('https://b.example');\n\
             shell.openExternal('https://c.example');\n",
        );
        let report = scan(&app).expect("scan");
        let found: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.api == "shell.openExternal")
            .collect();
        assert_eq!(found.len(), 1, "still deduped to one displayed finding");
        assert_eq!(found[0].occurrences, 3);
        assert_eq!(found[0].line, 2, "first occurrence's line is kept");
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn dev_url_is_detected_from_the_main_process() {
        let app = fresh_test_dir("dev-url");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             const url = isDev ? 'http://localhost:4000' : 'file://' + path.join(__dirname, 'dist/index.html');\n\
             mainWindow.loadURL(url);\n",
        );
        let report = scan(&app).expect("scan");
        let dev = report.dev_url.expect("dev URL detected");
        assert_eq!(dev.url, "http://localhost:4000");
        assert_eq!(dev.file, "main.js");
        assert_eq!(dev.line, 2);
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn single_instance_deep_link_and_remote_patterns_fire() {
        let app = fresh_test_dir("b13-patterns");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             const gotLock = app.requestSingleInstanceLock();\n\
             app.on('second-instance', () => {});\n\
             app.setAsDefaultProtocolClient('myapp');\n\
             app.on('open-url', (e, url) => {});\n",
        );
        write_test_file(
            &app,
            "preload.js",
            "const { contextBridge, remote } = require('electron');\n\
             remote.dialog.showMessageBoxSync({});\n",
        );
        let report = scan(&app).expect("scan");
        for api in [
            "app.requestSingleInstanceLock",
            "app.setAsDefaultProtocolClient",
            "remote",
        ] {
            assert!(
                report.findings.iter().any(|f| f.api == api),
                "missing finding for {api}; findings: {:?}",
                report.findings
            );
        }
        let remote = report
            .findings
            .iter()
            .find(|f| f.api == "remote")
            .expect("remote finding");
        assert_eq!(remote.classification, Classification::Manual);
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn ipc_wrappers_are_counted_and_definitions_skipped() {
        // N6 (Insomnia): `ipcMainHandle` / `ipcMainOn` are one-line
        // arrow wrappers around `ipcMain.handle` / `ipcMain.on` — the
        // 212 registrations through them were invisible, while the
        // `ipcMain.*` calls inside the definitions were counted once
        // each as dynamic channels.
        let app = fresh_test_dir("ipc-wrappers");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "electron.ts",
            "import { ipcMain } from 'electron';\n\
             export const ipcMainHandle = (\n\
             \x20 channel: string,\n\
             \x20 listener: (...args: any[]) => void,\n\
             ) => ipcMain.handle(channel, listener);\n\
             export const ipcMainOn = (channel: string, listener: () => void) => ipcMain.on(channel, listener);\n",
        );
        write_test_file(
            &app,
            "handlers.ts",
            "import { ipcMainHandle, ipcMainOn } from './electron';\n\
             ipcMainHandle('app:get-info', async () => ({}));\n\
             ipcMainHandle(dynamicChannel, () => {});\n\
             ipcMainOn('app:quit', () => {});\n",
        );
        write_test_file(&app, "main.js", "console.log('main');\n");
        let report = scan(&app).expect("scan");
        let names: Vec<&str> = report
            .ipc_channels
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert!(
            names.contains(&"app:get-info"),
            "wrapper call with a literal channel must be recorded; channels: {:?}",
            report.ipc_channels
        );
        assert!(
            names.contains(&"dynamic"),
            "wrapper call with a variable channel must be recorded as dynamic; channels: {:?}",
            report.ipc_channels
        );
        assert!(
            names.contains(&"app:quit"),
            "ipcMainOn wrapper call must be recorded; channels: {:?}",
            report.ipc_channels
        );
        assert_eq!(
            report.ipc_channels.len(),
            3,
            "the ipcMain.* calls inside the wrapper definitions must not be counted; channels: {:?}",
            report.ipc_channels
        );
        assert!(
            report
                .findings
                .iter()
                .filter(|f| f.api == "ipcMain.handle")
                .all(|f| f.file == "handlers.ts"),
            "no ipcMain.handle finding may come from the definition file; findings: {:?}",
            report.findings
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn ipc_wrapper_scan_survives_multibyte_chars() {
        // Regression (fix round 2, N6): `find_ipc_wrappers` sliced
        // `content[tail_start..tail_start + 600]`; when byte 600 landed
        // inside a multi-byte character the slice panicked and the whole
        // scan died — Joplin's sources contain 'Ř' at exactly such a
        // spot, so the fixed binary could not scan Joplin at all. The
        // end is now snapped down to a char boundary, and the wrapper
        // in front of it is still found.
        let def_and_call = "export const ipcMainHandle = (channel, listener) => ipcMain.handle(channel, listener);\n";
        let tail_start = def_and_call.find("(channel,").unwrap() + "(channel,".len();
        // Place a 2-byte 'Ř' so byte tail_start+600 lands inside it.
        let pad = 600usize - 1 - (def_and_call.len() - tail_start);
        let mut content = String::from(def_and_call);
        content.push_str(&"x".repeat(pad));
        content.push('Ř');
        content.push_str("yyyyyyyyyy");
        assert_eq!(content.len(), tail_start + 600 + 1 + 10);
        let (wrappers, _) = find_ipc_wrappers(&content);
        assert!(
            wrappers.contains_key("ipcMainHandle"),
            "the wrapper before the multi-byte char must still be found"
        );
    }

    #[test]
    fn webview_tag_and_webview_tag_preference_are_manual() {
        // N6: the `<webview>` tag and the `webviewTag` webPreference
        // have no Tauri equivalent — they are MANUAL findings, found
        // even in renderer files that never import Electron.
        let app = fresh_test_dir("webview-tag");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             new BrowserWindow({ width: 800, height: 600, webPreferences: { webviewTag: true } });\n",
        );
        write_test_file(
            &app,
            "response-web-view.tsx",
            "export function ResponseWebView({ url }: { url: string }) {\n\
             \x20 return <webview src={url} partition=\"persist:webview\" />;\n\
             }\n",
        );
        let report = scan(&app).expect("scan");
        let webviews: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.api == "webview")
            .collect();
        assert_eq!(
            webviews.len(),
            2,
            "both the tag and the webPreference must be found; findings: {:?}",
            report.findings
        );
        assert!(
            webviews
                .iter()
                .all(|f| f.classification == Classification::Manual),
            "webview has no Tauri equivalent; findings: {:?}",
            webviews
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn window_size_constants_are_resolved() {
        // N6: `width: MAIN_WIDTH` with `const MAIN_WIDTH = 1280` at
        // the top of the file is a known size, not a dynamic one.
        let app = fresh_test_dir("window-consts");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             const MAIN_WIDTH = 1280;\n\
             const MAIN_HEIGHT = 800;\n\
             new BrowserWindow({ width: MAIN_WIDTH, height: MAIN_HEIGHT, title: 'App' });\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(report.windows.len(), 1, "one window; {:?}", report.windows);
        let w = &report.windows[0];
        assert_eq!(w.width, 1280);
        assert_eq!(w.height, 800);
        assert!(w.width_known && w.height_known, "const sizes are known");
        assert!(!w.from_window_state, "not an electron-window-state default");
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn remote_mediated_findings_are_flagged() {
        // N8 (Mailspring): a `safeStorage` call through
        // `@electron/remote` and a `webContents.send` through a
        // remote-derived window are MANUAL *because of remote* — the
        // matrix must be able to split them from findings that are
        // MANUAL on their own merits. A direct `webContents.send` is
        // not flagged.
        let app = fresh_test_dir("remote-mediated");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "key-manager.ts",
            "const { safeStorage } = require('@electron/remote');\n\
             safeStorage.encryptString('secret');\n",
        );
        write_test_file(
            &app,
            "window-event-handler.ts",
            "const { getCurrentWindow } = require('@electron/remote');\n\
             const win = getCurrentWindow();\n\
             win.webContents.send('app-event', { x: 1 });\n",
        );
        write_test_file(
            &app,
            "direct.js",
            "const { BrowserWindow } = require('electron');\n\
             const win = new BrowserWindow({ width: 800, height: 600 });\n\
             win.webContents.send('direct-event', {});\n",
        );
        write_test_file(&app, "main.js", "console.log('main');\n");
        let report = scan(&app).expect("scan");
        let safe = report
            .findings
            .iter()
            .find(|f| f.api == "safeStorage")
            .expect("safeStorage finding");
        assert_eq!(safe.classification, Classification::Manual);
        assert!(
            safe.remote_mediated,
            "remote safeStorage must be flagged remote-mediated"
        );
        let sends: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.api == "webContents.send")
            .collect();
        assert_eq!(sends.len(), 2, "both sends must be found; {:?}", sends);
        let remote_send = sends
            .iter()
            .find(|f| f.file == "window-event-handler.ts")
            .expect("remote send");
        assert_eq!(remote_send.classification, Classification::Manual);
        assert!(
            remote_send.remote_mediated,
            "remote-derived webContents.send must be flagged"
        );
        let direct_send = sends
            .iter()
            .find(|f| f.file == "direct.js")
            .expect("direct send");
        assert!(
            !direct_send.remote_mediated,
            "a direct webContents.send must not be flagged"
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn typed_session_parameter_counts_as_session() {
        // Signal / Joplin: a `session: Session` parameter is an
        // Electron session passed in — `session.setProxy` on it is a
        // real `session` finding even though no binding names it.
        let app = fresh_test_dir("typed-session");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "handle-custom-protocols.ts",
            "import { net, Session } from 'electron';\n\
             export function handleCustomProtocols(session: Session) {\n\
             \x20 session.setProxy({ proxyRules: 'http://proxy:8080' });\n\
             }\n",
        );
        write_test_file(&app, "main.js", "console.log('main');\n");
        let report = scan(&app).expect("scan");
        let session = report
            .findings
            .iter()
            .find(|f| f.api == "session")
            .expect("session finding");
        assert_eq!(session.file, "handle-custom-protocols.ts");
        assert_eq!(session.line, 3, "finding must point at the setProxy call");
        assert_eq!(session.classification, Classification::Manual);
        fs::remove_dir_all(&app).ok();
    }

    // ---- Proof-campaign fix round: regression tests -------------------

    #[test]
    fn test_files_are_excluded_from_scan() {
        let app = fresh_test_dir("test-exclusion");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow, ipcMain } = require('electron');\n\
             new BrowserWindow({ width: 800, height: 600 });\n\
             ipcMain.handle('real-channel', () => {});\n\
             app.whenReady().then(() => {});\n",
        );
        // Every test-file shape the proof apps showed: Testing Library
        // helpers, spec files, fixtures, playwright e2e, `test-` files.
        // GitHub Desktop had 48.3% of occurrences in test files; VS Code
        // 52.2% of its IPC channels.
        write_test_file(
            &app,
            "__tests__/main.test.js",
            "const { app, BrowserWindow, ipcMain } = require('electron');\n\
             const { screen } = require('@testing-library/react');\n\
             new BrowserWindow({ width: 1, height: 1 });\n\
             ipcMain.handle('fake-channel', () => {});\n\
             app.quit();\n\
             screen.getByText('hello');\n",
        );
        write_test_file(
            &app,
            "tests/helper.spec.js",
            "const { dialog } = require('electron');\ndialog.showOpenDialog();\n",
        );
        write_test_file(
            &app,
            "fixtures/data.js",
            "const { shell } = require('electron');\nshell.openExternal('https://example.com');\n",
        );
        write_test_file(
            &app,
            "test-foo.js",
            "const { app } = require('electron');\napp.quit();\n",
        );
        write_test_file(
            &app,
            "spec/bar.js",
            "const { app } = require('electron');\napp.quit();\n",
        );
        write_test_file(
            &app,
            "playwright/e2e.spec.ts",
            "import { app } from 'electron';\napp.quit();\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(
            report.files_scanned, 1,
            "only main.js should be scanned; files from test dirs must not count"
        );
        assert!(
            report
                .findings
                .iter()
                .all(|f| !crate::scan::is_test_path(&f.file)),
            "no finding may come from a test file; findings: {:?}",
            report.findings
        );
        assert_eq!(
            report.windows.len(),
            1,
            "only the real window counts; windows: {:?}",
            report.windows
        );
        assert_eq!(
            report.ipc_channels.len(),
            1,
            "only the real channel counts; channels: {:?}",
            report.ipc_channels
        );
        assert_eq!(report.ipc_channels[0].name, "real-channel");
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn test_exclusion_covers_signal_shapes() {
        // N3: Signal's test windows live under `test-node/`, its test
        // main entry is `*_test.main.ts`, and `.vscode/` holds task
        // scripts — none of them may be scanned.
        let app = fresh_test_dir("test-exclusion-signal");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             new BrowserWindow({ width: 800, height: 600 });\n",
        );
        write_test_file(
            &app,
            "test-node/main.test.js",
            "const { BrowserWindow } = require('electron');\nnew BrowserWindow({ width: 1, height: 1 });\n",
        );
        write_test_file(
            &app,
            "ts/test-electron/test_test.main.ts",
            "const { BrowserWindow } = require('electron');\nnew BrowserWindow({ width: 1, height: 1 });\n",
        );
        write_test_file(
            &app,
            ".vscode/launch.js",
            "const { app } = require('electron');\napp.quit();\n",
        );
        write_test_file(
            &app,
            "insomnia-test/helper.js",
            "const { app } = require('electron');\napp.quit();\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(
            report.files_scanned, 1,
            "only main.js should be scanned; files: {:?}",
            report.files_scanned
        );
        assert_eq!(report.windows.len(), 1, "only the real window counts");
        assert!(
            report
                .findings
                .iter()
                .all(|f| !crate::scan::is_test_path(&f.file)),
            "no finding may come from a test file; findings: {:?}",
            report.findings
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn string_literal_and_same_name_false_positives_are_not_findings() {
        let app = fresh_test_dir("string-fp");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app } = require('electron');\n\
             const asarPath = 'app.asar';\n\
             const proto = require('./protocol.js');\n\
             const label = 'net.cozic.joplin';\n\
             const screenText = 'provider screen.';\n\
             const logPath = 'app.log';\n\
             app.quit();\n\
             const session = this.session;\n\
             window.reduxActions.app.quit();\n\
             navigator.clipboard.readText();\n\
             const remote = state.remote;\n\
             screen.getByText('hello');\n\
             document.querySelector('dialog');\n",
        );
        let report = scan(&app).expect("scan");
        assert!(
            report.findings.iter().any(|f| f.api == "app.quit"),
            "the real app.quit() call must still be found; findings: {:?}",
            report.findings
        );
        for fp in [
            "session",
            "clipboard",
            "remote",
            "screen",
            "dialog",
            "protocol",
        ] {
            assert!(
                report.findings.iter().all(|f| f.api != fp),
                "same-named local / string literal must not produce a {fp} finding; findings: {:?}",
                report.findings
            );
        }
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn variable_options_window_and_electron_prefix_are_detected() {
        let app = fresh_test_dir("var-window");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        // Every proof app's real main window passes its options as a
        // variable (and/or uses the `electron.BrowserWindow` prefix);
        // the old scanner required an inline `{` and found none of them.
        write_test_file(
            &app,
            "main.js",
            "const electron = require('electron');\n\
             const winOpts = { width: 1024, height: 768, title: 'Titled' };\n\
             const win = new electron.BrowserWindow(winOpts);\n\
             const win2 = new electron.BrowserWindow();\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(
            report.windows.len(),
            2,
            "both windows must be recorded; windows: {:?}",
            report.windows
        );
        let sized = &report.windows[0];
        assert_eq!(sized.width, 1024);
        assert_eq!(sized.height, 768);
        assert!(sized.width_known && sized.height_known);
        assert_eq!(sized.title.as_deref(), Some("Titled"));
        // No-arg construction still records a window — with unknown size,
        // not a confident 800x600.
        let no_size = &report.windows[1];
        assert!(!no_size.width_known && !no_size.height_known);
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn monorepo_package_json_picks_app_package_and_native_modules() {
        let app = fresh_test_dir("monorepo-pkg");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"root","version":"0.0.1","main":"index.js"}"#,
        );
        write_test_file(&app, "index.js", "console.log('root');\n");
        write_test_file(
            &app,
            "packages/app/package.json",
            r#"{"name":"myapp","version":"2.3.4","main":"src/main.js","dependencies":{"electron":"^30.0.0","keytar":"^7.0.0","sqlite3":"^5.0.0","react":"^18.0.0"}}"#,
        );
        write_test_file(
            &app,
            "packages/app/src/main.js",
            "const { app } = require('electron');\napp.whenReady().then(() => {});\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(report.app_name, "myapp");
        assert_eq!(report.app_version.as_deref(), Some("2.3.4"));
        assert!(
            report
                .electron_version
                .as_deref()
                .is_some_and(|v| v.contains("30")),
            "Electron version must come from the app package; got {:?}",
            report.electron_version
        );
        assert_eq!(
            report.main_entry.as_deref(),
            Some("packages/app/src/main.js")
        );
        assert!(
            report.native_modules.contains(&"keytar".to_string()),
            "keytar must be detected as a native module; got {:?}",
            report.native_modules
        );
        assert!(
            report.native_modules.contains(&"sqlite3".to_string()),
            "sqlite3 must be detected as a native module; got {:?}",
            report.native_modules
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn package_picker_prefers_root_over_extension_packages() {
        // N2 (VS Code): the nested-package picker used to choose
        // `extensions/copilot/package.json` (copilot-chat 0.77.0,
        // Electron ^42) over the root (code-oss-dev 1.149.0, Electron
        // 43.8.10 from `nativeBuild.target`). The root must win, and
        // the Electron version must come from the root's
        // `nativeBuild.target`, not an extension's dependency.
        let app = fresh_test_dir("picker-root");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"code-oss-dev","version":"1.149.0","main":"./out/main.js","nativeBuild":{"target":"43.8.10"}}"#,
        );
        write_test_file(
            &app,
            "extensions/copilot/package.json",
            r#"{"name":"copilot-chat","version":"0.77.0","main":"./dist/extension.js","dependencies":{"electron":"^42.10.0"}}"#,
        );
        write_test_file(&app, "out/main.js", "console.log('main');\n");
        write_test_file(
            &app,
            "extensions/copilot/dist/extension.js",
            "console.log('copilot');\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(report.app_name, "code-oss-dev");
        assert_eq!(report.app_version.as_deref(), Some("1.149.0"));
        assert_eq!(
            report.main_entry.as_deref(),
            Some("out/main.js"),
            "the root's main entry must win (leading ./ normalized)"
        );
        assert!(
            report
                .electron_version
                .as_deref()
                .is_some_and(|v| v.contains("43.8.10")),
            "Electron version must come from the root nativeBuild.target; got {:?}",
            report.electron_version
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn libcurl_and_parcel_watcher_are_native_modules() {
        // N5: Insomnia's `@getinsomnia/node-libcurl` and VS Code's
        // `@parcel/watcher` were missed by the native-module list.
        let app = fresh_test_dir("native-libcurl");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0","dependencies":{"@getinsomnia/node-libcurl":"^3.0.0","@parcel/watcher":"^2.0.0","react":"^18.0.0"}}"#,
        );
        write_test_file(&app, "main.js", "console.log('main');\n");
        let report = scan(&app).expect("scan");
        assert!(
            report
                .native_modules
                .contains(&"@getinsomnia/node-libcurl".to_string()),
            "node-libcurl must be a native module; got {:?}",
            report.native_modules
        );
        assert!(
            report
                .native_modules
                .contains(&"@parcel/watcher".to_string()),
            "@parcel/watcher must be a native module; got {:?}",
            report.native_modules
        );
        assert!(
            !report.native_modules.contains(&"react".to_string()),
            "pure-JS deps must not be native modules"
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn menu_collection_reports_app_menu_vs_context_menu() {
        let app = fresh_test_dir("menu-collection");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(&app, "main.js", "const { app } = require('electron');\n");
        write_test_file(
            &app,
            "app-menu.js",
            "const { Menu } = require('electron');\n\
             const template = [{ label: 'File', submenu: [{ label: 'Quit' }] }];\n\
             Menu.setApplicationMenu(Menu.buildFromTemplate(template));\n",
        );
        write_test_file(
            &app,
            "tray.js",
            "const { Menu, Tray } = require('electron');\n\
             const trayTemplate = [{ label: 'Show' }];\n\
             const trayMenu = Menu.buildFromTemplate(trayTemplate);\n\
             tray.setContextMenu(trayMenu);\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(
            report.menus.len(),
            2,
            "all buildFromTemplate sites must be collected; menus: {:?}",
            report.menus
        );
        let app_menu = report
            .menus
            .iter()
            .find(|m| m.file == "app-menu.js")
            .expect("app menu site");
        let tray_menu = report
            .menus
            .iter()
            .find(|m| m.file == "tray.js")
            .expect("tray menu site");
        assert!(
            app_menu.is_application_menu,
            "the setApplicationMenu site is the app menu"
        );
        assert!(
            !tray_menu.is_application_menu,
            "the tray/context site is not the app menu"
        );
        assert_eq!(
            report.menu.as_ref().map(|m| m.file.as_str()),
            Some("app-menu.js"),
            "the chosen menu must be the app menu, not the first match"
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn set_as_default_protocol_client_ignores_open_url_listener_and_definitions() {
        let app = fresh_test_dir("protocol-client");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app } = require('electron');\n\
             app.on('open-url', (e, url) => {});\n\
             function setAsDefaultProtocolClient(protocol) { return register(protocol); }\n",
        );
        let report = scan(&app).expect("scan");
        assert!(
            report
                .findings
                .iter()
                .all(|f| f.api != "app.setAsDefaultProtocolClient"),
            "an 'open-url' listener and a local wrapper definition are not registration; findings: {:?}",
            report.findings
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn new_api_patterns_fire_with_expected_classifications() {
        let app = fresh_test_dir("new-patterns");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow, WebContentsView, MessageChannelMain, utilityProcess, desktopCapturer, systemPreferences, webFrame, webUtils, contentTracing, ipcMain, Menu, MenuItem, ipcRenderer } = require('electron');\n\
             const { net } = require('electron');\n\
             const view = new WebContentsView();\n\
             const child = utilityProcess.fork('./child.js');\n\
             const { port1 } = new MessageChannelMain();\n\
             desktopCapturer.getSources({ types: ['screen'] });\n\
             systemPreferences.getMediaAccessStatus('camera');\n\
             webFrame.setZoomFactor(1.5);\n\
             webUtils.getPathForFile(file);\n\
             contentTracing.startRecording({});\n\
             ipcMain.handleOnce('once', () => {});\n\
             const item = new MenuItem({ label: 'Hi' });\n\
             Menu.setApplicationMenu(Menu.buildFromTemplate([]));\n\
             app.commandLine.appendSwitch('a', 'b');\n\
             app.dock.hide();\n\
             app.setBadgeCount(3);\n\
             net.resolveHost('example.com');\n\
             net.isOnline();\n\
             const { getGlobal, getCurrentWindow, getCurrentWebContents } = require('@electron/remote');\n\
             getGlobal('shared');\n\
             getCurrentWindow().focus();\n\
             getCurrentWebContents().send('x');\n\
             const { app: electronApp } = require('electron');\n\
             electronApp.quit();\n\
             require('@electron/remote').dialog.showMessageBoxSync({});\n",
        );
        let report = scan(&app).expect("scan");
        for api in [
            "WebContentsView",
            "utilityProcess",
            "MessageChannelMain",
            "desktopCapturer",
            "systemPreferences",
            "webFrame",
            "webUtils",
            "contentTracing",
            "MenuItem",
            "Menu.setApplicationMenu",
            "app.commandLine",
            "app.dock",
            "app.setBadgeCount",
            "net.resolveHost",
            "net.isOnline",
            "getGlobal",
            "getCurrentWindow",
            "getCurrentWebContents",
            "app.quit",
            "remote",
            "dialog",
        ] {
            assert!(
                report.findings.iter().any(|f| f.api == api),
                "missing finding for {api}; findings: {:?}",
                report.findings
            );
        }
        // Member-level app.* is MANUAL; net.* is ASSISTED (not reqwest);
        // webFrame is ASSISTED; the remote-mediated dialog is MANUAL.
        let class_of = |api: &str| {
            report
                .findings
                .iter()
                .find(|f| f.api == api)
                .expect("finding")
                .classification
        };
        assert_eq!(class_of("app.commandLine"), Classification::Manual);
        assert_eq!(class_of("app.setBadgeCount"), Classification::Manual);
        assert_eq!(class_of("net.resolveHost"), Classification::Assisted);
        assert_eq!(class_of("net.isOnline"), Classification::Assisted);
        assert_eq!(class_of("webFrame"), Classification::Assisted);
        assert_eq!(class_of("WebContentsView"), Classification::Manual);
        assert_eq!(class_of("dialog"), Classification::Manual);
        assert_eq!(class_of("remote"), Classification::Manual);
        // ipcMain.handleOnce is a real channel with the handle direction.
        assert!(
            report
                .ipc_channels
                .iter()
                .any(|c| c.name == "once" && c.direction == "handle"),
            "handleOnce channel must be recorded; channels: {:?}",
            report.ipc_channels
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn ipc_renderer_invoke_variable_channel_is_dynamic() {
        let patterns = Patterns::new();
        let content = "ipcRenderer.invoke(channelName, data);\n\
             ipcRenderer.invoke('static', data);\n\
             ipcMain.handleOnce(dynamicName, () => {});\n";
        let channels =
            collect_ipc_channels(&patterns, content, "renderer.js", &[], &HashSet::new());
        assert_eq!(channels.len(), 3);
        assert_eq!(channels[0].0.name, "dynamic");
        assert_eq!(channels[0].1, "ipcRenderer.invoke");
        assert_eq!(channels[1].0.name, "static");
        assert_eq!(channels[2].0.name, "dynamic");
        assert_eq!(channels[2].1, "ipcMain.handleOnce");
    }

    #[test]
    fn preload_suffix_set_preload_scripts_and_variable_paths_are_detected() {
        let app = fresh_test_dir("preload-var");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             const path = require('path');\n\
             const preloadPath = path.join(__dirname, 'bridge.js');\n\
             const win = new BrowserWindow({ width: 800, height: 600, webPreferences: { preload: preloadPath } });\n\
             win.webContents.session.setPreloadScripts([{ id: 'x', type: 'frame', filePath: './print-preload.js' }]);\n",
        );
        // `bridge.js` has no "preload" in its name: only the variable
        // preload path can find it. `print-preload.js` exercises the
        // `*Preload.js` suffix + setPreloadScripts registration.
        write_test_file(
            &app,
            "bridge.js",
            "const { contextBridge } = require('electron');\ncontextBridge.exposeInMainWorld('api', {});\n",
        );
        write_test_file(&app, "print-preload.js", "console.log('print');\n");
        write_test_file(&app, "preload.js", "console.log('plain');\n");
        let report = scan(&app).expect("scan");
        for file in ["bridge.js", "print-preload.js", "preload.js"] {
            assert!(
                report.preload.files.iter().any(|f| f == file),
                "preload file {file} must be detected; files: {:?}",
                report.preload.files
            );
        }
        assert_eq!(
            report.preload.files.len(),
            3,
            "files and shapes must agree on the preload count; files: {:?}",
            report.preload.files
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn preload_modules_are_not_entry_points() {
        // N4: Signal's `*.preload.ts` renderer modules (603 files)
        // were counted as preload files. Only entry points — files a
        // window actually loads, or named `preload.*` — count; the
        // modules are listed separately in `preload.modules`.
        let app = fresh_test_dir("preload-modules");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             new BrowserWindow({ width: 800, height: 600, webPreferences: { preload: './preload.js' } });\n",
        );
        write_test_file(&app, "preload.js", "console.log('entry');\n");
        write_test_file(
            &app,
            "ts/components/conversation.preload.ts",
            "console.log('module');\n",
        );
        write_test_file(
            &app,
            "ts/util/webviewPreloads.ts",
            "console.log('plural stem is not an entry');\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(
            report.preload.files,
            vec!["preload.js"],
            "only the entry point counts; files: {:?}",
            report.preload.files
        );
        assert_eq!(
            report.preload.modules,
            vec!["ts/components/conversation.preload.ts"],
            "the *.preload.ts module must be listed separately; modules: {:?}",
            report.preload.modules
        );
        assert!(
            !report
                .preload
                .files
                .iter()
                .any(|f| f.contains("webviewPreloads")),
            "webviewPreloads.ts (plural stem) is neither entry nor module"
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn phantom_preload_paths_are_dropped() {
        // N1: VS Code's root-relative `FileAccess.asFileUri` preload
        // paths point at files that do not exist in the tree; they must
        // not become preload files. A `.min.js` bundle whose source
        // `.ts` exists resolves to the source instead.
        let app = fresh_test_dir("preload-phantom");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             new BrowserWindow({ width: 800, height: 600, webPreferences: { preload: '/vs/workbench/electron-browser/bootstrap/preload.js' } });\n\
             new BrowserWindow({ width: 800, height: 600, webPreferences: { preload: path.join(__dirname, 'entry.main.min.js') } });\n",
        );
        write_test_file(&app, "entry.main.ts", "console.log('source');\n");
        let report = scan(&app).expect("scan");
        assert!(
            !report.preload.files.iter().any(|f| f.contains("bootstrap")),
            "the nonexistent root-relative preload must be dropped; files: {:?}",
            report.preload.files
        );
        assert!(
            report.preload.files.iter().any(|f| f == "entry.main.ts"),
            "the .min.js bundle must resolve to its .ts source; files: {:?}",
            report.preload.files
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn strip_comments_handles_regex_literal_with_quotes() {
        // Joplin's resourceHandling.ts:291 shape: a regex literal
        // containing a quote used to leave a phantom open quote, so the
        // `//` comment after it was never stripped.
        let content =
            "const pattern = /[\"']/g; // trailing comment\nconst x = 1; // another comment\n";
        let stripped = strip_comments(content);
        assert!(
            stripped.contains("/[\"']/g;"),
            "the regex literal itself must survive; stripped: {stripped}"
        );
        assert!(
            !stripped.contains("trailing comment") && !stripped.contains("another comment"),
            "both // comments must be stripped; stripped: {stripped}"
        );
        assert!(
            stripped.contains("const x = 1;"),
            "code after the comment must survive; stripped: {stripped}"
        );
    }

    #[test]
    fn strip_comments_preserves_line_numbers_in_jsx() {
        // Proof-campaign bug: a JSX closing tag `</div>` was treated as
        // a regex start, and the regex branch's newline break re-pushed
        // the same newline — one phantom line per closing tag, shifting
        // every later line number (Mailspring's .tsx windows reported
        // +2/+3/+6 lines too late).
        let content = "const x = <div>\n  <span>hi</span>\n  <span>bye</span>\n</div>;\n// a comment\napp.quit();\n";
        let stripped = strip_comments(content);
        assert_eq!(
            stripped.lines().count(),
            content.lines().count(),
            "no newlines may be added or lost; stripped: {stripped:?}"
        );
        assert!(
            !stripped.contains("a comment"),
            "the // comment must still be stripped; stripped: {stripped:?}"
        );
        assert_eq!(
            stripped.lines().nth(5).unwrap().trim(),
            "app.quit();",
            "code after the JSX must stay on its own line; stripped: {stripped:?}"
        );
    }

    #[test]
    fn jsx_closing_tags_do_not_shift_window_lines() {
        let app = fresh_test_dir("jsx-lines");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app } = require('electron');\napp.whenReady().then(() => {});\n",
        );
        write_test_file(
            &app,
            "view.tsx",
            "import { BrowserWindow } from '@electron/remote';\n\
             const el = <div>\n  <span>one</span>\n  <span>two</span>\n  <span>three</span>\n</div>;\n\
             const win = new BrowserWindow({ width: 800, height: 600 });\n",
        );
        let report = scan(&app).expect("scan");
        assert_eq!(
            report.windows.len(),
            1,
            "one window; windows: {:?}",
            report.windows
        );
        assert_eq!(
            report.windows[0].line, 7,
            "the window is on line 7 (the `new BrowserWindow` line), not shifted by the JSX above it"
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn monorepo_package_json_prefers_shallow_app_package_and_ignores_vendor() {
        let app = fresh_test_dir("monorepo-tiebreak");
        // Root orchestrator: electron dep but no main entry.
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"mailspring-root","version":"0.0.1","dependencies":{"electron":"^44.0.0"}}"#,
        );
        write_test_file(&app, "index.js", "console.log('root');\n");
        // The real app package: main + existing target (score 13).
        write_test_file(
            &app,
            "app/package.json",
            r#"{"name":"mailspring","version":"2.0.0","main":"./src/main.js"}"#,
        );
        write_test_file(
            &app,
            "app/src/main.js",
            "const { app } = require('electron');\napp.whenReady().then(() => {});\n",
        );
        // An internal package tying the app package's score (13): the
        // old deeper-first tie-break picked this one.
        write_test_file(
            &app,
            "app/internal_packages/account-sidebar/package.json",
            r#"{"name":"account-sidebar","version":"0.1.0","main":"./lib/main"}"#,
        );
        write_test_file(
            &app,
            "app/internal_packages/account-sidebar/lib/main.js",
            "module.exports = {};\n",
        );
        // A vendored third-party package that would outscore both (13)
        // if vendor/ were not excluded.
        write_test_file(
            &app,
            "vendor/printenvz/package.json",
            r#"{"name":"printenvz","version":"1.0.0","main":"index.js"}"#,
        );
        write_test_file(&app, "vendor/printenvz/index.js", "module.exports = {};\n");
        let report = scan(&app).expect("scan");
        assert_eq!(report.app_name, "mailspring");
        assert_eq!(report.app_version.as_deref(), Some("2.0.0"));
        assert_eq!(report.main_entry.as_deref(), Some("app/src/main.js"));
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn import_gate_allows_di_and_parameter_electron_use() {
        let app = fresh_test_dir("import-gate");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        // main.js imports the Electron exports and passes them in;
        // setup.js never imports Electron itself — it receives the
        // exports as parameters (the DI / parameter case).
        write_test_file(
            &app,
            "main.js",
            "const { app, globalShortcut } = require('electron');\n\
             const setup = require('./setup.js');\n\
             setup(globalShortcut, app);\n",
        );
        write_test_file(
            &app,
            "setup.js",
            "module.exports = function setup(globalShortcut, app) {\n\
             globalShortcut.register('CommandOrControl+X', () => {});\n\
             app.quit();\n\
             };\n",
        );
        let report = scan(&app).expect("scan");
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.api == "globalShortcut" && f.file == "setup.js"),
            "DI-passed globalShortcut must be found in setup.js; findings: {:?}",
            report.findings
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.api == "app.quit" && f.file == "setup.js"),
            "parameter-passed app must be found in setup.js; findings: {:?}",
            report.findings
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn local_typed_session_parameter_does_not_produce_session_finding() {
        // The smoke re-scan's biggest remaining false-positive class:
        // Mailspring's draft/agent "sessions" (`session:
        // DraftEditingSession`, `session: AgentSessionState`) and GitHub
        // Desktop's credential sessions (`const session =
        // this.sessionsByKey.get(...)`) all counted as Electron's
        // `session`, because some other file imports it. A file-local
        // declaration shadows the app-wide fallback.
        let app = fresh_test_dir("shadow-session");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.ts","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.ts",
            "import { session } from 'electron';\n\
             const s = session.fromPartition('persist:main');\n",
        );
        write_test_file(
            &app,
            "view.ts",
            "function latestRevision(session: AgentSessionState): any {\n\
             return session.revisions.length ? session.revisions[0] : null;\n\
             }\n\
             function isBusy(session: AgentSessionState | null) {\n\
             return !!session && session.pendingRequest;\n\
             }\n",
        );
        write_test_file(
            &app,
            "store.ts",
            "const session = getLocalSession();\n\
             session.teardown();\n\
             const other = session.headerMessageId;\n",
        );
        let report = scan(&app).expect("scan");
        let session_findings: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.api == "session")
            .collect();
        assert_eq!(
            session_findings.len(),
            1,
            "only the real Electron session use counts; findings: {:?}",
            report.findings
        );
        assert_eq!(session_findings[0].file, "main.ts");
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn object_property_named_session_is_not_a_finding() {
        let app = fresh_test_dir("shadow-propkey");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { session } = require('electron');\n\
             const s = session.fromPartition('persist:main');\n\
             const opts = { session: imageProxySession(), other: 1 };\n",
        );
        let report = scan(&app).expect("scan");
        let session_finding = report
            .findings
            .iter()
            .find(|f| f.api == "session")
            .expect("real session use must be found");
        assert_eq!(
            session_finding.occurrences, 1,
            "the `{{ session: ... }}` property key must not count as an occurrence; findings: {:?}",
            report.findings
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn electron_typed_parameter_is_not_a_shadow() {
        // The shadow exception: `app: Electron.App` really IS Electron's
        // app, passed in typed — it must keep resolving.
        let app = fresh_test_dir("shadow-electron-type");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.ts","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.ts",
            "import { app } from 'electron';\napp.whenReady().then(() => {});\n",
        );
        write_test_file(
            &app,
            "setup.ts",
            "export function setup(app: Electron.App) {\n  app.quit();\n}\n",
        );
        let report = scan(&app).expect("scan");
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.api == "app.quit" && f.file == "setup.ts"),
            "an Electron-typed parameter must still resolve; findings: {:?}",
            report.findings
        );
        fs::remove_dir_all(&app).ok();
    }
}
