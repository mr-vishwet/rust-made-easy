//! Scan an Electron app and report how it maps to Tauri 2.
//!
//! The scanner is a regex-based X-ray, not a full JavaScript parser: it
//! reads `package.json`, walks the source tree for `.js`/`.ts` files, and
//! pattern-matches Electron API usage. That makes it fast and dependency
//! light, at the cost of occasional false positives (e.g. a local variable
//! named `app` in a file that imports Electron). Main-process API patterns
//! are only applied to files that import `electron`, which keeps renderer
//! code from polluting the report; IPC and `contextBridge` patterns run on
//! every file, because renderer and preload files rarely import Electron
//! directly.
//!
//! Every finding is classified through the mapping table
//! ([`crate::mappings`]) and rolled into a 0–100 feasibility score.

use crate::mappings;
use crate::model::{
    Classification, Finding, IpcChannel, MenuInfo, PreloadInfo, ScanReport, WindowInfo,
};
use anyhow::Result;
use regex::Regex;
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

/// Dependencies that are native addons — or that otherwise need a real
/// port, not a mechanical translation — when moving to Tauri.
///
/// `electron-store` and `electron-updater` are pure JavaScript, but they
/// still need a port (to `tauri-plugin-store` / `tauri-plugin-updater`),
/// so they are listed here rather than silently passing as portable.
const NATIVE_MODULES: &[&str] = &[
    "node-pty",
    "electron-store",
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
    "@electron/remote",
    "electron-updater",
];

/// Build/packaging tooling. Reported as a note, not a native module:
/// Tauri replaces these with `cargo tauri build`, so there is nothing
/// to port — only the build setup to redo.
const BUILD_TOOLING: &[&str] = &["electron-builder", "electron-packager"];

/// Directories never descended into while scanning.
const SKIP_DIRS: &[&str] = &["node_modules", "dist", ".git", "out"];

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
        pattern: r"\bglobalShortcut\b",
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
        pattern: r"\bsafeStorage\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "nativeTheme",
        category: "theme",
        pattern: r"\bnativeTheme\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "autoUpdater",
        category: "updater",
        pattern: r"\bautoUpdater\b",
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
        pattern: r"\bpowerMonitor\b",
        skip_if_prefix_matched: None,
    },
    ApiPattern {
        api: "crashReporter",
        category: "crash",
        pattern: r"\bcrashReporter\b",
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
        pattern: r"\bnativeImage\b",
        skip_if_prefix_matched: None,
    },
];

/// All compiled regexes for one scan, built once and reused per file.
struct Patterns {
    electron_import: Regex,
    ipc_main: Regex,
    ipc_renderer: Regex,
    context_bridge: Regex,
    browser_window: Regex,
    menu_template: Regex,
    preload_path: Regex,
    width: Regex,
    height: Regex,
    title: Regex,
    resizable: Regex,
    frame: Regex,
    api_patterns: Vec<(ApiPattern, Regex)>,
}

