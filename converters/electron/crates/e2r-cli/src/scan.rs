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
//! in a comment cannot produce a finding; string literals are preserved,
//! because channel names, titles, and URLs live in them. Regex literals
//! containing `//` are a known blind spot of the stripper (see
//! [`strip_comments`]).
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
use std::collections::{BTreeSet, HashMap};
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
const NATIVE_MODULES: &[&str] = &[
    "node-pty",
    "better-sqlite3",
    "sqlite3",
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
}

/// The API pattern table. Specific patterns come before their generic
/// fallback within each family — the skip rule above depends on that order.
///
/// Bare-word patterns (`autoUpdater`, `nativeTheme`, ...) require a
/// call-site suffix (`.`, `(`, or `[`): a bare identifier also matches its
/// own import/destructure line, which is not a use — a dead import must
/// not drive a finding or a plugin inclusion.
const API_PATTERNS: &[ApiPattern] = &[
    ApiPattern {
        api: "app.getPath",
        category: "app",
        pattern: r"\bapp\.getPath\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "app.quit",
        category: "app",
        pattern: r"\bapp\.quit\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "app.requestSingleInstanceLock",
        category: "app",
        pattern: r#"\brequestSingleInstanceLock\b|['"]second-instance['"]"#,
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "app.setAsDefaultProtocolClient",
        category: "app",
        pattern: r#"\bsetAsDefaultProtocolClient\b|\bisDefaultProtocolClient\b|['"]open-url['"]"#,
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "app",
        category: "app",
        pattern: r"\bapp\.",
        skip_if_prefix_matched: Some("app."),
    },
    ApiPattern {
        api: "dialog.showOpenDialog",
        category: "dialog",
        pattern: r"\bdialog\.showOpenDialog\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "dialog.showSaveDialog",
        category: "dialog",
        pattern: r"\bdialog\.showSaveDialog\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "dialog.showMessageBox",
        category: "dialog",
        pattern: r"\bdialog\.showMessageBox\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "dialog",
        category: "dialog",
        pattern: r"\bdialog\.",
        skip_if_prefix_matched: Some("dialog."),
    },
    ApiPattern {
        api: "shell.openExternal",
        category: "shell",
        pattern: r"\bshell\.openExternal\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "shell.openPath",
        category: "shell",
        pattern: r"\bshell\.openPath\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "shell",
        category: "shell",
        pattern: r"\bshell\.",
        skip_if_prefix_matched: Some("shell."),
    },
    ApiPattern {
        api: "webContents.send",
        category: "webcontents",
        pattern: r"\bwebContents\.send\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "webContents.openDevTools",
        category: "webcontents",
        pattern: r"\bwebContents\.openDevTools\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "webContents",
        category: "webcontents",
        pattern: r"\bwebContents\.",
        skip_if_prefix_matched: Some("webContents."),
    },
    ApiPattern {
        api: "Tray",
        category: "tray",
        pattern: r"\bnew\s+Tray\s*\(",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "Notification",
        category: "notification",
        pattern: r"\bnew\s+Notification\s*\(",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "globalShortcut",
        category: "shortcut",
        pattern: r"\bglobalShortcut[\.\(\[]",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "protocol",
        category: "protocol",
        pattern: r"\bprotocol\.",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "session",
        category: "session",
        pattern: r"\bsession\.",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "safeStorage",
        category: "storage",
        pattern: r"\bsafeStorage[\.\(\[]",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "nativeTheme",
        category: "theme",
        pattern: r"\bnativeTheme[\.\(\[]",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "autoUpdater",
        category: "updater",
        pattern: r"\bautoUpdater[\.\(\[]",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "clipboard",
        category: "clipboard",
        pattern: r"\bclipboard\.",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "screen",
        category: "screen",
        pattern: r"\bscreen\.",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "powerMonitor",
        category: "power",
        pattern: r"\bpowerMonitor[\.\(\[]",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "crashReporter",
        category: "crash",
        pattern: r"\bcrashReporter[\.\(\[]",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "net",
        category: "net",
        pattern: r"\bnet\.",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "nativeImage",
        category: "image",
        pattern: r"\bnativeImage[\.\(\[]",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "remote",
        category: "remote",
        pattern: r"\bremote\.",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "electron-window-state",
        category: "window",
        pattern: r#"require\s*\(\s*['"]electron-window-state['"]\s*\)|from\s+['"]electron-window-state['"]"#,
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "nodeIntegration",
        category: "security",
        pattern: r"\bnodeIntegration\s*:\s*true\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "contextIsolation",
        category: "security",
        pattern: r"\bcontextIsolation\s*:\s*false\b",
        skip_if_prefix_matched: None,
    },
];

