//! Shared data model for scan reports.
//!
//! Every type here is serde-serializable so a [`ScanReport`] can be dumped
//! verbatim as JSON (`e2r scan --json`) or consumed by other tools.

use serde::{Deserialize, Serialize};

/// How much of an Electron API usage the converter can handle on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Classification {
    /// Direct Tauri equivalent exists; conversion is mechanical.
    Auto,
    /// A Tauri equivalent exists but needs human decisions (a plugin,
    /// different semantics, or a small rewrite).
    Assisted,
    /// No direct Tauri equivalent; the feature needs a manual port.
    Manual,
}

/// One IPC channel between the main and renderer processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcChannel {
    /// Channel name, e.g. `"read-file"`.
    pub name: String,
    /// `"handle"` (`ipcMain.handle`), `"on"` (`ipcMain.on` /
    /// `ipcRenderer.on`) or `"send"` (`ipcRenderer.invoke` /
    /// `ipcRenderer.send`).
    pub direction: String,
    /// Source file, relative to the scanned app directory.
    pub file: String,
    /// 1-based line number of the call.
    pub line: usize,
}

/// One Electron API usage found in the scanned source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Electron API name, e.g. `"ipcMain.handle"` or `"BrowserWindow"`.
    pub api: String,
    /// Coarse category, e.g. `"ipc"`, `"dialog"`, `"window"`.
    pub category: String,
    /// Source file, relative to the scanned app directory.
    pub file: String,
    /// 1-based line number of the first occurrence in this file.
    pub line: usize,
    /// How convertible this usage is (see [`Classification`]).
    pub classification: Classification,
    /// The Tauri target for this API, if the mapping table knows one.
    pub tauri_target: Option<String>,
    /// One honest line about what the conversion involves.
    pub note: String,
}

/// What the scanner learned about the app's preload scripts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreloadInfo {
    /// Preload script paths (files named `preload.*`, plus any path
    /// referenced from a `BrowserWindow` `webPreferences.preload`).
    pub files: Vec<String>,
    /// Whether any file calls `contextBridge.exposeInMainWorld`.
    pub uses_context_bridge: bool,
    /// Keys exposed to the renderer via `contextBridge.exposeInMainWorld`.
    pub exposed_keys: Vec<String>,
}

/// One `new BrowserWindow({...})` call site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Window width in pixels (Electron default 800 when unspecified).
    pub width: u32,
    /// Window height in pixels (Electron default 600 when unspecified).
    pub height: u32,
    /// Window title, if the options set one.
    pub title: Option<String>,
    /// Whether the window is resizable (Electron default `true`).
    pub resizable: bool,
    /// Whether the window has a native frame (Electron default `true`).
    pub frame: bool,
    /// Source file, relative to the scanned app directory.
    pub file: String,
    /// 1-based line number of the `new BrowserWindow` call.
    pub line: usize,
}

/// The app's menu, from the first `Menu.buildFromTemplate(...)` call found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MenuInfo {
    /// `true` when the template argument is a static array literal
    /// (`[...]`), which the converter can translate directly; `false`
    /// when it is a variable or expression, which needs human review.
    pub is_static_literal: bool,
    /// The literal template source text, when static (capped at ~4 KiB).
    pub template_source: Option<String>,
    /// Source file, relative to the scanned app directory.
    pub file: String,
    /// 1-based line number of the `Menu.buildFromTemplate` call.
    pub line: usize,
}

/// The full result of scanning one Electron app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanReport {
    /// App name from `package.json` (falls back to the directory name).
    pub app_name: String,
    /// Electron version from `dependencies`/`devDependencies`, if declared.
    pub electron_version: Option<String>,
    /// Main-process entry point from `package.json` (`"main"`,
    /// default `"main.js"`).
    pub main_entry: Option<String>,
    /// Number of source files actually read and scanned.
    pub files_scanned: usize,
    /// IPC channels found, in scan order.
    pub ipc_channels: Vec<IpcChannel>,
    /// Electron API usages, deduplicated per (API, file).
    pub findings: Vec<Finding>,
    /// Preload script information.
    pub preload: PreloadInfo,
    /// Native (or otherwise port-needing) dependencies from `package.json`.
    pub native_modules: Vec<String>,
    /// Feasibility score, 0–100. Higher means a smoother conversion.
    pub feasibility_score: u8,
    /// Human-readable verdict derived from the score.
    pub verdict: String,
    /// Windows found via `new BrowserWindow({...})`.
    pub windows: Vec<WindowInfo>,
    /// Menu info from the first `Menu.buildFromTemplate` call, if any.
    pub menu: Option<MenuInfo>,
}