impl Patterns {
    fn new() -> Self {
        Self {
            electron_import: Regex::new(
                r#"require\s*\(\s*['"]electron['"]\s*\)|from\s+['"]electron['"]"#,
            )
            .expect("electron import regex"),
            ipc_main: Regex::new(r#"ipcMain\.(handle|on)\s*\(\s*['"]([^'"]+)['"]"#)
                .expect("ipcMain regex"),
            ipc_renderer: Regex::new(r#"ipcRenderer\.(invoke|send|on)\s*\(\s*['"]([^'"]+)['"]"#)
                .expect("ipcRenderer regex"),
            context_bridge: Regex::new(
                r#"contextBridge\.exposeInMainWorld\s*\(\s*['"]([^'"]+)['"]"#,
            )
            .expect("contextBridge regex"),
            browser_window: Regex::new(r"new\s+BrowserWindow\s*\(").expect("BrowserWindow regex"),
            menu_template: Regex::new(r"Menu\.buildFromTemplate\s*\(")
                .expect("Menu.buildFromTemplate regex"),
            // Catches both `preload: './preload.js'` and
            // `preload: path.join(__dirname, 'preload.js')`.
            preload_path: Regex::new(r#"preload\s*:.*?['"]([^'"]+\.(?:js|ts|mjs|cjs))['"]"#)
                .expect("preload path regex"),
            width: Regex::new(r"width\s*:\s*(\d+)").expect("width regex"),
            height: Regex::new(r"height\s*:\s*(\d+)").expect("height regex"),
            title: Regex::new(r#"title\s*:\s*['"]([^'"]+)['"]"#).expect("title regex"),
            resizable: Regex::new(r"resizable\s*:\s*(true|false)").expect("resizable regex"),
            frame: Regex::new(r"frame\s*:\s*(true|false)").expect("frame regex"),
            api_patterns: API_PATTERNS
                .iter()
                .map(|p| (*p, Regex::new(p.pattern).expect("API pattern regex")))
                .collect(),
        }
    }
}

/// What `package.json` told us about the app.
#[derive(Debug, Default)]
struct PackageInfo {
    name: Option<String>,
    electron_version: Option<String>,
    main_entry: Option<String>,
    native_modules: Vec<String>,
    build_tooling: Vec<String>,
}

/// Read `package.json` (if present) for the app name, Electron version,
/// main entry and dependency-based native-module detection.
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
    info.main_entry = pkg
        .get("main")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| Some("main.js".to_string()));

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
                } else if BUILD_TOOLING.contains(&name.as_str()) {
                    info.build_tooling.push(name.clone());
                }
            }
        }
    }
    info.native_modules.sort();
    info.native_modules.dedup();
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

/// Extract the text inside the balanced delimiter pair whose opening
/// character sits at `open_idx` (e.g. the `{` of a `BrowserWindow`
/// options object or the `(` of a `buildFromTemplate` call).
///
/// String literals (`'...'`, `"..."`, `` `...` ``) are skipped, so a
/// brace or paren inside a string cannot unbalance the count. Braces
/// inside `${...}` template interpolations and `//` comments are not
/// handled — an accepted limitation for a v0.1 heuristic scanner.
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

/// Parse one `BrowserWindow` options object into a [`WindowInfo`].
/// Missing options fall back to Electron's defaults (800×600, resizable,
/// framed, no title).
fn extract_window_options(
    patterns: &Patterns,
    options: &str,
    file: &str,
    line: usize,
) -> WindowInfo {
    let width = patterns
        .width
        .captures(options)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(800);
    let height = patterns
        .height
        .captures(options)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(600);
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
        title,
        resizable,
        frame,
        file: file.to_string(),
        line,
    }
}

/// Find every `new BrowserWindow({...})` in one file, returning the
/// windows plus any `webPreferences.preload` paths found in their options.
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
        windows.push(extract_window_options(patterns, &options, file, line));
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
/// `"send"` (renderer → main); `ipcMain.handle` reports `"handle"` and
/// both `.on` variants report `"on"`.
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
                "on" => ("on", "ipcRenderer.on"),
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

/// Push a finding unless this (API, file) pair was already recorded —
/// findings deduplicate per file, keeping the first occurrence's line.
fn push_finding(
    findings: &mut Vec<Finding>,
    seen: &mut HashSet<(String, String)>,
    finding: Finding,
) {
    let key = (finding.api.clone(), finding.file.clone());
    if seen.insert(key) {
        findings.push(finding);
    }
}