/// All compiled regexes for one scan, built once and reused per file.
struct Patterns {
    electron_import: Regex,
    ipc_main: Regex,
    ipc_renderer: Regex,
    webcontents_send: Regex,
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
    api_patterns: Vec<(ApiPattern, Regex)>,
}

impl Patterns {
    fn new() -> Self {
        // The gate for main-process patterns: `electron` itself plus the
        // known ecosystem packages, so a file that drives updates via
        // `electron-updater` (without importing `electron`) is not
        // invisible to the API rules.
        const ECOSYSTEM: &str =
            "electron|electron-updater|electron-store|electron-window-state|@electron/remote";
        Self {
            electron_import: Regex::new(&format!(
                r#"require\s*\(\s*['"]({ECOSYSTEM})['"]\s*\)|from\s+['"]({ECOSYSTEM})['"]"#
            ))
            .expect("electron import regex"),
            ipc_main: Regex::new(r#"ipcMain\.(handle|on)\s*\(\s*['"]([^'"]+)['"]"#)
                .expect("ipcMain regex"),
            ipc_renderer: Regex::new(r#"ipcRenderer\.(invoke|send|on)\s*\(\s*['"]([^'"]+)['"]"#)
                .expect("ipcRenderer regex"),
            webcontents_send: Regex::new(r#"webContents\.send\s*\(\s*['"]([^'"]+)['"]"#)
                .expect("webContents.send regex"),
            expose_call: Regex::new(r"contextBridge\.exposeInMainWorld\s*\(")
                .expect("exposeInMainWorld call regex"),
            browser_window: Regex::new(r"new\s+BrowserWindow\s*\(").expect("BrowserWindow regex"),
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
            api_patterns: API_PATTERNS
                .iter()
                .map(|p| (*p, Regex::new(p.pattern).expect("API pattern regex")))
                .collect(),
        }
    }
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

/// Read `package.json` (if present) for the app name, version, product
/// name, bundle id, Electron version, main entry and dependency-based
/// native-module / port-needing-dep detection.
///
/// The bundle id and product name also come from the electron-builder
/// config when `package.json` does not carry them: the `build` key in
/// `package.json` first, then `electron-builder.json` at the app root.
///
/// A missing or unparsable `package.json` is not an error — the source
/// scan still runs and the report just has less metadata.
fn read_package_json(app_dir: &Path) -> PackageInfo {
    let mut info = PackageInfo::default();
    let Ok(text) = fs::read_to_string(app_dir.join("package.json")) else {
        return info;
    };
    let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) else {
        return info;
    };

    info.name = pkg.get("name").and_then(|v| v.as_str()).map(str::to_string);
    info.version = pkg
        .get("version")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    info.product_name = pkg
        .get("productName")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    info.app_id = pkg
        .get("build")
        .and_then(|b| b.get("appId"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    info.main_entry = pkg
        .get("main")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| Some("main.js".to_string()));

    // electron-builder.json is the other home of productName / appId.
    if info.product_name.is_none() || info.app_id.is_none() {
        if let Ok(builder_text) = fs::read_to_string(app_dir.join("electron-builder.json")) {
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
    }

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

    for section in ["dependencies", "devDependencies"] {
        if let Some(deps) = pkg.get(section).and_then(|d| d.as_object()) {
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
/// Starts at 100 and deducts: 15 per native module; 10 per Manual
/// finding (capped at −40); 5 per Assisted finding beyond the first 5
/// (capped at −15); 10 when `ipcRenderer` is used but no preload script
/// was found (the bridge has to be invented, not translated).
///
/// Pure-JS port-needing deps (e.g. `electron-updater`) cost nothing
/// here: they have 1:1 plugin equivalents, so they are AUTO conversions,
/// not 15-point native-module penalties.
pub fn feasibility_score(
    native_modules: usize,
    manual_findings: usize,
    assisted_findings: usize,
    has_preload: bool,
    uses_ipc_renderer: bool,
) -> u8 {
    let mut score: i32 = 100;
    score -= 15 * native_modules as i32;
    score -= (10 * manual_findings as i32).min(40);
    let assisted_excess = assisted_findings.saturating_sub(5);
    score -= (5 * assisted_excess as i32).min(15);
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

/// Strip `//` and `/* */` comments from JavaScript/TypeScript source,
/// replacing every comment character with a space (newlines are kept, so
/// line numbers are unchanged) and leaving string literals untouched.
///
/// The stripper is string-aware (`'...'`, `"..."`, `` `...` `` with
/// backslash escapes), so a `//` inside a URL string is not treated as a
/// comment. Known blind spots, accepted for a heuristic scanner:
/// regex literals containing `//` (the rest of that line is stripped),
/// and `${...}` interpolations inside template literals (a `//` there is
/// treated as a comment start).
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
    let (width, width_known, mut from_window_state) = match patterns
        .width
        .captures(options)
        .and_then(|c| c[1].parse().ok())
    {
        Some(w) => (w, true, false),
        None => match patterns
            .window_state_width
            .captures(file_content)
            .and_then(|c| c[1].parse().ok())
        {
            Some(w) => (w, true, true),
            None => (800, false, false),
        },
    };
    let (height, height_known, from_ws_height) = match patterns
        .height
        .captures(options)
        .and_then(|c| c[1].parse().ok())
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
    };
    from_window_state = from_window_state || from_ws_height;
    let min_width = patterns
        .min_width
        .captures(options)
        .and_then(|c| c[1].parse().ok());
    let min_height = patterns
        .min_height
        .captures(options)
        .and_then(|c| c[1].parse().ok());
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

/// Find every `new BrowserWindow({...})` in one file, returning the
/// windows plus any `webPreferences.preload` paths found in their options.
/// `content` must be comment-stripped (see [`strip_comments`]).
fn extract_windows(
    patterns: &Patterns,
    content: &str,
    file: &str,
) -> (Vec<WindowInfo>, Vec<String>) {
    let mut windows = Vec::new();
    let mut preload_paths = Vec::new();
    for m in patterns.browser_window.find_iter(content) {
        let rest = &content[m.end()..];
        let Some(brace_rel) = rest.find('{') else {
            continue;
        };
        // Only whitespace may sit between `(` and the options object;
        // anything else means this is `new BrowserWindow()` with no
        // options, and the `{` belongs to something further down.
        if !rest[..brace_rel].trim().is_empty() {
            continue;
        }
        let brace_idx = m.end() + brace_rel;
        let Some((options, _end)) = extract_balanced(content, brace_idx, '{', '}') else {
            continue;
        };
        let line = line_number(content, m.start());
        windows.push(extract_window_options(
            patterns, &options, file, line, content,
        ));
        if let Some(cap) = patterns.preload_path.captures(&options) {
            preload_paths.push(cap[1].to_string());
        }
    }
    (windows, preload_paths)
}

/// Find the first `Menu.buildFromTemplate(...)` in one file.
///
/// A static array literal argument is kept as template source (capped at
/// ~4 KiB) so the converter can translate it; a variable or expression
/// argument is recorded as non-static and left for human review.
fn extract_menu(patterns: &Patterns, content: &str, file: &str) -> Option<MenuInfo> {
    let m = patterns.menu_template.find(content)?;
    // The match ends right after `(`, so the paren itself is at end - 1.
    let open_idx = m.end() - 1;
    let (arg, _end) = extract_balanced(content, open_idx, '(', ')')?;
    let trimmed = arg.trim();
    let is_static_literal = trimmed.starts_with('[');
    let template_source = if is_static_literal {
        Some(trimmed.chars().take(TEMPLATE_SOURCE_CAP).collect())
    } else {
        None
    };
    Some(MenuInfo {
        is_static_literal,
        template_source,
        file: file.to_string(),
        line: line_number(content, m.start()),
    })
}

/// Find every IPC channel in one file, paired with the API name that
/// produced it (e.g. `"ipcMain.handle"`), for finding classification.
///
/// `ipcRenderer.invoke` and `ipcRenderer.send` both report direction
/// `"send"` (renderer → main); `ipcMain.handle` reports `"handle"`,
/// `ipcMain.on` reports `"on"`, and `ipcRenderer.on` reports `"listen"`
/// (the renderer subscribing to a main → renderer event — a different
/// direction from `ipcMain.on`, which the Rust side must receive).
/// `content` must be comment-stripped.
fn collect_ipc_channels(
    patterns: &Patterns,
    content: &str,
    file: &str,
) -> Vec<(IpcChannel, &'static str)> {
    let mut channels = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_no = idx + 1;
        for cap in patterns.ipc_main.captures_iter(line) {
            let (direction, api) = match &cap[1] {
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
    }
    channels
}

/// Find every main → renderer `webContents.send('channel', ...)` call
/// site in one file. The pattern is unambiguous (only Electron has
/// `webContents`), so this runs on every file, gated or not.
/// `content` must be comment-stripped.
fn collect_send_channels(patterns: &Patterns, content: &str, file: &str) -> Vec<IpcChannel> {
    let mut channels = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        for cap in patterns.webcontents_send.captures_iter(line) {
            channels.push(IpcChannel {
                name: cap[1].to_string(),
                direction: "emit".to_string(),
                file: file.to_string(),
                line: idx + 1,
            });
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
/// `(url, file, line, in_load_url_file)` — the last flag marks files that
/// contain `loadURL`, which are preferred when picking the winner.
fn find_dev_url_candidates(
    patterns: &Patterns,
    content: &str,
    file: &str,
) -> Vec<(DevUrlInfo, bool)> {
    let in_load_url_file = content.contains("loadURL");
    let mut out = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        for cap in patterns.dev_url_literal.captures_iter(line) {
            out.push((
                DevUrlInfo {
                    url: cap[0].to_string(),
                    file: file.to_string(),
                    line: idx + 1,
                },
                in_load_url_file,
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
                in_load_url_file,
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
fn add_finding(
    findings: &mut Vec<Finding>,
    seen: &mut HashMap<(String, String), usize>,
    api: &str,
    category: &str,
    file: &str,
    line: usize,
) {
    let (classification, tauri_target, note) = classify(api);
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

fn is_preload_filename(path: &Path) -> bool {
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    name == "preload"
        || name.starts_with("preload.")
        || name.starts_with("preload-")
        || name.starts_with("preload_")
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

    let mut files_scanned = 0usize;
    let mut ipc_channels = Vec::new();
    let mut main_to_renderer = Vec::new();
    let mut findings: Vec<Finding> = Vec::new();
    let mut seen_findings: HashMap<(String, String), usize> = HashMap::new();
    let mut windows = Vec::new();
    let mut menu: Option<MenuInfo> = None;
    let mut preload_files: BTreeSet<String> = BTreeSet::new();
    let mut exposed_keys: BTreeSet<String> = BTreeSet::new();
    let mut preload_shapes: Vec<PreloadShape> = Vec::new();
    let mut uses_context_bridge = false;
    let mut uses_ipc_renderer = false;
    let mut dev_url_candidates: Vec<(DevUrlInfo, bool)> = Vec::new();

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
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // unreadable (binary, permissions) — skip
        };
        files_scanned += 1;
        // All matching below runs on comment-stripped text (same line
        // numbers, string literals intact).
        let stripped = strip_comments(&content);

        if is_preload_filename(path) {
            preload_files.insert(rel.clone());
        }

        let imports_electron = patterns.electron_import.is_match(&stripped);

        // IPC channels: every file, including renderer and preload code
        // that never imports Electron directly.
        for (channel, api) in collect_ipc_channels(&patterns, &stripped, &rel) {
            if api.starts_with("ipcRenderer") {
                uses_ipc_renderer = true;
            }
            add_finding(
                &mut findings,
                &mut seen_findings,
                api,
                "ipc",
                &rel,
                channel.line,
            );
            ipc_channels.push(channel);
        }

        // Main → renderer channels: every file (the pattern is unambiguous).
        for channel in collect_send_channels(&patterns, &stripped, &rel) {
            add_finding(
                &mut findings,
                &mut seen_findings,
                "webContents.send",
                "webcontents",
                &rel,
                channel.line,
            );
            main_to_renderer.push(channel);
        }

        // contextBridge: every file (preload scripts live here). The
        // call-based scan also recovers each exposed object's shape.
        for m in patterns.expose_call.find_iter(&stripped) {
            // The match ends right after `(`, so the paren is at end - 1.
            let open_idx = m.end() - 1;
            let Some((args, _)) = extract_balanced(&stripped, open_idx, '(', ')') else {
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
            let line = line_number(&stripped, m.start());
            add_finding(
                &mut findings,
                &mut seen_findings,
                "contextBridge",
                "preload",
                &rel,
                line,
            );
            if let Some(shape) = extract_preload_shape(&stripped, &rel, &key, open_idx) {
                preload_shapes.push(shape);
            }
        }

        // Main-process APIs, windows, menus, and dev URLs: only files that
        // import Electron (or an ecosystem package), so unrelated code
        // with a variable named `app` or `net` does not pollute the report.
        if imports_electron {
            for (idx, line) in stripped.lines().enumerate() {
                let mut matched_on_line: Vec<&str> = Vec::new();
                for (pat, re) in &patterns.api_patterns {
                    if let Some(prefix) = pat.skip_if_prefix_matched {
                        if matched_on_line.iter().any(|a| a.starts_with(prefix)) {
                            continue;
                        }
                    }
                    if re.is_match(line) {
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
                        add_finding(
                            &mut findings,
                            &mut seen_findings,
                            pat.api,
                            pat.category,
                            &rel,
                            idx + 1,
                        );
                    }
                }
            }

            let (mut found_windows, preload_paths) = extract_windows(&patterns, &stripped, &rel);
            for w in &found_windows {
                add_finding(
                    &mut findings,
                    &mut seen_findings,
                    "BrowserWindow",
                    "window",
                    &rel,
                    w.line,
                );
            }
            windows.append(&mut found_windows);
            for p in preload_paths {
                // Resolve against the referencing file's directory, so
                // `./preload.js` from `desktop/app.js` dedups with the
                // `desktop/preload.js` found by filename.
                preload_files.insert(resolve_relative(&rel, &p));
            }

            if menu.is_none() {
                if let Some(m) = extract_menu(&patterns, &stripped, &rel) {
                    add_finding(
                        &mut findings,
                        &mut seen_findings,
                        "Menu",
                        "menu",
                        &rel,
                        m.line,
                    );
                    menu = Some(m);
                }
            }

            dev_url_candidates.extend(find_dev_url_candidates(&patterns, &stripped, &rel));
        }
    }

    // Build tooling is a note, not a native module: record it as an
    // assisted finding so it shows up without skewing the score.
    for tool in &pkg.build_tooling {
        push_finding(
            &mut findings,
            &mut seen_findings,
            Finding {
                api: tool.clone(),
                category: "build".to_string(),
                file: "package.json".to_string(),
                line: 1,
                classification: Classification::Assisted,
                tauri_target: Some("cargo tauri build".to_string()),
                note: "Build/packaging tooling — Tauri bundles with `cargo tauri build` instead."
                    .to_string(),
                occurrences: 1,
            },
        );
    }

    // Port-needing JS deps get a finding each (classified through the
    // mapping table, so `electron-updater` is AUTO and `@electron/remote`
    // is MANUAL) — they are visible in the report without the 15-point
    // native-module penalty.
    for dep in &pkg.port_needing_deps {
        add_finding(
            &mut findings,
            &mut seen_findings,
            dep,
            "dependency",
            "package.json",
            1,
        );
    }

    // Deterministic order, independent of filesystem walk order.
    findings.sort_by(|a, b| {
        (a.file.as_str(), a.line, a.api.as_str()).cmp(&(b.file.as_str(), b.line, b.api.as_str()))
    });
    ipc_channels.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));
    main_to_renderer.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));
    windows.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));
    preload_shapes.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));

    // Dev URL: prefer a candidate from a file that calls `loadURL`, then
    // the earliest by (file, line) — deterministic across walks.
    dev_url_candidates.sort_by(|a, b| {
        (std::cmp::Reverse(a.1), a.0.file.as_str(), a.0.line).cmp(&(
            std::cmp::Reverse(b.1),
            b.0.file.as_str(),
            b.0.line,
        ))
    });
    let dev_url = dev_url_candidates.into_iter().next().map(|(d, _)| d);

    let manual = findings
        .iter()
        .filter(|f| f.classification == Classification::Manual)
        .count();
    let assisted = findings
        .iter()
        .filter(|f| f.classification == Classification::Assisted)
        .count();
    let has_preload = !preload_files.is_empty();
    let score = feasibility_score(
        pkg.native_modules.len(),
        manual,
        assisted,
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
            uses_context_bridge,
            exposed_keys: exposed_keys.into_iter().collect(),
            shapes: preload_shapes,
        },
        native_modules: pkg.native_modules,
        port_needing_deps: pkg.port_needing_deps,
        dev_url,
        feasibility_score: score,
        verdict: verdict(score).to_string(),
        windows,
        menu,
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
    if !report.preload.exposed_keys.is_empty() {
        out.push_str(&format!(
            "  Exposed keys: {}\n",
            report.preload.exposed_keys.join(", ")
        ));
    }

    if let Some(menu) = &report.menu {
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
            out.push_str(&format!(
                "    - {} — {}:{} → {}{}\n",
                f.api, f.file, f.line, target, count
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
        let channels = collect_ipc_channels(&patterns, content, "main.js");
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
        let channels = collect_send_channels(&patterns, content, "app.js");
        // The variable channel (`commandName`) cannot be resolved statically.
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].name, "wpLogin");
        assert_eq!(channels[0].direction, "emit");
        assert_eq!(channels[1].name, "appCommand");
        assert_eq!(channels[1].line, 2);
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
        assert_eq!(feasibility_score(0, 0, 0, true, false), 100);
        assert_eq!(feasibility_score(1, 0, 0, true, false), 85);
        assert_eq!(feasibility_score(0, 2, 0, true, false), 80);
        // Manual penalty caps at -40.
        assert_eq!(feasibility_score(0, 10, 0, true, false), 60);
        // Assisted: only the ones beyond the first 5 count, capped at -15.
        assert_eq!(feasibility_score(0, 0, 5, true, false), 100);
        assert_eq!(feasibility_score(0, 0, 8, true, false), 85);
        assert_eq!(feasibility_score(0, 0, 20, true, false), 85);
        // IPC without a preload script costs 10.
        assert_eq!(feasibility_score(0, 0, 0, false, true), 90);
        assert_eq!(feasibility_score(0, 0, 0, false, false), 100);
        // Score floors at 0.
        assert_eq!(feasibility_score(10, 10, 20, false, true), 0);
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
        assert!(found[0].1, "file calls loadURL");

        let host = "indexPath = url.format({\n  protocol: 'http:',\n  host: 'localhost:8080',\n});";
        let found = find_dev_url_candidates(&patterns, host, "main.js");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0.url, "http://localhost:8080");
        assert_eq!(found[0].0.line, 3);
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
    fn dead_autoupdater_import_is_ignored_but_call_sites_count() {
        let app = fresh_test_dir("dead-updater");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","version":"1.0.0"}"#,
        );
        // Electro-dash shape: a renderer file imports autoUpdater and never
        // uses it; the main file really calls it.
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
        let updater: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.api == "autoUpdater")
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
        // Only the true native module costs 15 points; the two pure-JS
        // deps cost nothing (their AUTO findings do not affect the score).
        assert_eq!(report.feasibility_score, 85);
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
        // still get its autoUpdater call sites flagged.
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
                .any(|f| f.api == "autoUpdater" && f.file == "updater.js"),
            "updater.js must produce an autoUpdater finding; findings: {:?}",
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
}