/// Classify `api` through the mapping table and record the finding.
fn add_finding(
    findings: &mut Vec<Finding>,
    seen: &mut HashSet<(String, String)>,
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
    let mut findings: Vec<Finding> = Vec::new();
    let mut seen_findings: HashSet<(String, String)> = HashSet::new();
    let mut windows = Vec::new();
    let mut menu: Option<MenuInfo> = None;
    let mut preload_files: BTreeSet<String> = BTreeSet::new();
    let mut exposed_keys: BTreeSet<String> = BTreeSet::new();
    let mut uses_context_bridge = false;
    let mut uses_ipc_renderer = false;

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

        if is_preload_filename(path) {
            preload_files.insert(rel.clone());
        }

        let imports_electron = patterns.electron_import.is_match(&content);

        // IPC channels: every file, including renderer and preload code
        // that never imports Electron directly.
        for (channel, api) in collect_ipc_channels(&patterns, &content, &rel) {
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

        // contextBridge: every file (preload scripts live here).
        for (idx, line) in content.lines().enumerate() {
            for cap in patterns.context_bridge.captures_iter(line) {
                uses_context_bridge = true;
                exposed_keys.insert(cap[1].to_string());
                add_finding(
                    &mut findings,
                    &mut seen_findings,
                    "contextBridge",
                    "preload",
                    &rel,
                    idx + 1,
                );
            }
        }

        // Main-process APIs, windows and menus: only files that import
        // Electron, so unrelated code with a variable named `app` or
        // `net` does not pollute the report.
        if imports_electron {
            for (idx, line) in content.lines().enumerate() {
                let mut matched_on_line: Vec<&str> = Vec::new();
                for (pat, re) in &patterns.api_patterns {
                    if let Some(prefix) = pat.skip_if_prefix_matched {
                        if matched_on_line.iter().any(|a| a.starts_with(prefix)) {
                            continue;
                        }
                    }
                    if re.is_match(line) {
                        matched_on_line.push(pat.api);
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

            let (mut found_windows, preload_paths) = extract_windows(&patterns, &content, &rel);
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
                preload_files.insert(p);
            }

            if menu.is_none() {
                if let Some(m) = extract_menu(&patterns, &content, &rel) {
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
            },
        );
    }

    // Deterministic order, independent of filesystem walk order.
    findings.sort_by(|a, b| {
        (a.file.as_str(), a.line, a.api.as_str()).cmp(&(b.file.as_str(), b.line, b.api.as_str()))
    });
    ipc_channels.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));
    windows.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));

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
        electron_version: pkg.electron_version,
        main_entry: pkg.main_entry,
        files_scanned,
        ipc_channels,
        findings,
        preload: PreloadInfo {
            files: preload_files.into_iter().collect(),
            uses_context_bridge,
            exposed_keys: exposed_keys.into_iter().collect(),
        },
        native_modules: pkg.native_modules,
        feasibility_score: score,
        verdict: verdict(score).to_string(),
        windows,
        menu,
    })
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
    out.push_str(&format!(
        "Main entry:       {}\n",
        report.main_entry.as_deref().unwrap_or("not found")
    ));
    out.push_str(&format!("Files scanned:    {}\n", report.files_scanned));

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
        out.push_str(&format!(
            "  - {}x{} \"{}\" ({}, {}) — {}:{}\n",
            w.width, w.height, title, resizable, frame, w.file, w.line
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
            out.push_str(&format!(
                "    - {} — {}:{} → {}\n",
                f.api, f.file, f.line, target
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
        let win = extract_window_options(&patterns, options, "main.js", 7);
        assert_eq!(win.width, 1280);
        assert_eq!(win.height, 720);
        assert_eq!(win.title.as_deref(), Some("My App"));
        assert!(!win.resizable);
        assert!(!win.frame);
        assert_eq!(win.file, "main.js");
        assert_eq!(win.line, 7);
    }

    #[test]
    fn browser_window_options_default_when_missing() {
        let patterns = Patterns::new();
        let win = extract_window_options(&patterns, "{ webPreferences: {} }", "main.js", 1);
        assert_eq!(win.width, 800);
        assert_eq!(win.height, 600);
        assert_eq!(win.title, None);
        assert!(win.resizable);
        assert!(win.frame);
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
        assert_eq!(channels[4].0.line, 5);
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
}
