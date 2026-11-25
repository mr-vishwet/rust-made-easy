//! Convert a scanned Electron app into a working Tauri 2 project.
//!
//! [`convert`] runs the scanner over an Electron app directory and writes a
//! fresh Tauri 2 project (Rust backend, TypeScript bridge, migration notes)
//! into the output directory. IPC handlers become compiling `TODO` stubs —
//! porting their bodies is the human's job (see `MIGRATION-NOTES.md`).
//!
//! The renderer is carried over when e2r can find it, in this order:
//! existing build output (`dist/` / `build/` / `out/` with an
//! `index.html`), then bundler source plus its preserved build script,
//! then static files at the app root. A placeholder `dist/index.html` is
//! generated only when no renderer files are found at all. `frontendDist`
//! always points at the output that is actually present, and `devUrl`
//! uses the app's real dev server port when it can be detected.
//!
//! The generated Rust is deliberately minimal and only uses stable Tauri 2
//! APIs, so `cargo check` passes on the generated `src-tauri` project.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::mappings;
use crate::model::{Classification, Finding, IpcChannel, MenuInfo, PreloadMemberKind, ScanReport};
use crate::scan::scan;

/// The result of converting one Electron app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvertReport {
    /// Files written into the output directory, relative to it, in write order.
    pub files_written: Vec<PathBuf>,
    /// Non-fatal problems the user should know about (skipped files,
    /// APIs that need a manual port, and so on).
    pub warnings: Vec<String>,
}

/// Convert the Electron app at `app_dir` into a Tauri 2 project at
/// `out_dir`.
///
/// Fails if `out_dir` already exists and is not empty. On success returns
/// the list of files written plus warnings about everything that still
/// needs a human (see `MIGRATION-NOTES.md` in the output for detail).
pub fn convert(app_dir: &Path, out_dir: &Path) -> Result<ConvertReport> {
    let report = scan(app_dir).context("failed to scan Electron app")?;

    if out_dir.exists() {
        if !out_dir.is_dir() {
            return Err(anyhow!(
                "output path {} exists and is not a directory",
                out_dir.display()
            ));
        }
        if fs::read_dir(out_dir)
            .context("failed to read output directory")?
            .next()
            .is_some()
        {
            return Err(anyhow!(
                "output directory {} already exists and is not empty; refusing to overwrite it",
                out_dir.display()
            ));
        }
    }
    fs::create_dir_all(out_dir).context("failed to create output directory")?;

    let pkg = sanitize_package_name(&report.app_name);
    let lib_snake = lib_crate_name(&pkg);
    let plugins = detect_plugins(
        &report.findings,
        &report.native_modules,
        &report.port_needing_deps,
    );

    // One Rust command per `ipcMain.handle` channel. Sanitized-name
    // collisions (e.g. "read-file" vs "read_file") keep the first channel
    // and warn about the rest.
    let mut handle_cmds: Vec<(IpcChannel, String)> = Vec::new();
    let mut seen_names: BTreeSet<String> = BTreeSet::new();
    let mut warnings: Vec<String> = Vec::new();
    for ch in report
        .ipc_channels
        .iter()
        .filter(|c| c.direction == "handle")
    {
        let name = sanitize_channel_name(&ch.name);
        if !seen_names.insert(name.clone()) {
            warnings.push(format!(
                "IPC channel '{}' ({}:{}) collides with another channel after sanitization; skipped (kept the first one)",
                ch.name, ch.file, ch.line
            ));
            continue;
        }
        handle_cmds.push((ch.clone(), name));
    }

    // One Rust event listener per `ipcMain.on` channel (direction "on";
    // `ipcRenderer.on` is direction "listen" and is a bridge concern, not
    // a Rust listener). Same-name channels keep the first and warn.
    let mut on_cmds: Vec<IpcChannel> = Vec::new();
    let mut seen_events: BTreeSet<String> = BTreeSet::new();
    for ch in report.ipc_channels.iter().filter(|c| c.direction == "on") {
        if !seen_events.insert(ch.name.clone()) {
            warnings.push(format!(
                "IPC channel '{}' ({}:{}) has more than one ipcMain.on handler; only the first gets a Rust listener stub",
                ch.name, ch.file, ch.line
            ));
            continue;
        }
        on_cmds.push(ch.clone());
    }

    // `nativeTheme` gets a real `set_theme` command (see gen_commands_rs).
    let has_theme = report.findings.iter().any(|f| f.api == "nativeTheme");

    // Renderer carry-over: detect what frontend the app actually has,
    // copy it into the generated project, and wire `tauri.conf.json` /
    // `package.json` to match. The copy runs before the generated files
    // are written, so a generated file (notably `src/lib/e2r-bridge.ts`)
    // wins on a name collision with a copied file.
    let mut files_written: Vec<PathBuf> = Vec::new();
    let pkg_json = read_package_json_value(app_dir);
    let mut renderer = detect_renderer(app_dir, &pkg_json, &report);
    let (copied_files, copied_bytes, skipped) =
        execute_renderer_plan(app_dir, out_dir, &renderer, &mut files_written)?;
    renderer.copied_files = copied_files;
    renderer.copied_bytes = copied_bytes;
    if copied_files > 0 {
        let noun = if copied_files == 1 { "file" } else { "files" };
        renderer.details.push(format!(
            "{copied_files} {noun} ({}) copied in total.",
            format_bytes(copied_bytes)
        ));
    }
    for entry in skipped {
        warnings.push(format!("Skipped while copying the renderer: {entry}"));
    }

    write_output_file(
        out_dir,
        &mut files_written,
        "src-tauri/Cargo.toml",
        &gen_cargo_toml(&pkg, &lib_snake, &plugins, report.app_version.as_deref()),
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        "src-tauri/build.rs",
        "fn main() {\n    tauri_build::build()\n}\n",
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        "src-tauri/tauri.conf.json",
        &gen_tauri_conf(&pkg, &report, &renderer)?,
    )?;
    // Tauri 2 denies plugin/core commands by default: the capability file
    // grants `core:default` plus the permissions for every included plugin.
    write_output_file(
        out_dir,
        &mut files_written,
        "src-tauri/capabilities/default.json",
        &gen_capabilities_json(&plugins, &report)?,
    )?;
    // Tauri's `generate_context!()` reads `icons/icon.png` at compile time,
    // so the project does not build without it. Prefer the app's real icon
    // when a BrowserWindow references a PNG that exists on disk; otherwise
    // fall back to the placeholder.
    let (icon_copied, icon_bytes) = find_real_icon(app_dir, &report);
    if let Some(icon_rel) = report.windows.iter().find_map(|w| w.icon.clone()) {
        if icon_copied.is_none() {
            warnings.push(format!(
                "Window icon '{icon_rel}' could not be used (not a PNG on disk); generated a placeholder src-tauri/icons/icon.png instead"
            ));
        }
    }
    match icon_bytes {
        Some(bytes) => write_output_file_bytes(
            out_dir,
            &mut files_written,
            "src-tauri/icons/icon.png",
            &bytes,
        )?,
        None => write_output_file_bytes(
            out_dir,
            &mut files_written,
            "src-tauri/icons/icon.png",
            &gen_placeholder_icon_png(),
        )?,
    }
    write_output_file(
        out_dir,
        &mut files_written,
        "src-tauri/src/main.rs",
        &gen_main_rs(&lib_snake),
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        "src-tauri/src/lib.rs",
        &gen_lib_rs(
            &handle_cmds,
            &plugins,
            report.menu.is_some(),
            !on_cmds.is_empty(),
            has_theme,
        ),
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        "src-tauri/src/commands.rs",
        &gen_commands_rs(&handle_cmds, has_theme),
    )?;
    if !on_cmds.is_empty() {
        write_output_file(
            out_dir,
            &mut files_written,
            "src-tauri/src/listeners.rs",
            &gen_listeners_rs(&on_cmds),
        )?;
    }
    if let Some(menu) = &report.menu {
        write_output_file(
            out_dir,
            &mut files_written,
            "src-tauri/src/menu.rs",
            &gen_menu_rs(menu),
        )?;
    }
    write_output_file(
        out_dir,
        &mut files_written,
        "src/lib/e2r-bridge.ts",
        &gen_bridge_ts(&report, &handle_cmds),
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        "package.json",
        &gen_package_json(&pkg, &renderer, report.app_version.as_deref())?,
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        "README.md",
        &gen_readme(&report, &renderer),
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        ".gitignore",
        "# Generated by electron-to-rust (e2r)\nnode_modules/\ndist/\nsrc-tauri/target/\n",
    )?;
    // Tauri's build script requires `frontendDist` to exist. When the
    // carried-over renderer already has an `index.html` there, nothing
    // more is needed; otherwise a stand-in keeps `cargo check` green
    // until the real frontend is built (Source) or copied in
    // (Placeholder).
    let frontend_index = PathBuf::from(&renderer.frontend_dir).join("index.html");
    if !out_dir.join(&frontend_index).is_file() {
        let text = if renderer.kind == RendererKind::Source {
            gen_build_standin_html(&renderer.before_build_command)
        } else {
            DIST_PLACEHOLDER.to_string()
        };
        write_output_file(
            out_dir,
            &mut files_written,
            &frontend_index.to_string_lossy(),
            &text,
        )?;
    }
    write_output_file(
        out_dir,
        &mut files_written,
        "MIGRATION-NOTES.md",
        &gen_migration_notes(
            &report,
            &plugins,
            &handle_cmds,
            &on_cmds,
            &renderer,
            icon_copied.as_deref(),
        ),
    )?;

    // Warnings: manual ports, native modules, and anything skipped.
    for f in report
        .findings
        .iter()
        .filter(|f| f.classification == Classification::Manual)
    {
        if f.note.is_empty() {
            warnings.push(format!(
                "Manual port needed: `{}` ({}:{})",
                f.api, f.file, f.line
            ));
        } else {
            warnings.push(format!(
                "Manual port needed: `{}` ({}:{}) — {}",
                f.api, f.file, f.line, f.note
            ));
        }
    }
    for m in &report.native_modules {
        warnings.push(format!(
            "Native module '{m}' has no Tauri equivalent; find a Rust crate or rewrite it"
        ));
    }
    for m in &report.port_needing_deps {
        match mappings::lookup(m) {
            Some(mapping) => match &mapping.plugin {
                Some(plugin) => warnings.push(format!(
                    "Dependency '{m}' is covered by {plugin} (included); port its calls to the plugin API"
                )),
                None => warnings.push(format!(
                    "Dependency '{m}' has no Tauri equivalent; port its calls to invoke-based Rust commands"
                )),
            },
            None => warnings.push(format!(
                "Dependency '{m}' needs a port; no mapping for it yet — review manually"
            )),
        }
    }
    for w in &report.windows {
        if !w.width_known || !w.height_known {
            warnings.push(format!(
                "Window at {}:{} has dynamic dimensions that could not be resolved; width/height were omitted from tauri.conf.json (Tauri defaults to 800x600) — set the real size by hand",
                w.file, w.line
            ));
        }
    }
    if let Some(menu) = &report.menu {
        if !menu.is_static_literal {
            warnings.push(format!(
                "Menu template at {}:{} is built dynamically; the generated menu.rs is a stub — port it by hand",
                menu.file, menu.line
            ));
        }
    }
    if report.windows.is_empty() {
        warnings.push(
            "No BrowserWindow found in the scanned app; generated a default 800x600 window"
                .to_string(),
        );
    }
    for caveat in &renderer.caveats {
        warnings.push(caveat.clone());
    }
    if renderer.kind == RendererKind::Placeholder {
        warnings.push(
            "No renderer files found in the scanned app; generated a placeholder dist/index.html (see MIGRATION-NOTES.md)"
                .to_string(),
        );
    }

    // Decision registry: one entry per Electron API finding the
    // conversion acted on, appended to `<out-dir>/.e2r/decisions.jsonl`
    // (see `crate::decisions`). The log is a metadata sidecar — it is
    // deliberately NOT listed in `files_written` — and writing it must
    // never fail the conversion: on error, record a warning and carry on.
    let decisions_path = crate::decisions::default_path(Some(out_dir));
    let entries = crate::decisions::entries_from_findings(&report.findings);
    if let Err(err) = crate::decisions::append(&decisions_path, &entries) {
        warnings.push(format!(
            "Failed to write decision log {}: {err:#}",
            decisions_path.display()
        ));
    }

    Ok(ConvertReport {
        files_written,
        warnings,
    })
}

/// Write one generated file, creating parent directories, and record its
/// path (relative to `out_dir`) in `files_written`.
fn write_output_file(
    out_dir: &Path,
    files_written: &mut Vec<PathBuf>,
    rel: &str,
    contents: &str,
) -> Result<()> {
    write_output_file_bytes(out_dir, files_written, rel, contents.as_bytes())
}

/// Byte-oriented variant of [`write_output_file`] (for the icon PNG).
fn write_output_file_bytes(
    out_dir: &Path,
    files_written: &mut Vec<PathBuf>,
    rel: &str,
    contents: &[u8],
) -> Result<()> {
    let path = out_dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory for {rel}"))?;
    }
    fs::write(&path, contents).with_context(|| format!("failed to write {rel}"))?;
    files_written.push(PathBuf::from(rel));
    Ok(())
}

/// Placeholder frontend, written only when no renderer files were found
/// in the scanned app at all: Tauri's build script requires
/// `build.frontendDist` to exist, so `cargo check` needs something there.
const DIST_PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>Placeholder — generated by electron-to-rust</title>
  </head>
  <body>
    <h1>Placeholder frontend</h1>
    <p>
      No renderer files were found in the scanned Electron app (no
      <code>dist/</code>, <code>build/</code>, or <code>out/</code> build
      output, no renderer source directory, and no root
      <code>index.html</code>), so electron-to-rust generated this
      placeholder. Replace it with your real frontend: copy your renderer
      source into <code>src/</code> (or point
      <code>build.frontendDist</code> in
      <code>src-tauri/tauri.conf.json</code> at your existing build output)
      and delete this file. See <code>MIGRATION-NOTES.md</code>.
    </p>
  </body>
</html>
"#;

/// Stand-in `index.html` for the frontend output directory when the
/// renderer source was carried over but not built yet: Tauri's build
/// script requires `frontendDist` to exist, so this keeps `cargo check`
/// green until the first real frontend build replaces it. `build_command`
/// is the `beforeBuildCommand` (e.g. `npm run build:frontend`), or empty
/// when the original app had no build script.
fn gen_build_standin_html(build_command: &str) -> String {
    let (instruction, auto_note) = if build_command.is_empty() {
        (
            "Add a build script to <code>package.json</code> (as <code>build:frontend</code>) and run it"
                .to_string(),
            String::new(),
        )
    } else {
        (
            format!("Run <code>npm install &amp;&amp; {build_command}</code>"),
            " (<code>tauri build</code> runs the frontend build automatically via <code>beforeBuildCommand</code>.)"
                .to_string(),
        )
    };
    format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>Build the frontend first — generated by electron-to-rust</title>
  </head>
  <body>
    <h1>Build the frontend first</h1>
    <p>
      electron-to-rust copied the renderer source into this project, but
      the frontend has not been built yet. {instruction} to build it —
      the real output replaces this file.{auto_note} See
      <code>MIGRATION-NOTES.md</code>.
    </p>
  </body>
</html>
"#,
    )
}

/// Build a minimal 128×128 RGBA PNG (placeholder app icon) with no image
/// dependency: the pixel data is stored uncompressed (zlib "stored" DEFLATE
/// blocks) with hand-rolled CRC-32 (IEEE) and Adler-32. A dark square with
/// a light inset square — clearly a placeholder, valid everywhere a real
/// icon is expected. Replace it with the app's real icon.
fn gen_placeholder_icon_png() -> Vec<u8> {
    const SIZE: usize = 128;
    const BG: [u8; 4] = [0x1e, 0x1e, 0x2e, 0xff];
    const FG: [u8; 4] = [0xf5, 0xf5, 0xf5, 0xff];

    // Raw scanlines: one filter byte (0 = None) + SIZE RGBA pixels per row.
    let mut raw = Vec::with_capacity(SIZE * (1 + SIZE * 4));
    for y in 0..SIZE {
        raw.push(0u8);
        for x in 0..SIZE {
            let in_square = (32..96).contains(&x) && (32..96).contains(&y);
            raw.extend_from_slice(if in_square { &FG } else { &BG });
        }
    }

    // zlib stream: header 0x78 0x01 (no/low compression) + stored blocks
    // (max 65_535 bytes each) + Adler-32 of the raw data.
    let mut zlib = vec![0x78, 0x01];
    let chunks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let last = i + 1 == chunks.len();
        zlib.push(if last { 0x01 } else { 0x00 }); // BFINAL + BTYPE=00
        let len = chunk.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(chunk);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    // PNG: signature + IHDR (8-bit RGBA) + IDAT + IEND.
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(SIZE as u32).to_be_bytes());
    ihdr.extend_from_slice(&(SIZE as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth 8, color type 6 (RGBA)
    push_png_chunk(&mut png, b"IHDR", &ihdr);
    push_png_chunk(&mut png, b"IDAT", &zlib);
    push_png_chunk(&mut png, b"IEND", &[]);
    png
}

/// Append one PNG chunk (length + type + data + CRC-32 of type+data).
fn push_png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    png.extend_from_slice(&crc32_ieee(&crc_input).to_be_bytes());
}

/// CRC-32 (IEEE 802.3, polynomial 0xEDB88320), bitwise — fast enough for a
/// ~66 KiB icon.
fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Adler-32, as used by zlib.
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

/// Tauri plugins to include, decided from the Electron APIs the scan found.
/// Only plugins whose Electron API actually appears in the findings are
/// included.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Plugins {
    opener: bool,
    dialog: bool,
    store: bool,
    updater: bool,
    notification: bool,
    single_instance: bool,
    deep_link: bool,
    window_state: bool,
    global_shortcut: bool,
    clipboard: bool,
}

impl Plugins {
    /// Cargo dependency names (e.g. `tauri-plugin-dialog`), in a fixed order.
    fn cargo_deps(&self) -> Vec<&'static str> {
        let mut deps = Vec::new();
        if self.clipboard {
            deps.push("tauri-plugin-clipboard-manager");
        }
        if self.deep_link {
            deps.push("tauri-plugin-deep-link");
        }
        if self.dialog {
            deps.push("tauri-plugin-dialog");
        }
        if self.global_shortcut {
            deps.push("tauri-plugin-global-shortcut");
        }
        if self.notification {
            deps.push("tauri-plugin-notification");
        }
        if self.opener {
            deps.push("tauri-plugin-opener");
        }
        if self.single_instance {
            deps.push("tauri-plugin-single-instance");
        }
        if self.store {
            deps.push("tauri-plugin-store");
        }
        if self.updater {
            deps.push("tauri-plugin-updater");
        }
        if self.window_state {
            deps.push("tauri-plugin-window-state");
        }
        deps
    }

    /// `.plugin(...)` expressions for `lib.rs`, in the same order as
    /// [`Plugins::cargo_deps`].
    fn init_exprs(&self) -> Vec<&'static str> {
        let mut exprs = Vec::new();
        if self.clipboard {
            exprs.push("tauri_plugin_clipboard_manager::init()");
        }
        if self.deep_link {
            exprs.push("tauri_plugin_deep_link::init()");
        }
        if self.dialog {
            exprs.push("tauri_plugin_dialog::init()");
        }
        if self.global_shortcut {
            exprs.push("tauri_plugin_global_shortcut::Builder::new().build()");
        }
        if self.notification {
            exprs.push("tauri_plugin_notification::init()");
        }
        if self.opener {
            exprs.push("tauri_plugin_opener::init()");
        }
        if self.single_instance {
            exprs.push("tauri_plugin_single_instance::init(|_app, _args, _cwd| {})");
        }
        if self.store {
            exprs.push("tauri_plugin_store::Builder::default().build()");
        }
        if self.updater {
            exprs.push("tauri_plugin_updater::Builder::new().build()");
        }
        if self.window_state {
            exprs.push("tauri_plugin_window_state::Builder::default().build()");
        }
        exprs
    }

    /// Capability permission identifiers for the included plugins, in the
    /// same order as [`Plugins::cargo_deps`] (each plugin's `:default`
    /// permission set). `core:default` is added separately by the
    /// capability generator — it is not a plugin permission.
    ///
    /// `single_instance` is deliberately absent: `tauri-plugin-single-instance`
    /// exposes no frontend commands, so it has no permission set at all, and
    /// naming `single-instance:default` in a capability makes the Tauri build
    /// script fail ("Permission single-instance:default not found"). Proven by
    /// the Simplenote battle-test re-run, 2026-11-25.
    fn capability_permissions(&self) -> Vec<&'static str> {
        let mut permissions = Vec::new();
        if self.clipboard {
            permissions.push("clipboard-manager:default");
        }
        if self.deep_link {
            permissions.push("deep-link:default");
        }
        if self.dialog {
            permissions.push("dialog:default");
        }
        if self.global_shortcut {
            permissions.push("global-shortcut:default");
        }
        if self.notification {
            permissions.push("notification:default");
        }
        if self.opener {
            permissions.push("opener:default");
        }
        // No single-instance entry — see the doc comment above.
        if self.store {
            permissions.push("store:default");
        }
        if self.updater {
            permissions.push("updater:default");
        }
        if self.window_state {
            permissions.push("window-state:default");
        }
        permissions
    }
}

/// Decide which Tauri plugins to include from the scan findings and the
/// dependency lists. Matching is deliberately explicit: a plugin is
/// included only when its Electron API appears in the findings, or when
/// its pure-JS package appears in `port_needing_deps`
/// (`electron-store` / `electron-updater` / `electron-window-state`).
///
/// `Tray` is intentionally NOT mapped here: the mapping table names the
/// Tauri v1 `tauri-plugin-system-tray`, which does not exist for Tauri 2
/// (tray is core `tauri::tray` there) — including it would break the
/// generated build.
fn detect_plugins(
    findings: &[Finding],
    native_modules: &[String],
    port_needing_deps: &[String],
) -> Plugins {
    let mut plugins = Plugins::default();
    for f in findings {
        match f.api.as_str() {
            "shell.openExternal" | "shell.openPath" | "shell" => plugins.opener = true,
            "dialog.showOpenDialog"
            | "dialog.showSaveDialog"
            | "dialog.showMessageBox"
            | "dialog" => plugins.dialog = true,
            "electron-store" => plugins.store = true,
            "autoUpdater" | "electron-updater" => plugins.updater = true,
            "Notification" => plugins.notification = true,
            "globalShortcut" => plugins.global_shortcut = true,
            "clipboard" => plugins.clipboard = true,
            "app.requestSingleInstanceLock" => plugins.single_instance = true,
            "app.setAsDefaultProtocolClient" => plugins.deep_link = true,
            "electron-window-state" => plugins.window_state = true,
            _ => {}
        }
    }
    // Note: `native_modules` is deliberately not consulted here — native
    // addons have no plugin equivalent, so they never drive inclusion.
    let _ = native_modules;
    for m in port_needing_deps {
        match m.as_str() {
            "electron-store" => plugins.store = true,
            "electron-updater" => plugins.updater = true,
            "electron-window-state" => plugins.window_state = true,
            _ => {}
        }
    }
    plugins
}

/// The app's version as a Cargo-valid semver string: the real version
/// from `package.json` when it parses as semver (`1.0` is padded to
/// `1.0.0`), `0.1.0` otherwise. A non-semver version in `Cargo.toml`
/// would break the generated build, so anything unparseable falls back
/// instead of being copied verbatim.
pub fn sanitize_version(version: Option<&str>) -> String {
    const FALLBACK: &str = "0.1.0";
    let Some(raw) = version.map(str::trim).filter(|v| !v.is_empty()) else {
        return FALLBACK.to_string();
    };
    // Split off any pre-release / build metadata (`-beta.1`, `+build`).
    let (core, suffix) = match raw.find(['-', '+']) {
        Some(idx) => (&raw[..idx], &raw[idx..]),
        None => (raw, ""),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() > 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return FALLBACK.to_string();
    }
    let mut numbers: Vec<&str> = parts;
    while numbers.len() < 3 {
        numbers.push("0");
    }
    format!("{}{}", numbers.join("."), suffix)
}

/// The app's real icon, when a `BrowserWindow` references a PNG that
/// exists on disk: returns (app-relative path, PNG bytes). `None` when no
/// window references an icon, the file is missing, or it is not a PNG
/// (Tauri's `icons/icon.png` must be a PNG; `.ico`/`.icns` build icons
/// cannot be converted without image tooling).
fn find_real_icon(app_dir: &Path, report: &ScanReport) -> (Option<String>, Option<Vec<u8>>) {
    const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    for w in &report.windows {
        let Some(icon_rel) = &w.icon else {
            continue;
        };
        if !icon_rel.to_lowercase().ends_with(".png") {
            continue;
        }
        if let Ok(bytes) = fs::read(app_dir.join(icon_rel)) {
            if bytes.starts_with(PNG_MAGIC) {
                return (Some(icon_rel.clone()), Some(bytes));
            }
        }
    }
    (None, None)
}

/// Window labels for `tauri.conf.json` / capabilities: the first window is
/// `main`, the rest `window-2`, `window-3`, ...; a single `main` when the
/// scan found no windows at all.
fn window_labels(report: &ScanReport) -> Vec<String> {
    if report.windows.is_empty() {
        return vec!["main".to_string()];
    }
    report
        .windows
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if i == 0 {
                "main".to_string()
            } else {
                format!("window-{}", i + 1)
            }
        })
        .collect()
}

/// Whether a `backgroundColor` value is safe to carry into
/// `tauri.conf.json`: Tauri parses hex colors (`#rgb`, `#rgba`, `#rrggbb`,
/// `#rrggbbaa`) but not named colors (`white`) — a named color would fail
/// config parsing and break the generated build.
fn is_hex_color(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `src-tauri/Cargo.toml`: package, lib + bin targets, `tauri-build`, and
/// only the plugins the scan showed are used. The version is the app's
/// real version from `package.json` (sanitized to semver), `0.1.0` when
/// the app declares none.
fn gen_cargo_toml(pkg: &str, lib_snake: &str, plugins: &Plugins, version: Option<&str>) -> String {
    let version = sanitize_version(version);
    let mut out = format!(
        "[package]\nname = \"{pkg}\"\nversion = \"{version}\"\ndescription = \"Generated by electron-to-rust (e2r) from an Electron app\"\nedition = \"2021\"\n\n[lib]\nname = \"{lib_snake}\"\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"{pkg}\"\npath = \"src/main.rs\"\n\n# The generated project is its own workspace, so it builds standalone even\n# when the output lands inside another Cargo workspace.\n[workspace]\n\n[build-dependencies]\ntauri-build = {{ version = \"2\", features = [] }}\n\n[dependencies]\ntauri = {{ version = \"2\", features = [] }}\nserde = {{ version = \"1\", features = [\"derive\"] }}\nserde_json = \"1\"\n"
    );
    for dep in plugins.cargo_deps() {
        out.push_str(&format!("{dep} = \"2\"\n"));
    }
    out
}

/// `src-tauri/capabilities/default.json`: the default Tauri 2 capability,
/// granting `core:default` plus the `:default` permission set of every
/// plugin the project includes, scoped to the generated windows. Without
/// this file every plugin call from the frontend is denied at runtime —
/// the project compiles and launches, then fails on first use.
fn gen_capabilities_json(plugins: &Plugins, report: &ScanReport) -> Result<String> {
    let mut permissions = vec!["core:default".to_string()];
    permissions.extend(
        plugins
            .capability_permissions()
            .iter()
            .map(|p| p.to_string()),
    );
    let cap = serde_json::json!({
        "identifier": "default",
        "description": "Default capability generated by electron-to-rust (e2r): grants the core commands and the permissions for every plugin this project includes.",
        "windows": window_labels(report),
        "permissions": permissions,
    });
    Ok(serde_json::to_string_pretty(&cap)?)
}

/// `src-tauri/tauri.conf.json`: minimal but schema-valid Tauri 2 config.
/// Every window from the scan goes in `app.windows` (the first labelled
/// `main`, the rest `window-2`, `window-3`, ...); with no windows at all a
/// single default 800×600 window is emitted.
///
/// A window dimension the scanner could not resolve is OMITTED (Tauri
/// defaults to 800×600) rather than emitted as a confident wrong number;
/// literal `minWidth`/`minHeight` and hex `backgroundColor` are carried
/// through. Product name, version, and bundle id are the app's real ones
/// when declared, falling back to the previous defaults only when absent.
///
/// `frontendDist` points at the carried-over renderer output, `devUrl` at
/// the app's detected dev server (or `null` when the app has no dev
/// server, so `tauri dev` serves `frontendDist`), and the
/// before-dev/build commands run the preserved frontend scripts.
///
/// Note: Electron's `frame` option maps to Tauri's `decorations` — there is
/// no `frame` key in the Tauri 2 window config.
fn gen_tauri_conf(pkg: &str, report: &ScanReport, plan: &RendererPlan) -> Result<String> {
    let mut windows = Vec::new();
    if report.windows.is_empty() {
        windows.push(serde_json::json!({
            "label": "main",
            "width": 800,
            "height": 600,
            "resizable": true,
            "decorations": true
        }));
    } else {
        for (i, w) in report.windows.iter().enumerate() {
            let mut win = serde_json::Map::new();
            let label = if i == 0 {
                "main".to_string()
            } else {
                format!("window-{}", i + 1)
            };
            win.insert("label".to_string(), serde_json::Value::String(label));
            if let Some(title) = &w.title {
                win.insert(
                    "title".to_string(),
                    serde_json::Value::String(title.clone()),
                );
            }
            // Unknown dimensions are omitted, not faked (see doc comment).
            if w.width_known {
                win.insert("width".to_string(), serde_json::json!(w.width));
            }
            if w.height_known {
                win.insert("height".to_string(), serde_json::json!(w.height));
            }
            if let Some(min_width) = w.min_width {
                win.insert("minWidth".to_string(), serde_json::json!(min_width));
            }
            if let Some(min_height) = w.min_height {
                win.insert("minHeight".to_string(), serde_json::json!(min_height));
            }
            if let Some(bg) = &w.background_color {
                if is_hex_color(bg) {
                    win.insert(
                        "backgroundColor".to_string(),
                        serde_json::Value::String(bg.clone()),
                    );
                }
            }
            win.insert(
                "resizable".to_string(),
                serde_json::Value::Bool(w.resizable),
            );
            win.insert("decorations".to_string(), serde_json::Value::Bool(w.frame));
            windows.push(serde_json::Value::Object(win));
        }
    }
    let product_name = report
        .product_name
        .clone()
        .unwrap_or_else(|| report.app_name.clone());
    let identifier = report
        .app_id
        .clone()
        .unwrap_or_else(|| format!("com.e2r.{pkg}"));
    let conf = serde_json::json!({
        "$schema": "https://schema.tauri.app/config/2",
        "productName": product_name,
        "version": sanitize_version(report.app_version.as_deref()),
        "identifier": identifier,
        "build": {
            "frontendDist": plan.frontend_dist,
            "devUrl": plan.dev_url,
            "beforeDevCommand": plan.before_dev_command,
            "beforeBuildCommand": plan.before_build_command
        },
        "app": {
            "windows": windows,
            "security": { "csp": null }
        },
        "bundle": {
            "active": true,
            "targets": ["deb", "appimage", "msi", "nsis", "dmg", "app"]
        }
    });
    Ok(serde_json::to_string_pretty(&conf)?)
}

/// `src-tauri/src/main.rs`: the standard Tauri 2 entry point, delegating to
/// the library's `run()`.
fn gen_main_rs(lib_snake: &str) -> String {
    format!(
        "#![cfg_attr(not(debug_assertions), windows_subsystem = \"windows\")]\n\nfn main() {{\n    {lib_snake}::run();\n}}\n"
    )
}

/// `src-tauri/src/lib.rs`: the Tauri builder with plugins, commands, and
/// (when the app has a menu) the menu wiring, plus (when the app has
/// `ipcMain.on` channels) the event-listener registration.
///
/// All windows already live in `tauri.conf.json`, so `setup` only wires
/// listeners and the menu — it does not create extra windows (that would
/// duplicate labels).
fn gen_lib_rs(
    handle_cmds: &[(IpcChannel, String)],
    plugins: &Plugins,
    has_menu: bool,
    has_listeners: bool,
    has_theme_cmd: bool,
) -> String {
    let mut out = String::from(
        "//! Tauri application entry point — generated by electron-to-rust (e2r).\n\n",
    );
    out.push_str("mod commands;\n");
    if has_listeners {
        out.push_str("mod listeners;\n");
    }
    if has_menu {
        out.push_str("mod menu;\n");
    }
    out.push_str(
        "\n#[cfg_attr(mobile, tauri::mobile_entry_point)]\npub fn run() {\n    tauri::Builder::default()\n",
    );
    for expr in plugins.init_exprs() {
        out.push_str("        .plugin(");
        out.push_str(expr);
        out.push_str(")\n");
    }
    let mut command_names: Vec<String> = handle_cmds
        .iter()
        .map(|(_, name)| format!("commands::{name}"))
        .collect();
    if has_theme_cmd {
        command_names.push("commands::set_theme".to_string());
    }
    if !command_names.is_empty() {
        out.push_str("        .invoke_handler(tauri::generate_handler![");
        out.push_str(&command_names.join(", "));
        out.push_str("])\n");
    }
    if has_menu || has_listeners {
        out.push_str("        .setup(|app| {\n");
        if has_listeners {
            out.push_str("            listeners::register_listeners(app);\n");
        }
        if has_menu {
            out.push_str(
                "            let menu = menu::build_menu(app.handle())?;\n            app.set_menu(menu)?;\n",
            );
        }
        out.push_str("            Ok(())\n        })\n");
    }
    out.push_str(
        "        .run(tauri::generate_context!())\n        .expect(\"error while running tauri application\");\n}\n",
    );
    out
}

/// `src-tauri/src/commands.rs`: one `#[tauri::command]` stub per
/// `ipcMain.handle` channel. Every stub compiles and returns a `TODO` error
/// until the original handler is ported. With no channels the file is a
/// comment only, and `lib.rs` omits `invoke_handler` entirely.
///
/// When the app uses `nativeTheme`, a real `set_theme` command is added:
/// it maps `"dark"` / `"light"` / `"system"` onto the Tauri window theme
/// API, so the renderer can drive the theme the original app set via
/// `nativeTheme.themeSource` (wiring the call sites is still a TODO — see
/// `MIGRATION-NOTES.md`).
fn gen_commands_rs(handle_cmds: &[(IpcChannel, String)], has_theme_cmd: bool) -> String {
    let mut out = String::from(
        "//! Tauri commands generated by electron-to-rust (e2r) from Electron\n//! `ipcMain.handle` channels. Each command is a `TODO` stub: port the\n//! original Electron main-process handler into the function body.\n",
    );
    if handle_cmds.is_empty() && !has_theme_cmd {
        out.push_str(
            "\n// No `ipcMain.handle` channels were found in the scanned app, so there\n// are no commands to generate.\n",
        );
        return out;
    }
    for (ch, name) in handle_cmds {
        out.push_str(&format!(
            "\n/// Channel `{}` (original: {}:{}).\n///\n/// TODO(e2r): port the Electron main-process handler for this channel.\n#[tauri::command]\npub async fn {name}(payload: Option<serde_json::Value>) -> Result<serde_json::Value, String> {{\n    let _ = payload;\n    Err(format!(\"TODO(e2r): port handler for channel '{{}}' — original in Electron main process\", \"{esc}\"))\n}}\n",
            comment_safe(&ch.name),
            ch.file,
            ch.line,
            esc = rust_str_escape(&ch.name),
        ));
    }
    if has_theme_cmd {
        out.push_str(
            "\n/// Set the window theme (original: `nativeTheme.themeSource`).\n///\n/// `theme` is `\"dark\"`, `\"light\"`, or `\"system\"` (follow the OS).\n/// TODO(e2r): the original app set the theme from its settings on state\n/// changes — call this command (via `api.setTheme` in the bridge) from\n/// the same places.\n#[tauri::command]\npub fn set_theme(window: tauri::Window, theme: String) -> Result<(), String> {\n    let parsed = match theme.as_str() {\n        \"dark\" => Some(tauri::Theme::Dark),\n        \"light\" => Some(tauri::Theme::Light),\n        \"system\" | \"auto\" => None,\n        other => {\n            return Err(format!(\n                \"unknown theme {other:?} — expected 'dark', 'light', or 'system'\"\n            ))\n        }\n    };\n    window.set_theme(parsed).map_err(|e| e.to_string())\n}\n",
        );
    }
    out
}

/// `src-tauri/src/listeners.rs`: one `app.listen` stub per `ipcMain.on`
/// channel. Without these, every renderer → main event the bridge emits
/// would go nowhere — the bridge's `emit()` calls need a Rust-side
/// receiver, and this file is it. Each listener is a `TODO` stub naming
/// the original handler's location; porting the bodies is the human's job.
fn gen_listeners_rs(on_cmds: &[IpcChannel]) -> String {
    let mut out = String::from(
        "//! Event listeners generated by electron-to-rust (e2r) from Electron\n//! `ipcMain.on` channels. Each listener is a `TODO` stub: port the\n//! original Electron main-process handler into the closure body.\n\nuse tauri::Listener;\n\n/// Register one `app.listen` stub per `ipcMain.on` channel from the\n/// original Electron app. Called from `lib.rs` during setup.\npub fn register_listeners(app: &tauri::App) {\n",
    );
    for ch in on_cmds {
        out.push_str(&format!(
            "    // Channel `{}` (original: {}:{}).\n    //\n    // TODO(e2r): port the Electron main-process handler for this channel.\n    let _ = app.listen(\"{}\", |_event| {{\n        // TODO(e2r): port the handler for `{}` here.\n    }});\n",
            comment_safe(&ch.name),
            ch.file,
            ch.line,
            rust_str_escape(&ch.name),
            comment_safe(&ch.name),
        ));
    }
    out.push_str("}\n");
    out
}

/// `src-tauri/src/menu.rs`: a `build_menu` stub. When the Electron template
/// was a static literal its source is kept as a doc comment so the port has
/// the original to work from; dynamic templates get a `TODO` with the
/// source location.
fn gen_menu_rs(menu: &MenuInfo) -> String {
    let mut out =
        String::from("//! Application menu — generated by electron-to-rust (e2r).\n//!\n");
    if menu.is_static_literal {
        match &menu.template_source {
            Some(src) => {
                out.push_str(&format!(
                    "//! Original Electron template (static literal from {}:{}):\n",
                    menu.file, menu.line
                ));
                for line in src.lines() {
                    out.push_str("//! ");
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str(
                    "//!\n//! TODO(e2r): port the template items below with `tauri::menu` builders.\n",
                );
            }
            None => {
                out.push_str(&format!(
                    "//! The Electron template at {}:{} is a static literal, but its source\n//! was not captured. TODO(e2r): port it by hand from the original file.\n",
                    menu.file, menu.line
                ));
            }
        }
    } else {
        out.push_str(&format!(
            "//! The Electron menu template at {}:{} is built dynamically, so e2r\n//! could not lift it verbatim. TODO(e2r): port it by hand.\n",
            menu.file, menu.line
        ));
    }
    out.push_str(
        "\n/// Build the application menu.\n///\n/// TODO(e2r): this is a minimal placeholder — add `MenuItem`/`Submenu`\n/// entries mirroring the original Electron template (see module docs).\npub fn build_menu(app: &tauri::AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {\n    tauri::menu::Menu::new(app)\n}\n",
    );
    out
}

/// `src/lib/e2r-bridge.ts`: re-implements the preload's `window.api` surface
/// over `@tauri-apps/api`.
///
/// Direction mapping (see `model::IpcChannel`):
/// - `"handle"` channels become `invoke` wrappers (request/response).
/// - `"on"` channels (`ipcMain.on`) become `emit` wrappers; the Rust side
///   has a matching listener stub in `src-tauri/src/listeners.rs`.
/// - `"send"` channels that match a `"handle"` channel are the renderer's
///   `ipcRenderer.invoke` side of the same channel, already covered by the
///   `invoke` wrapper, so they are skipped; other `"send"` channels become
///   `emit` wrappers.
/// - `"listen"` channels (`ipcRenderer.on`) and `webContents.send` channels
///   (main → renderer) become `listen` wrappers in the `mainEvents` export.
///
/// Preload keys other than `api` become typed objects derived from the
/// recovered preload shape — never a generic `Proxy`: a proxy would invent
/// commands that do not exist and turn the preload's booleans (e.g.
/// `isMac`) into functions. When the shape could not be recovered
/// statically, no wrapper is emitted at all — just a `TODO` comment.
fn gen_bridge_ts(report: &ScanReport, handle_cmds: &[(IpcChannel, String)]) -> String {
    let has_theme = report.findings.iter().any(|f| f.api == "nativeTheme");
    let needs_platform_import = report.preload.shapes.iter().any(|s| {
        s.members.iter().any(|m| {
            matches!(
                m.kind,
                PreloadMemberKind::PlatformMac
                    | PreloadMemberKind::PlatformLinux
                    | PreloadMemberKind::PlatformWindows
            )
        })
    });
    let mut out = String::from(
        "/**\n * Generated by electron-to-rust (e2r).\n *\n * This module replaces the Electron preload script: instead of\n * `contextBridge.exposeInMainWorld`, the renderer talks to the Rust backend\n * through `@tauri-apps/api`.\n *\n * Before (Electron):\n *   window.api.readFile(path)\n * After (Tauri):\n *   import { api } from \"./lib/e2r-bridge\";\n *   api.readFile(path)\n */\n",
    );
    if !report.preload.files.is_empty() {
        out.push_str(&format!(
            "// Replaces preload script(s): {}\n",
            report.preload.files.join(", ")
        ));
    }
    out.push_str(
        "\nimport { invoke } from \"@tauri-apps/api/core\";\nimport { emit, listen } from \"@tauri-apps/api/event\";\n",
    );
    if needs_platform_import {
        out.push_str("import { platform } from \"@tauri-apps/api/os\";\n");
    }
    out.push_str(
        "\n/** Low-level Tauri primitives, re-exported for hand-written wrappers. */\nexport const e2rApi = { invoke, listen, emit };\n",
    );

    // JS keys must be unique across all channel kinds.
    let mut used_keys: BTreeSet<String> = BTreeSet::new();
    let mut entries: Vec<String> = Vec::new();
    for (ch, rust_name) in handle_cmds {
        let key = to_camel_case(&ch.name);
        if !used_keys.insert(key.clone()) {
            continue;
        }
        entries.push(format!(
            "  /** Channel `{}` (original: {}:{}). */\n  {}: (payload?: unknown) => invoke(\"{}\", {{ payload }}),",
            comment_safe(&ch.name),
            ch.file,
            ch.line,
            key,
            rust_str_escape(rust_name),
        ));
    }
    for ch in report.ipc_channels.iter().filter(|c| c.direction == "on") {
        let key = to_camel_case(&ch.name);
        if !used_keys.insert(key.clone()) {
            continue;
        }
        entries.push(format!(
            "  /** Send-only channel `{}` (original: {}:{}). The Rust side has a listener stub for it in `src-tauri/src/listeners.rs` — port the original handler there. */\n  {}: (payload?: unknown) => emit(\"{}\", payload),",
            comment_safe(&ch.name),
            ch.file,
            ch.line,
            key,
            rust_str_escape(&ch.name),
        ));
    }
    let handle_names: BTreeSet<&str> = handle_cmds.iter().map(|(ch, _)| ch.name.as_str()).collect();
    for ch in report.ipc_channels.iter().filter(|c| c.direction == "send") {
        // The renderer's `ipcRenderer.invoke` side of a handled channel:
        // already covered by the `invoke` wrapper above.
        if handle_names.contains(ch.name.as_str()) {
            continue;
        }
        let key = to_camel_case(&ch.name);
        if !used_keys.insert(key.clone()) {
            continue;
        }
        entries.push(format!(
            "  /** Renderer-side channel `{}` (original: {}:{}). TODO(e2r): check the original call site — if this is a main-to-renderer event, use `mainEvents` (`listen`) instead of `emit`. */\n  {}: (payload?: unknown) => emit(\"{}\", payload),",
            comment_safe(&ch.name),
            ch.file,
            ch.line,
            key,
            rust_str_escape(&ch.name),
        ));
    }
    if has_theme && used_keys.insert("setTheme".to_string()) {
        entries.push(
            "  /** Set the window theme: `\"dark\"`, `\"light\"`, or `\"system\"` (original: `nativeTheme.themeSource`). */\n  setTheme: (theme: string) => invoke(\"set_theme\", { theme }),"
                .to_string(),
        );
    }

    out.push_str(
        "\n/**\n * Typed wrappers for the converted IPC channels.\n * Drop-in replacement for the `window.api` object the preload exposed.\n */\nexport const api = {\n",
    );
    if entries.is_empty() {
        out.push_str("  // No IPC channels were found in the scanned app.\n");
    } else {
        for entry in entries {
            out.push_str(&entry);
            out.push('\n');
        }
    }
    out.push_str("};\n");

    // Main → renderer events: `ipcRenderer.on` subscriptions plus
    // `webContents.send` call sites, deduped by channel name. Each wrapper
    // returns the `listen` unlisten function.
    let mut seen_events: BTreeSet<&str> = BTreeSet::new();
    let mut main_events: Vec<(&str, &str, usize)> = Vec::new();
    for ch in report
        .ipc_channels
        .iter()
        .filter(|c| c.direction == "listen")
        .chain(report.main_to_renderer.iter())
    {
        if seen_events.insert(ch.name.as_str()) {
            main_events.push((ch.name.as_str(), ch.file.as_str(), ch.line));
        }
    }
    if !main_events.is_empty() {
        out.push_str(
            "\n/**\n * Main-to-renderer events: the original app pushed these from the main\n * process (`webContents.send`) or subscribed to them in the renderer\n * (`ipcRenderer.on`). Each wrapper returns an unlisten function — call it\n * to unsubscribe.\n */\nexport const mainEvents = {\n",
        );
        for (name, file, line) in main_events {
            out.push_str(&format!(
                "  /** `{}` (original: {}:{}). */\n  on{}: (callback: (payload: unknown) => void) => listen(\"{}\", (event) => callback(event.payload)),\n",
                comment_safe(name),
                file,
                line,
                to_pascal_case(name),
                rust_str_escape(name),
            ));
        }
        out.push_str("};\n");
    }

    // Preload keys other than `api` (covered by the `api` object above):
    // typed wrappers derived from the recovered preload shape, or nothing
    // with a TODO when the shape could not be recovered. A generic Proxy
    // is never emitted: it would invoke commands that do not exist and
    // turn the preload's booleans into (always truthy) functions.
    for key in &report.preload.exposed_keys {
        if key == "api" {
            continue;
        }
        let ident = to_camel_case(key);
        if ident == "api" || ident == "e2rApi" || ident == "mainEvents" {
            continue;
        }
        match report.preload.shapes.iter().find(|s| s.key == *key) {
            Some(shape) => out.push_str(&gen_preload_key_ts(shape, &ident)),
            None => {
                let location = report
                    .findings
                    .iter()
                    .find(|f| f.api == "contextBridge")
                    .map(|f| format!("{}:{}", f.file, f.line))
                    .unwrap_or_else(|| "the preload script".to_string());
                out.push_str(&format!(
                    "\n// Preload key `{}` (from {}): e2r could not determine its shape\n// statically (the exposed value is computed or imported), so no wrapper\n// was generated. TODO(e2r): add typed wrappers matching the original\n// preload — do NOT route its methods through `invoke` with the preload's\n// method names, because no such commands exist.\n",
                    comment_safe(key),
                    location,
                ));
            }
        }
    }
    out
}

/// One preload key's typed wrapper, derived from its recovered shape.
///
/// - Members named `send` / `receive` (the conventional preload IPC
///   plumbing) map to `emit` / `listen`; `removeListener` has no Tauri
///   equivalent by channel (each `listen` returns its own unlisten
///   function), so it is a `TODO` stub.
/// - Platform-check members (`process.platform === 'darwin'`, ...) map to
///   `platform()` comparisons from `@tauri-apps/api/os`.
/// - Other function members are `TODO` stubs that throw: the original
///   behavior (sync dialogs, `process.versions`, ...) has no mechanical
///   equivalent, and a throwing stub fails loudly instead of silently
///   returning the wrong thing.
/// - Other value members are `undefined` with a `TODO`: their original
///   value may depend on Node/Electron globals that do not exist here.
fn gen_preload_key_ts(shape: &crate::model::PreloadShape, ident: &str) -> String {
    let mut out = format!(
        "\n// Preload key `{}` (from {}:{}): typed from the original preload\n// shape. Members that map to Tauri calls are wired; the rest are\n// TODO(e2r) stubs — port them by hand.\nexport const {} = {{\n",
        comment_safe(&shape.key),
        shape.file,
        shape.line,
        ident,
    );
    if shape.members.is_empty() {
        out.push_str("  // The exposed object has no members e2r could recover.\n");
    }
    for member in &shape.members {
        let name = &member.name;
        match member.kind {
            PreloadMemberKind::Function if name == "send" => {
                out.push_str(
                    "  /** Send a message to the main process (was `ipcRenderer.send`). */\n  send: (channel: string, data?: unknown) => emit(channel, data),\n",
                );
            }
            PreloadMemberKind::Function if name == "receive" => {
                out.push_str(
                    "  /** Subscribe to a main-process event (was `ipcRenderer.on`). Returns an unlisten function. */\n  receive: (channel: string, callback: (data: unknown) => void) => listen(channel, (event) => callback(event.payload)),\n",
                );
            }
            PreloadMemberKind::Function if name == "removeListener" => {
                out.push_str(&format!(
                    "  /** TODO(e2r): Tauri `listen` returns an unlisten function per subscription — keep it and call it instead of `removeListener(channel)`. */\n  removeListener: (_channel: string) => {{ throw new Error(\"TODO(e2r): `{}.removeListener` has no channel-based equivalent — use the unlisten function returned by `listen`\"); }},\n",
                    comment_safe(&shape.key),
                ));
            }
            PreloadMemberKind::Function => {
                out.push_str(&format!(
                    "  /** TODO(e2r): `{}` has no mechanical Tauri equivalent — port it by hand (original: {}:{}). */\n  {}: (..._args: unknown[]): unknown => {{ throw new Error(\"TODO(e2r): `{}.{}` is not ported yet — see {}:{}\"); }},\n",
                    comment_safe(name),
                    shape.file,
                    shape.line,
                    name,
                    comment_safe(&shape.key),
                    comment_safe(name),
                    shape.file,
                    shape.line,
                ));
            }
            PreloadMemberKind::PlatformMac => {
                out.push_str(&format!(
                    "  /** Original: `process.platform === 'darwin'`. */\n  {name}: platform() === \"macos\",\n",
                ));
            }
            PreloadMemberKind::PlatformLinux => {
                out.push_str(&format!(
                    "  /** Original: `process.platform === 'linux'`. */\n  {name}: platform() === \"linux\",\n",
                ));
            }
            PreloadMemberKind::PlatformWindows => {
                out.push_str(&format!(
                    "  /** Original: `process.platform === 'win32'`. */\n  {name}: platform() === \"windows\",\n",
                ));
            }
            PreloadMemberKind::Value => {
                out.push_str(&format!(
                    "  /** TODO(e2r): `{}` was a non-function value in the preload — port its original value by hand (original: {}:{}). */\n  {name}: undefined,\n",
                    comment_safe(name),
                    shape.file,
                    shape.line,
                ));
            }
        }
    }
    out.push_str("};\n");
    out
}

/// `package.json` for the generated project. The Tauri scripts stay the
/// same; when renderer source was carried over, the original build/dev
/// scripts are preserved as `build:frontend` / `dev:frontend` and the
/// original dependencies (minus Electron tooling) are carried over so
/// those scripts can actually run. The original `"type"` field is kept so
/// bundler configs keep working. The version is the app's real version
/// (sanitized to semver), `0.1.0` when the app declares none.
fn gen_package_json(pkg: &str, plan: &RendererPlan, version: Option<&str>) -> Result<String> {
    let mut doc = serde_json::json!({
        "name": pkg,
        "version": sanitize_version(version),
        "private": true,
        "type": plan.module_type.as_deref().unwrap_or("module"),
        "scripts": {
            "dev": "tauri dev",
            "build": "tauri build",
            "tauri": "tauri"
        },
        "dependencies": {
            "@tauri-apps/api": "^2"
        },
        "devDependencies": {
            "@tauri-apps/cli": "^2"
        }
    });
    let obj = doc.as_object_mut().expect("package.json is an object");
    let scripts = obj["scripts"]
        .as_object_mut()
        .expect("scripts is an object");
    for (name, command) in &plan.extra_scripts {
        scripts.insert(name.clone(), serde_json::Value::String(command.clone()));
    }
    let dependencies = obj["dependencies"]
        .as_object_mut()
        .expect("dependencies is an object");
    for (name, version) in &plan.extra_dependencies {
        dependencies
            .entry(name.clone())
            .or_insert_with(|| serde_json::Value::String(version.clone()));
    }
    let dev_dependencies = obj["devDependencies"]
        .as_object_mut()
        .expect("devDependencies is an object");
    for (name, version) in &plan.extra_dev_dependencies {
        dev_dependencies
            .entry(name.clone())
            .or_insert_with(|| serde_json::Value::String(version.clone()));
    }
    Ok(serde_json::to_string_pretty(&doc)?)
}

/// `README.md` for the generated project. The renderer bullet describes
/// what was actually carried over (or what is still missing).
fn gen_readme(report: &ScanReport, plan: &RendererPlan) -> String {
    let has_listeners = report.ipc_channels.iter().any(|c| c.direction == "on");
    let listeners_bullet = if has_listeners {
        "- Port the `TODO(e2r)` listeners in `src-tauri/src/listeners.rs` — each\n  one is a stub for an `ipcMain.on` channel from the original app.\n"
    } else {
        ""
    };
    let renderer_bullet = match plan.kind {
        RendererKind::BuiltOutput if plan.source_carried => {
            "- Rebuild the frontend after changing it: `npm run build:frontend` (it also runs automatically on `tauri build`)."
        }
        RendererKind::BuiltOutput => {
            "- The built frontend was copied into `dist/` — the renderer source was not found, so rebuild it in the original app and re-run `e2r convert` to pick up a fresh build."
        }
        RendererKind::Source if plan.before_build_command.is_empty() => {
            "- Add a `build:frontend` script to `package.json` that builds the renderer source, then run it — the placeholder in `dist/` is replaced by the real build output."
        }
        RendererKind::Source => {
            "- Build the frontend once: `npm run build:frontend` — the placeholder in `dist/` is replaced by the real build output. (`tauri build` runs it automatically.)"
        }
        RendererKind::Static => {
            "- The static renderer was copied into `dist/` — edit it there; no build step needed."
        }
        RendererKind::Placeholder => {
            "- Copy your renderer source into `src/` (or point `build.frontendDist` in `src-tauri/tauri.conf.json` at your existing build output) and delete the placeholder `dist/index.html`."
        }
    };
    format!(
        "# {} (Tauri 2)\n\nGenerated by [electron-to-rust](https://github.com/mr-vishwet/rust-made-easy) (e2r) from an Electron app.\n\n## Prerequisites (Linux)\n\nTauri needs GTK and WebKit system libraries. On Debian/Ubuntu:\n\n```sh\nsudo apt install libwebkit2gtk-4.1-dev build-essential libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev\n```\n\nSee <https://v2.tauri.app/start/prerequisites/> for other platforms.\n\n## Run it\n\n```sh\nnpm install\nnpm run tauri dev\n```\n\n## What still needs doing\n\n- Port the `TODO(e2r)` commands in `src-tauri/src/commands.rs` — each one\n  is a stub for an `ipcMain.handle` channel from the original app.\n{listeners_bullet}{renderer_bullet}\n- See `MIGRATION-NOTES.md` for the full list.\n\n## Layout\n\n- `src-tauri/` — Rust backend (Tauri 2)\n- `src/lib/e2r-bridge.ts` — TypeScript bridge replacing the Electron preload\n",
        report.app_name
    )
}

/// `MIGRATION-NOTES.md`: what was converted, what needs hand work, and the
/// next steps.
fn gen_migration_notes(
    report: &ScanReport,
    plugins: &Plugins,
    handle_cmds: &[(IpcChannel, String)],
    on_cmds: &[IpcChannel],
    plan: &RendererPlan,
    icon_copied: Option<&str>,
) -> String {
    let mut out = format!(
        "# Migration notes — {}\n\nGenerated by electron-to-rust (e2r) from an Electron app.\n\n",
        report.app_name
    );
    if let Some(version) = &report.electron_version {
        out.push_str(&format!("Electron version: {version}\n\n"));
    }
    if let Some(version) = &report.app_version {
        out.push_str(&format!("App version: {version}\n\n"));
    }
    out.push_str(&format!(
        "Feasibility: {}/100 — {}\n\n",
        report.feasibility_score, report.verdict
    ));
    out.push_str("## Converted automatically\n\n");

    if report.windows.is_empty() {
        out.push_str("- **Windows:** none found; a default 800×600 window was generated.\n");
    } else {
        out.push_str(&format!("- **Windows ({}):**\n", report.windows.len()));
        for (i, w) in report.windows.iter().enumerate() {
            let label = if i == 0 {
                "main".to_string()
            } else {
                format!("window-{}", i + 1)
            };
            let title = w
                .title
                .as_ref()
                .map(|t| format!(", titled {t:?}"))
                .unwrap_or_default();
            let mut flags = String::new();
            if !w.resizable {
                flags.push_str(", not resizable");
            }
            if !w.frame {
                flags.push_str(", frameless (Tauri `decorations: false`)");
            }
            if let (Some(min_w), Some(min_h)) = (w.min_width, w.min_height) {
                flags.push_str(&format!(", min {min_w}×{min_h}"));
            }
            if let Some(bg) = &w.background_color {
                flags.push_str(&format!(", background {bg}"));
            }
            if w.from_window_state {
                flags.push_str(" (size from electron-window-state defaults)");
            } else if !w.width_known || !w.height_known {
                flags
                    .push_str(" (size unknown — dynamic dimensions, omitted from tauri.conf.json)");
            }
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
            out.push_str(&format!(
                "  - `{label}`: {width}×{height}{title}{flags} ({}:{})\n",
                w.file, w.line
            ));
        }
    }

    if handle_cmds.is_empty() {
        out.push_str("- **IPC commands:** none — no `ipcMain.handle` channels were found.\n");
    } else {
        out.push_str(&format!(
            "- **IPC commands ({}):** `TODO` stubs in `src-tauri/src/commands.rs`:\n",
            handle_cmds.len()
        ));
        for (ch, name) in handle_cmds {
            out.push_str(&format!(
                "  - `{}` → `commands::{name}` ({}:{})\n",
                ch.name, ch.file, ch.line
            ));
        }
    }
    if report.findings.iter().any(|f| f.api == "nativeTheme") {
        out.push_str("- **Theme:** `commands::set_theme` in `src-tauri/src/commands.rs` (call via `api.setTheme` in the bridge) — wire it where the original set `nativeTheme.themeSource`.\n");
    }
    if !on_cmds.is_empty() {
        out.push_str(&format!(
            "- **Event listeners ({}):** `app.listen` stubs in `src-tauri/src/listeners.rs` (registered from `lib.rs`); the bridge emits to them via `emit`:\n",
            on_cmds.len()
        ));
        for ch in on_cmds {
            out.push_str(&format!("  - `{}` ({}:{})\n", ch.name, ch.file, ch.line));
        }
    }
    // Main → renderer events, deduplicated by name: the same channel often
    // appears as a `webContents.send` call and an `ipcRenderer.on`
    // subscription.
    let mut seen_main_events: BTreeSet<&str> = BTreeSet::new();
    let main_events: Vec<&IpcChannel> = report
        .ipc_channels
        .iter()
        .filter(|c| c.direction == "listen")
        .chain(report.main_to_renderer.iter())
        .filter(|c| seen_main_events.insert(c.name.as_str()))
        .collect();
    if !main_events.is_empty() {
        out.push_str("- **Main → renderer events** (`listen` wrappers in `src/lib/e2r-bridge.ts`, `mainEvents`):\n");
        for ch in main_events {
            out.push_str(&format!("  - `{}` ({}:{})\n", ch.name, ch.file, ch.line));
        }
    }
    let mut seen_sends: BTreeSet<&str> = BTreeSet::new();
    let send_channels: Vec<&IpcChannel> = report
        .ipc_channels
        .iter()
        .filter(|c| c.direction == "send" && !handle_cmds.iter().any(|(h, _)| h.name == c.name))
        .filter(|c| seen_sends.insert(c.name.as_str()))
        .collect();
    if !send_channels.is_empty() {
        out.push_str("- **Renderer event sends** (via `emit` in `src/lib/e2r-bridge.ts`):\n");
        for ch in send_channels {
            out.push_str(&format!("  - `{}` ({}:{})\n", ch.name, ch.file, ch.line));
        }
    }

    let deps = plugins.cargo_deps();
    if deps.is_empty() {
        out.push_str("- **Plugins:** none needed.\n");
    } else {
        out.push_str(&format!("- **Plugins:** {}\n", deps.join(", ")));
    }
    let permissions = plugins.capability_permissions();
    if permissions.is_empty() {
        out.push_str(
            "- **Capabilities:** `src-tauri/capabilities/default.json` grants `core:default`.\n",
        );
    } else {
        out.push_str(&format!(
            "- **Capabilities:** `src-tauri/capabilities/default.json` grants `core:default` plus {}.\n",
            permissions.join(", ")
        ));
    }

    match &report.menu {
        Some(menu) if menu.is_static_literal => {
            out.push_str(&format!(
                "- **Menu:** static template from {}:{} — stub in `src-tauri/src/menu.rs` (original template kept as a comment); items still need porting.\n",
                menu.file, menu.line
            ));
        }
        Some(menu) => {
            out.push_str(&format!(
                "- **Menu:** dynamic template at {}:{} — `src-tauri/src/menu.rs` is a stub; port it by hand.\n",
                menu.file, menu.line
            ));
        }
        None => {}
    }

    if report.preload.uses_context_bridge {
        out.push_str("- **Preload:** `window.");
        out.push_str(&report.preload.exposed_keys.join("`, `window."));
        out.push_str("` re-implemented in `src/lib/e2r-bridge.ts`");
        if !report.preload.files.is_empty() {
            out.push_str(&format!(" (from {})", report.preload.files.join(", ")));
        }
        out.push_str(".\n");
    }

    out.push_str(&format!("- **Renderer:** {}\n", plan.summary));
    for detail in &plan.details {
        out.push_str(&format!("  - {detail}\n"));
    }

    out.push_str("\n## Needs hand work\n\n");
    let manual: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| f.classification == Classification::Manual)
        .collect();
    let assisted: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| f.classification == Classification::Assisted)
        .collect();
    if manual.is_empty()
        && assisted.is_empty()
        && report.native_modules.is_empty()
        && report.port_needing_deps.is_empty()
    {
        out.push_str("Nothing flagged beyond the `TODO(e2r)` command stubs above.\n");
    }
    if !manual.is_empty() {
        out.push_str("### Manual ports (no direct Tauri equivalent)\n\n");
        for f in &manual {
            out.push_str(&format!("- `{}` ({}:{})", f.api, f.file, f.line));
            if f.occurrences > 1 {
                out.push_str(&format!(" (×{} occurrences)", f.occurrences));
            }
            if !f.note.is_empty() {
                out.push_str(&format!(" — {}", f.note));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    if !assisted.is_empty() {
        out.push_str("### Assisted conversions (Tauri equivalent exists, needs decisions)\n\n");
        for f in &assisted {
            out.push_str(&format!("- `{}` ({}:{})", f.api, f.file, f.line));
            if f.occurrences > 1 {
                out.push_str(&format!(" (×{} occurrences)", f.occurrences));
            }
            if let Some(target) = &f.tauri_target {
                out.push_str(&format!(" → {target}"));
            }
            if !f.note.is_empty() {
                out.push_str(&format!(" — {}", f.note));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    if !report.native_modules.is_empty() {
        out.push_str("### Native modules\n\n");
        for m in &report.native_modules {
            out.push_str(&format!(
                "- `{m}`: no Tauri equivalent — find a Rust crate or rewrite it.\n"
            ));
        }
        out.push('\n');
    }
    if !report.port_needing_deps.is_empty() {
        out.push_str("### Port-needing dependencies (pure JS, plugin-backed)\n\n");
        for m in &report.port_needing_deps {
            match mappings::lookup(m) {
                Some(mapping) => match (&mapping.plugin, &mapping.tauri_target) {
                    (Some(plugin), _) => out.push_str(&format!(
                        "- `{m}`: covered by `{plugin}` (included) — port its calls to the plugin API.\n",
                    )),
                    (None, _) => out.push_str(&format!(
                        "- `{m}`: no Tauri equivalent — port its calls to invoke-based Rust commands.\n",
                    )),
                },
                None => out.push_str(&format!(
                    "- `{m}`: needs a port; no mapping for it yet — review manually.\n"
                )),
            }
        }
        out.push('\n');
    }
    if plan.kind == RendererKind::Placeholder || !plan.caveats.is_empty() {
        out.push_str("### Frontend\n\n");
        if plan.kind == RendererKind::Placeholder {
            out.push_str("No renderer files were found in the scanned app (no `dist/`/`build/`/`out/` build output, no renderer source directory, no root `index.html`), so a placeholder `dist/index.html` was generated. Copy your renderer source into `src/` (or point `build.frontendDist` in `src-tauri/tauri.conf.json` at your existing build output) and delete the placeholder.\n\n");
        }
        for caveat in &plan.caveats {
            out.push_str(&format!("- {caveat}\n"));
        }
        if !plan.caveats.is_empty() {
            out.push('\n');
        }
    }
    match icon_copied {
        Some(icon) => out.push_str(&format!(
            "### Icon\n\n`src-tauri/icons/icon.png` is the app's real icon, copied from `{icon}` — generate the platform sizes with `cargo tauri icon`.\n\n"
        )),
        None => out.push_str("### Icon\n\n`src-tauri/icons/icon.png` is a placeholder — replace it with the app's real icon (and generate the platform sizes with `cargo tauri icon`).\n\n"),
    }
    let renderer_step = match plan.kind {
        RendererKind::BuiltOutput if plan.source_carried => {
            "2. Rebuild the frontend when you change it: `npm run build:frontend` (or just run `tauri build` — it runs automatically).\n"
        }
        RendererKind::BuiltOutput => {
            "2. The built frontend is already in `dist/` — nothing to copy. Rebuild it in the original app and re-run `e2r convert` to pick up changes.\n"
        }
        RendererKind::Source if plan.before_build_command.is_empty() => {
            "2. Add a `build:frontend` script to `package.json` that builds the renderer source, then run it.\n"
        }
        RendererKind::Source => {
            "2. Build the frontend: `npm run build:frontend` (runs automatically on `tauri build`).\n"
        }
        RendererKind::Static => "2. The static renderer is already in `dist/` — nothing to copy.\n",
        RendererKind::Placeholder => "2. Copy your renderer source in (see above).\n",
    };
    out.push_str("## Next steps\n\n1. `npm install`\n");
    out.push_str(renderer_step);
    out.push_str("3. Port each `TODO(e2r)` command in `src-tauri/src/commands.rs`");
    if !on_cmds.is_empty() {
        out.push_str(" and each `TODO(e2r)` listener in `src-tauri/src/listeners.rs`");
    }
    out.push_str(".\n4. `npm run tauri dev`\n");
    out
}

/// How the renderer was carried over into the generated project.
///
/// `pub(crate)` so the simulator ([`crate::simulate`]) can reuse the
/// converter's renderer detection instead of re-implementing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RendererKind {
    /// Built output (`dist/` / `build/` / `out/`) was copied.
    BuiltOutput,
    /// Renderer source was copied and the build script preserved.
    Source,
    /// Static files at the app root were copied into `dist/`.
    Static,
    /// No renderer files were found; a placeholder was generated.
    Placeholder,
}

/// One copy instruction: copy `from` (relative to the app dir) to `to`
/// (relative to the output dir). A directory is copied recursively.
///
/// `pub(crate)` because [`RendererPlan`] (which is `pub(crate)`) holds a
/// `Vec` of these.
#[derive(Debug, Clone)]
pub(crate) struct CopySpec {
    from: PathBuf,
    to: PathBuf,
}

/// Everything the converter decided about the renderer: what to copy,
/// how to wire `tauri.conf.json` / `package.json`, and what to tell the
/// user in the notes and warnings.
///
/// `pub(crate)` so the simulator ([`crate::simulate`]) can reuse the
/// converter's renderer detection instead of re-implementing it. Only
/// the fields the simulator reads are `pub(crate)`; the rest stay
/// private to the converter.
#[derive(Debug, Clone)]
pub(crate) struct RendererPlan {
    pub(crate) kind: RendererKind,
    /// Frontend output directory (relative to the project root), e.g.
    /// `"dist"`. `frontend_dist` is the same directory relative to
    /// `src-tauri/`.
    pub(crate) frontend_dir: String,
    /// `build.frontendDist`, e.g. `"../dist"`.
    frontend_dist: String,
    /// `build.devUrl`, or `None` when the app has no dev server (Tauri
    /// dev then serves `frontendDist`).
    dev_url: Option<String>,
    /// `true` when `dev_url` is the 1420 fallback, not a detected port.
    dev_port_fallback: bool,
    /// `build.beforeDevCommand`, e.g. `"npm run dev:frontend"`.
    before_dev_command: String,
    /// `build.beforeBuildCommand`, e.g. `"npm run build:frontend"`.
    before_build_command: String,
    /// Extra `scripts` for the generated `package.json`.
    extra_scripts: Vec<(String, String)>,
    /// `dependencies` carried over from the original `package.json`.
    extra_dependencies: Vec<(String, String)>,
    /// `devDependencies` carried over from the original `package.json`.
    extra_dev_dependencies: Vec<(String, String)>,
    /// The original `"type"` field (`"module"` / `"commonjs"`), if any —
    /// copied so bundler configs keep working.
    module_type: Option<String>,
    /// What to copy.
    copies: Vec<CopySpec>,
    /// One-line summary for the "Converted automatically" notes.
    pub(crate) summary: String,
    /// Extra detail bullets for the "Converted automatically" notes.
    details: Vec<String>,
    /// Honest caveats for the "Needs hand work" notes (and warnings).
    pub(crate) caveats: Vec<String>,
    /// Whether renderer source was copied (also true when built output
    /// won and the source came along for rebuilds).
    pub(crate) source_carried: bool,
    /// The renderer source directory relative to the app root, when one
    /// was found (e.g. `"src"`, `"app/src/ui"`, `"packages/app-desktop"`).
    /// The simulator scans this directory — not the fixed root
    /// `src/` / `renderer/` / `app/` / `web/` set — for Electron-API
    /// touch points, so a monorepo renderer is not mistaken for the
    /// whole app tree.
    pub(crate) source_dir: Option<String>,
    /// Whether the copied build output looks stale (source newer).
    pub(crate) stale_build_output: bool,
    /// Files actually copied (filled in by the copy step).
    copied_files: usize,
    /// Bytes actually copied (filled in by the copy step).
    copied_bytes: u64,
}

impl Default for RendererPlan {
    fn default() -> Self {
        Self {
            kind: RendererKind::Placeholder,
            frontend_dir: "dist".to_string(),
            frontend_dist: "../dist".to_string(),
            dev_url: None,
            dev_port_fallback: false,
            before_dev_command: String::new(),
            before_build_command: String::new(),
            extra_scripts: Vec::new(),
            extra_dependencies: Vec::new(),
            extra_dev_dependencies: Vec::new(),
            module_type: None,
            copies: Vec::new(),
            summary: String::new(),
            details: Vec::new(),
            caveats: Vec::new(),
            source_carried: false,
            source_dir: None,
            stale_build_output: false,
            copied_files: 0,
            copied_bytes: 0,
        }
    }
}

/// A frontend bundler, detected from dependencies and scripts. Used for
/// the dev-server default port and the build output directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bundler {
    Vite,
    Webpack,
    Next,
    ReactScripts,
    Parcel,
    ElectronVite,
    Rolldown,
}

impl Bundler {
    fn default_dev_port(self) -> u16 {
        match self {
            Bundler::Vite | Bundler::ElectronVite => 5173,
            Bundler::Webpack => 8080,
            Bundler::Next | Bundler::ReactScripts => 3000,
            Bundler::Parcel => 1234,
            // Rolldown has no dev server of its own; the Tauri default.
            Bundler::Rolldown => 1420,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Bundler::Vite => "Vite",
            Bundler::Webpack => "webpack",
            Bundler::Next => "Next.js",
            Bundler::ReactScripts => "Create React App",
            Bundler::Parcel => "Parcel",
            Bundler::ElectronVite => "electron-vite",
            Bundler::Rolldown => "rolldown",
        }
    }
}

/// A frontend dev server, with the port `devUrl` should use and the
/// original script text (preserved as `dev:frontend`).
#[derive(Debug, Clone)]
struct DevServer {
    url: String,
    /// `true` when the port is the 1420 fallback, not a detected port.
    port_fallback: bool,
    /// Where the port came from, for the notes (e.g. "`vite.config.js`").
    port_source: String,
    /// The original `dev` script text.
    script: String,
}

/// Build-output directories checked (in order) for a built renderer:
/// the first one containing an `index.html` wins. `bundles/` is
/// Signal's rolldown output directory.
const BUILD_OUTPUT_DIRS: &[&str] = &["dist", "build", "out", "bundles"];

/// Directories that may hold renderer source, checked in order.
/// `ts/` is Signal's renderer home (rolldown bundles it into `bundles/`).
const SOURCE_DIRS: &[&str] = &["src", "renderer", "app", "web", "ts"];

/// Subdirectories of a source dir that may hold the renderer one level
/// down (e.g. `src/renderer/` in electron-vite / electron-react-boilerplate
/// layouts, `app/src/ui/` in GitHub Desktop's monorepo layout). The whole
/// top-level dir is copied, preserving relative paths.
const NESTED_SOURCE_DIRS: &[&str] = &["renderer", "app", "web", "src", "ui", "gui"];

/// File stems that mark a renderer entry point inside a source dir.
const RENDERER_ENTRY_STEMS: &[&str] = &["main", "index", "renderer", "App", "app"];

/// Directory names the recursive renderer search never descends into.
/// `examples/` and friends are last: a renderer never lives under an
/// example, demo, sample, sandbox, or playground tree (Insomnia's
/// `examples/insomnia-plugin-sandbox-demo` was copied as the renderer
/// before this list grew — see `is_renderer_search_skip_dir`).
const RENDERER_SEARCH_SKIP_DIRS: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    "out",
    "bundles",
    ".git",
    ".hg",
    ".svn",
    "target",
    "test",
    "tests",
    "spec",
    "specs",
    "__tests__",
    "fixtures",
    "playwright",
    "e2e",
    "integration-tests",
    "extensions",
    "docs",
    "scripts",
    "tools",
    "examples",
    "example",
    "demo",
    "demos",
    "sample",
    "samples",
    "sandbox",
    "playground",
];

/// Directory-name fragments that mark a dir as a non-shipped
/// example/demo/sample tree, matched case-insensitively against the
/// dir name: a renderer never lives under one (a `*-demo` plugin
/// sandbox, a `sample-app`). Catches names the exact list above
/// misses, e.g. `insomnia-plugin-sandbox-demo`.
const NON_SHIPPED_DIR_FRAGMENTS: &[&str] = &["example", "demo", "sample", "sandbox", "playground"];

/// Whether a directory name is skipped by the recursive renderer
/// search: an exact skip-dir name, a hidden dir, or a name containing
/// a non-shipped fragment.
fn is_renderer_search_skip_dir(name: &str) -> bool {
    if RENDERER_SEARCH_SKIP_DIRS.contains(&name) || name.starts_with('.') {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    NON_SHIPPED_DIR_FRAGMENTS
        .iter()
        .any(|frag| lower.contains(frag))
}

/// Minimum `.tsx`/`.jsx` files for a dir to count as a dense renderer
/// candidate without an entry file (Joplin's `gui/` and Insomnia's
/// `src/` have no entry file at all — only density marks them).
const DENSE_RENDERER_MIN_TSX: usize = 10;

/// Upper bound on `.tsx`/`.jsx` files counted per candidate dir:
/// high enough that real renderer trees count exactly (Signal's `ts/`
/// holds 1,159), low enough to bound the walk. Above the cap the
/// ranking degrades to ties, broken by entry/depth/lexicographic
/// order — never to a wrong pick from an uncounted file.
const RENDERER_COUNT_CAP: usize = 2000;

/// A container dir is dropped when a descendant holds at least this
/// fraction of its renderer files: the parent adds (almost) nothing
/// of its own, so it is a package/container dir, not the renderer
/// (`packages/app-desktop/` holds 158 `.tsx` files, 154 of them under
/// its `gui/` — the 4 outside are plugin webviews, not a second
/// renderer).
const CONTAINER_COLLAPSE_FRACTION: f64 = 0.9;

/// Directory names that hold a static HTML shell rather than renderer
/// source: when the only entry found is an `index.html` inside one of
/// these, the *parent* directory is the renderer source (Mailspring's
/// `app/static/index.html` → `app/`).
const STATIC_SHELL_DIRS: &[&str] = &["static", "public", "assets", "www"];

/// Extensions of a renderer entry file.
const RENDERER_ENTRY_EXTS: &[&str] = &["js", "jsx", "ts", "tsx", "mjs", "cjs"];

/// Root-level asset directories copied for a static renderer.
const STATIC_ASSET_DIRS: &[&str] = &[
    "assets", "static", "public", "css", "js", "img", "images", "fonts", "media", "styles",
    "scripts", "vendor",
];

/// Root-level file extensions copied for a static renderer (other files
/// at the app root are main-process code, config, or docs — not renderer).
const STATIC_ASSET_EXTS: &[&str] = &[
    "html", "css", "js", "png", "jpg", "jpeg", "gif", "svg", "ico", "webp", "avif", "woff",
    "woff2", "ttf", "otf", "eot", "mp3", "mp4", "wav", "webm",
];

/// Bundler / tooling config files copied alongside renderer source so
/// `npm run build:frontend` reproduces the original build.
const BUILD_CONFIG_FILES: &[&str] = &[
    "vite.config.js",
    "vite.config.mjs",
    "vite.config.cjs",
    "vite.config.ts",
    "vite.config.mts",
    "vite.config.cts",
    "webpack.config.js",
    "webpack.config.mjs",
    "webpack.config.cjs",
    "webpack.config.ts",
    "esbuild.config.js",
    "esbuild.config.mjs",
    "esbuild.config.cjs",
    "esbuild.config.ts",
    "rollup.config.js",
    "rollup.config.mjs",
    "rollup.config.cjs",
    "rollup.config.ts",
    "rolldown.config.js",
    "rolldown.config.mjs",
    "rolldown.config.cjs",
    "rolldown.config.ts",
    "electron-vite.config.js",
    "electron-vite.config.mjs",
    "electron-vite.config.cjs",
    "electron-vite.config.ts",
    "next.config.js",
    "next.config.mjs",
    "next.config.cjs",
    "next.config.ts",
    "tailwind.config.js",
    "tailwind.config.ts",
    "postcss.config.js",
    "postcss.config.cjs",
    "tsconfig.json",
    "jsconfig.json",
    "babel.config.js",
    "babel.config.cjs",
    ".babelrc",
];

/// Vite config filenames (a subset of [`BUILD_CONFIG_FILES`], searched
/// for `server.port` and `build.outDir`).
const VITE_CONFIG_FILES: &[&str] = &[
    "vite.config.js",
    "vite.config.mjs",
    "vite.config.cjs",
    "vite.config.ts",
    "vite.config.mts",
    "vite.config.cts",
];

/// Next.js config filenames, searched for `output: 'export'`.
const NEXT_CONFIG_FILES: &[&str] = &[
    "next.config.js",
    "next.config.mjs",
    "next.config.cjs",
    "next.config.ts",
];

/// Directories never copied (dependencies, version control).
const COPY_SKIP_DIRS: &[&str] = &["node_modules", ".git", ".hg", ".svn"];

/// Dependencies dropped when the original `package.json` dependencies are
/// carried over: Electron itself and its packaging tooling have no place
/// in the Tauri project. (`@electron/*` scoped packages are dropped too.)
const DROPPED_DEPENDENCIES: &[&str] = &[
    "electron",
    "electron-builder",
    "electron-packager",
    "electron-rebuild",
];

/// Keywords that mark a `dev` script as a frontend dev server (as opposed
/// to a script that just launches Electron).
const DEV_SERVER_KEYWORDS: &[&str] = &[
    "vite",
    "webpack",
    "next",
    "parcel",
    "react-scripts",
    "serve",
    "http-server",
    "live-server",
];

/// Keywords that mark a `build` script as a renderer build (as opposed to
/// a packaging-only script).
const BUNDLER_BUILD_KEYWORDS: &[&str] = &[
    "vite",
    "webpack",
    "esbuild",
    "rollup",
    "parcel",
    "next",
    "react-scripts",
    "tsc",
    "swc",
    "babel",
];

/// How much newer than the build output a source file must be before the
/// output is flagged stale. A fresh checkout sets every file's mtime
/// within seconds of each other, so a 60-second margin keeps checkouts
/// from false-positiveing while still catching genuinely stale builds.
const STALE_MARGIN: Duration = Duration::from_secs(60);

/// Parse the app's `package.json` (if it exists and parses) for renderer
/// detection: scripts, dependencies, and `"type"`. A missing or broken
/// file is not an error — detection just has less to work with.
fn read_package_json_value(app_dir: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(app_dir.join("package.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Strip a leading `./` from a relative path for comparison.
fn normalize_rel(path: &str) -> &str {
    path.strip_prefix("./").unwrap_or(path)
}

/// Detect the renderer and plan how to carry it over.
///
/// `pub(crate)` so the simulator ([`crate::simulate`]) can reuse this
/// detection instead of re-implementing renderer analysis.
///
/// Priority order: (a) existing build output, (b) renderer source plus
/// its build script, (c) static files at the app root, (d) placeholder
/// when nothing is found. (a) and (b) combine when both exist: the built
/// output is what `frontendDist` points at, and the source comes along
/// so `tauri build` can rebuild it.
pub(crate) fn detect_renderer(
    app_dir: &Path,
    pkg: &Option<serde_json::Value>,
    report: &ScanReport,
) -> RendererPlan {
    let mut plan = RendererPlan::default();
    let pkg_ref = pkg.as_ref();
    let main_entry = report.main_entry.as_deref();
    let preloads: Vec<String> = report
        .preload
        .files
        .iter()
        .map(|p| normalize_rel(p).to_string())
        .collect();

    let scripts = pkg_ref.and_then(|p| p.get("scripts"));
    let build_script = scripts
        .and_then(|s| s.get("build"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let bundler = pkg_ref.and_then(detect_bundler);
    let source_dir = find_source_dir(app_dir, main_entry, &preloads);
    plan.source_dir = source_dir.clone();
    let root_index = app_dir.join("index.html").is_file();
    let dev_server = pkg_ref.and_then(|p| detect_dev_server(app_dir, p));

    // (a) Existing build output wins for `frontendDist`: it is the
    // renderer's real, runnable form.
    let mut built_output: Option<String> = None;
    if let Some(out) = find_build_output(app_dir) {
        built_output = Some(out.clone());
        plan.kind = RendererKind::BuiltOutput;
        plan.frontend_dir = out.clone();
        plan.frontend_dist = format!("../{out}");
        plan.copies.push(CopySpec {
            from: PathBuf::from(&out),
            to: PathBuf::from(&out),
        });
        if source_newer_than_build(app_dir, &out) {
            plan.stale_build_output = true;
            let rebuild = if build_script.is_some() {
                "`npm run build:frontend`"
            } else {
                "in the original app"
            };
            plan.caveats.push(format!(
                "The build output in `{out}/` may be stale (renderer source files are newer) — rebuild it ({rebuild}) and re-run `e2r convert` to pick up a fresh copy"
            ));
        }
    }

    // (b) Renderer source: copied so the frontend can be rebuilt inside
    // the generated project. Triggers on a source dir with a renderer
    // entry, or on a root `index.html` with a build/dev script (the Vite
    // root layout). A packaging-only build script must not turn a static
    // app into a "source" app.
    let packaging_only_build = build_script
        .as_deref()
        .is_some_and(is_packaging_only_build_script);
    let source_mode = source_dir.is_some()
        || (root_index
            && !packaging_only_build
            && (build_script.is_some() || dev_server.is_some()));
    if source_mode {
        plan.source_carried = true;
        if let Some(dir) = &source_dir {
            plan.copies.push(CopySpec {
                from: PathBuf::from(dir),
                to: PathBuf::from(dir),
            });
        }
        if root_index {
            plan.copies.push(CopySpec {
                from: PathBuf::from("index.html"),
                to: PathBuf::from("index.html"),
            });
        }
        if app_dir.join("public").is_dir() {
            plan.copies.push(CopySpec {
                from: PathBuf::from("public"),
                to: PathBuf::from("public"),
            });
        }
        for cfg in BUILD_CONFIG_FILES {
            if app_dir.join(cfg).is_file() {
                plan.copies.push(CopySpec {
                    from: PathBuf::from(cfg),
                    to: PathBuf::from(cfg),
                });
            }
        }
        if let Some(p) = pkg_ref {
            merge_deps(p, &mut plan);
            plan.module_type = p.get("type").and_then(|v| v.as_str()).map(str::to_string);
        }
        match &build_script {
            Some(script) => {
                plan.extra_scripts
                    .push(("build:frontend".to_string(), script.clone()));
                plan.before_build_command = "npm run build:frontend".to_string();
                if script_contains_electron_tooling(script) {
                    plan.caveats.push(format!(
                        "The build script references Electron tooling (`{script}`) — it was preserved as `build:frontend`, but remove the Electron packaging step (Tauri bundles with `cargo tauri build`) or run that step manually"
                    ));
                }
            }
            None => {
                plan.caveats.push(
                    "No `build` script in the original `package.json` — the renderer source was copied, but add a build script (as `build:frontend`) so `tauri build` can rebuild it"
                        .to_string(),
                );
            }
        }
        if plan.kind == RendererKind::Placeholder {
            plan.kind = RendererKind::Source;
            let out_dir = detect_out_dir(app_dir, bundler);
            plan.frontend_dir = out_dir.clone();
            plan.frontend_dist = format!("../{out_dir}");
        } else if let Some(out) = &built_output {
            // The build reproduces into `out_dir`; if that is not the
            // directory the copied output lives in, say so.
            let out_dir = detect_out_dir(app_dir, bundler);
            if out_dir != *out {
                plan.caveats.push(format!(
                    "The build script outputs to `{out_dir}/` but the copied build output is in `{out}/` — after the first rebuild, point `build.frontendDist` at `../{out_dir}`"
                ));
            }
        }
        if let Some(dir) = &source_dir {
            if let Some(main) = main_entry {
                if normalize_rel(main).starts_with(&format!("{dir}/")) {
                    plan.caveats.push(format!(
                        "The copied `{dir}/` also contains the Electron main process (`{main}`) — delete it once its handlers are ported to Rust"
                    ));
                }
            }
        }
        match bundler {
            Some(Bundler::ElectronVite) => plan.caveats.push(
                "This app uses electron-vite, which bundles main + preload + renderer for Electron — the preserved build script targets Electron's triple, so adapt it to a plain `vite build` for the renderer (Tauri replaces the main process)"
                    .to_string(),
            ),
            Some(Bundler::Next) if !next_static_export(app_dir) => plan.caveats.push(
                "This app uses Next.js, which renders server-side by default — Tauri serves static files, so enable `output: 'export'` in `next.config` and rebuild, or the app will not load"
                    .to_string(),
            ),
            _ => {}
        }
    } else if build_script.is_some() {
        // A build script exists but no renderer source was found — the
        // script probably builds something else (or the layout is exotic).
        plan.caveats.push(
            "The original `package.json` has a `build` script but no renderer source directory was found, so it was not preserved — check whether it builds the renderer"
                .to_string(),
        );
    }

    // (c) Plain static renderer at the app root.
    if plan.kind == RendererKind::Placeholder && root_index {
        plan.kind = RendererKind::Static;
        plan.frontend_dir = "dist".to_string();
        plan.frontend_dist = "../dist".to_string();
        plan.copies.push(CopySpec {
            from: PathBuf::from("index.html"),
            to: PathBuf::from("dist/index.html"),
        });
        for dir in STATIC_ASSET_DIRS {
            if app_dir.join(dir).is_dir() {
                plan.copies.push(CopySpec {
                    from: PathBuf::from(*dir),
                    to: PathBuf::from("dist").join(dir),
                });
            }
        }
        if let Ok(entries) = fs::read_dir(app_dir) {
            for entry in entries.flatten() {
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let name = match entry.file_name().to_str() {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                if name == "index.html" {
                    continue;
                }
                if main_entry.map(normalize_rel) == Some(name.as_str()) {
                    continue; // the main-process entry is not a renderer asset
                }
                if preloads.iter().any(|p| p == &name) {
                    continue; // preload scripts are replaced by the bridge
                }
                let path = entry.path();
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !STATIC_ASSET_EXTS.contains(&ext) {
                    continue;
                }
                if is_main_process_script(&path) {
                    plan.caveats.push(format!(
                        "`{name}` at the app root imports Electron, so it was treated as main-process code and not copied into `dist/`"
                    ));
                    continue;
                }
                plan.copies.push(CopySpec {
                    from: PathBuf::from(&name),
                    to: PathBuf::from("dist").join(&name),
                });
            }
        }
    }

    // No source was carried with the built output: the copied output is
    // the only frontend in the generated project, and it is gitignored.
    if plan.kind == RendererKind::BuiltOutput && !plan.source_carried {
        plan.caveats.push(
            "No renderer source was found, so the copied build output is the only frontend in this project — it is gitignored by the generated `.gitignore`; remove the `dist/` line from `.gitignore` if you want it versioned"
                .to_string(),
        );
    }

    // Dev server wiring (independent of the carry-over kind).
    if let Some(dev) = &dev_server {
        plan.dev_url = Some(dev.url.clone());
        plan.dev_port_fallback = dev.port_fallback;
        plan.before_dev_command = "npm run dev:frontend".to_string();
        plan.extra_scripts
            .push(("dev:frontend".to_string(), dev.script.clone()));
        plan.details.push(format!(
            "Dev server: `{}` (port from {})",
            dev.url, dev.port_source
        ));
        if dev.port_fallback {
            plan.caveats.push(
                "The dev server port could not be detected, so `devUrl` uses the Tauri default 1420 — set the real port in `src-tauri/tauri.conf.json` if `tauri dev` cannot connect"
                    .to_string(),
            );
        }
        if bundler == Some(Bundler::ElectronVite) {
            plan.caveats.push(
                "The dev script runs electron-vite, which launches Electron — replace `dev:frontend` with plain `vite` for `tauri dev`"
                    .to_string(),
            );
        }
        if !plan.source_carried {
            if let Some(p) = pkg_ref {
                merge_deps(p, &mut plan);
            }
        }
    } else if let Some(dev_url) = &report.dev_url {
        // No dev script, but the main process loads a dev URL (e.g.
        // `loadURL('http://localhost:4000')`): carry it into `devUrl`
        // so `tauri dev` connects to the app's real dev server. There is
        // no script to start it, so the user runs the server by hand.
        plan.dev_url = Some(dev_url.url.clone());
        plan.details.push(format!(
            "Dev server: `{}` (from the main process loading it in {}:{})",
            dev_url.url, dev_url.file, dev_url.line
        ));
        plan.caveats.push(
            "No frontend dev script was found in `package.json`, so `tauri dev` cannot start the dev server — start it manually (or add a `dev` script) before running `tauri dev`"
                .to_string(),
        );
    } else if matches!(plan.kind, RendererKind::BuiltOutput | RendererKind::Source) {
        plan.details.push(
            "No frontend dev server detected — `tauri dev` serves the built frontend from `frontendDist`."
                .to_string(),
        );
    }

    plan.summary = renderer_summary(&plan, built_output.as_deref(), source_dir.as_deref());
    plan
}

/// One-line summary of what the renderer plan did, for the notes.
fn renderer_summary(
    plan: &RendererPlan,
    built_output: Option<&str>,
    source_dir: Option<&str>,
) -> String {
    match plan.kind {
        RendererKind::BuiltOutput => {
            let out = built_output.unwrap_or(plan.frontend_dir.as_str());
            let mut summary = format!(
                "Copied the built frontend from `{out}/` into `{out}/`",
            );
            if plan.source_carried {
                summary.push_str("; the renderer source was also copied");
                if let Some(dir) = source_dir {
                    summary.push_str(&format!(" (`{dir}/`)"));
                }
                summary.push_str(" with its build script preserved as `build:frontend`");
            }
            summary.push('.');
            summary
        }
        RendererKind::Source => {
            let copied: Vec<String> = plan
                .copies
                .iter()
                .map(|c| format!("`{}`", c.from.display()))
                .collect();
            let mut summary = format!("Copied the renderer source ({})", copied.join(", "));
            if !plan.before_build_command.is_empty() {
                summary.push_str(
                    " and preserved the build script as `build:frontend` (wired as `beforeBuildCommand`, so `tauri build` rebuilds it)",
                );
            }
            summary.push('.');
            summary
        }
        RendererKind::Static => {
            "Copied the static renderer (`index.html` + assets) into `dist/`.".to_string()
        }
        RendererKind::Placeholder => {
            "No renderer files were found in the scanned app, so a placeholder `dist/index.html` was generated."
                .to_string()
        }
    }
}

/// The first build-output directory containing an `index.html`, if any.
/// Root first (`dist/` / `build/` / `out/`), then package dirs one level
/// down (`app/dist/`, `packages/app-desktop/dist/`, ...).
fn find_build_output(app_dir: &Path) -> Option<String> {
    for dir in BUILD_OUTPUT_DIRS {
        if app_dir.join(dir).join("index.html").is_file() {
            return Some(dir.to_string());
        }
    }
    let mut nested: Vec<String> = Vec::new();
    for entry in WalkDir::new(app_dir)
        .max_depth(2)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            if !e.file_type().is_dir() {
                return true;
            }
            e.file_name()
                .to_str()
                .map(|n| {
                    !RENDERER_SEARCH_SKIP_DIRS.contains(&n)
                        && !n.starts_with('.')
                        && !BUILD_OUTPUT_DIRS.contains(&n)
                })
                .unwrap_or(false)
        })
        .flatten()
    {
        if !entry.file_type().is_dir() || entry.depth() == 0 {
            continue;
        }
        let dir = entry.path();
        for out in BUILD_OUTPUT_DIRS {
            if dir.join(out).join("index.html").is_file() {
                let rel = dir
                    .join(out)
                    .strip_prefix(app_dir)
                    .unwrap_or(&dir.join(out))
                    .to_string_lossy()
                    .replace('\\', "/");
                nested.push(rel);
            }
        }
    }
    nested.sort();
    nested.into_iter().next()
}

/// Whether `dir` holds a renderer entry: an `index.html`, or a
/// `main`/`index`/`renderer`/`App`/`app` script (among `exts`) that is
/// not the main-process entry and not a preload script.
///
/// The main-process entry is excluded two ways: by exact relative path,
/// and by stem — a `main.ts` next to a `main.js` main entry is the main
/// process's source, not a renderer entry (VS Code's `src/main.ts` used
/// to match the `main` stem and pull all of `src/` in as "renderer").
/// The stem rule only fires for non-`.tsx`/`.jsx` files that import
/// Electron: a React renderer's `main.tsx` is a renderer entry even when
/// the main entry is also called `main`.
///
/// The recursive search passes `&["tsx", "jsx"]` only: a barrel
/// `index.ts` deep inside a library package (Joplin's
/// `gui/NoteEditor/commands/index.ts`) is a module entry, not a
/// renderer entry, and counting it would promote library subdirs over
/// the renderer itself. Plain `.js`/`.ts` renderers are covered by the
/// fixed source-dir check, which passes [`RENDERER_ENTRY_EXTS`].
fn dir_has_renderer_entry(
    app_dir: &Path,
    dir: &Path,
    main_entry: Option<&str>,
    preloads: &[String],
    exts: &[&str],
) -> bool {
    if dir.join("index.html").is_file() {
        return true;
    }
    let main_stem = main_entry.and_then(|m| {
        Path::new(m)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
    });
    for stem in RENDERER_ENTRY_STEMS {
        for ext in exts {
            let file = dir.join(format!("{stem}.{ext}"));
            if !file.is_file() {
                continue;
            }
            let rel = file
                .strip_prefix(app_dir)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            let rel = normalize_rel(&rel);
            if main_entry.map(normalize_rel) == Some(rel) {
                continue; // the main-process entry is not a renderer entry
            }
            if preloads.iter().any(|p| p == rel) {
                continue;
            }
            if Some(*stem) == main_stem.as_deref() && *ext != "tsx" && *ext != "jsx" {
                // Same stem as the main entry: only a renderer entry when
                // it does not import Electron (a main-process source file
                // does; a plain renderer bootstrap does not).
                if let Ok(text) = fs::read_to_string(&file) {
                    if text.contains("require('electron')")
                        || text.contains("require(\"electron\")")
                        || text.contains("from 'electron'")
                        || text.contains("from \"electron\"")
                    {
                        continue;
                    }
                }
            }
            return true;
        }
    }
    false
}

/// Whether a directory is a standalone sub-package (its own
/// `package.json` without a `main` field — a pure frontend sub-app like
/// Signal's `sticker-creator`), rather than a renderer dir of the
/// current package. Such dirs are not renderer candidates: the
/// sticker-creator is not Signal's renderer.
fn is_standalone_subpackage(dir: &Path) -> bool {
    let Ok(text) = fs::read_to_string(dir.join("package.json")) else {
        return false;
    };
    let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    pkg.get("main").is_none()
}

/// Whether `dir` lives inside a package that is not the app package:
/// its nearest `package.json` ancestor (excluding the app root itself)
/// is a *foreign* package. Foreign means one of two things, both
/// campaign-proven (2026-11-25):
///
/// * the owning package sits OUTSIDE the app package's dir — a sibling
///   sub-app or library (Joplin's `packages/pdf-viewer`, Insomnia's
///   `examples/insomnia-plugin-sandbox-demo`, whose package dirs are
///   siblings of the app package, not inside it); or
/// * the owning package is a standalone sub-app: its `package.json`
///   has NO `main` field (Signal's `sticker-creator/`, whose `src/`
///   has no `package.json` of its own but inherits the standalone
///   parent's). This subsumes [`is_standalone_subpackage`] for nested
///   dirs.
///
/// NOT foreign: packages nested INSIDE the app package that have a
/// `main` field — Mailspring's 53 `internal_packages/*` (each with
/// `main: ./lib/main`) are the app's own plugin UI, loaded by its
/// renderer; the first version of this rule wrongly excluded them and
/// narrowed Mailspring's renderer from `app/` to `app/src/`, dropping
/// 215 touch points. With no main entry the app package cannot be
/// located, so nothing is foreign.
fn is_in_foreign_package(dir: &Path, app_dir: &Path, main_entry: Option<&str>) -> bool {
    let Some(main) = main_entry else {
        return false;
    };
    // The app package: nearest `package.json` ancestor of the main
    // entry (the app root itself when the entry lives there).
    let main_path = app_dir.join(normalize_rel(main));
    let mut app_pkg: Option<PathBuf> = None;
    for anc in main_path.ancestors() {
        if anc == app_dir {
            app_pkg = Some(app_dir.to_path_buf());
            break;
        }
        if anc.join("package.json").is_file() {
            app_pkg = Some(anc.to_path_buf());
            break;
        }
    }
    let Some(app_pkg) = app_pkg else {
        return false;
    };
    // The owning package of `dir`: nearest `package.json` ancestor
    // (excluding the app root).
    let mut owning: Option<PathBuf> = None;
    for anc in dir.ancestors() {
        if anc == app_dir {
            break;
        }
        if anc.join("package.json").is_file() {
            owning = Some(anc.to_path_buf());
            break;
        }
    }
    let Some(owning) = owning else {
        return false;
    };
    // Standalone sub-app (no `main` field) is always foreign. An
    // unreadable package.json is treated as having a main field — an
    // exclusion must rest on evidence, not on a read failure.
    let has_main = fs::read_to_string(owning.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .map(|pkg| pkg.get("main").is_some())
        .unwrap_or(true);
    if !has_main {
        return true;
    }
    // Otherwise foreign only when the owning package sits outside the
    // app package's dir.
    !owning.starts_with(&app_pkg)
}

/// Drop container candidates: when candidate A is an ancestor of
/// candidate B and B holds at least [`CONTAINER_COLLAPSE_FRACTION`] of
/// A's `.tsx`/`.jsx` files, A adds (almost) nothing of its own — it is
/// a package/container dir, not the renderer. Keeps the deepest dir
/// holding (nearly) all the files.
///
/// Candidates are `(has_stem, tsx_count, depth, rel)`; the input order
/// does not matter, the output keeps it.
fn collapse_renderer_containers(
    mut candidates: Vec<(bool, usize, usize, String)>,
) -> Vec<(bool, usize, usize, String)> {
    // Deepest first, so a container is dropped only when a KEPT deeper
    // candidate already covers it.
    candidates.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)));
    let mut kept: Vec<(bool, usize, usize, String)> = Vec::new();
    for cand in candidates {
        let covered = cand.1 > 0
            && kept.iter().any(|k| {
                k.3.starts_with(&format!("{}/", cand.3))
                    && (k.1 as f64) >= CONTAINER_COLLAPSE_FRACTION * (cand.1 as f64)
            });
        if !covered {
            kept.push(cand);
        }
    }
    kept
}

/// Count `.tsx` / `.jsx` files under `dir` (bounded walk, test files
/// excluded), stopping as soon as `cap` is reached. Used to rank
/// renderer candidates and for the dense-renderer fallback. Foreign
/// packages and example/demo trees under `dir` are pruned, not
/// counted: a sibling sub-app's files must not inflate its parent's
/// density (`packages/` must not count `packages/pdf-viewer/`'s files
/// toward its own renderer candidacy).
///
/// The walk has NO depth limit on purpose: the count must be
/// comparable between a dir and its children (the container collapse
/// compares them), and a depth limit relative to each dir breaks that
/// — Mailspring's `app/` counted 269 files at depth ≤4 while its
/// `internal_packages/` counted 260, a 96.7% ratio that collapsed `app/`
/// away, when the true shares are 464 vs 308 (66%). The `cap` bounds
/// the work instead: the walk stops as soon as `cap` files are counted
/// (the density probe uses a cap of 10, so it stays cheap).
fn count_tsx_files(dir: &Path, cap: usize, app_dir: &Path, main_entry: Option<&str>) -> usize {
    let mut count = 0usize;
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            !(e.file_type().is_dir()
                && (e
                    .file_name()
                    .to_str()
                    .map(is_renderer_search_skip_dir)
                    .unwrap_or(false)
                    || is_in_foreign_package(e.path(), app_dir, main_entry)))
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "tsx" && ext != "jsx" {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.contains(".test.") || name.contains(".spec.") {
            continue;
        }
        count += 1;
        if count >= cap {
            break;
        }
    }
    count
}

/// Whether `dir` contains the app's main-process source: a file whose
/// stem matches the main entry's stem and which imports Electron. Such
/// a dir is the main process's home, not a renderer dir (VS Code's
/// `src/` holds `src/main.ts`).
fn dir_contains_main_source(dir: &Path, main_entry: Option<&str>) -> bool {
    let Some(main) = main_entry else {
        return false;
    };
    let Some(stem) = Path::new(main)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
    else {
        return false;
    };
    for entry in WalkDir::new(dir)
        .max_depth(2)
        .into_iter()
        .filter_entry(|e| {
            !(e.depth() > 0
                && e.file_type().is_dir()
                && e.file_name()
                    .to_str()
                    .map(|n| RENDERER_SEARCH_SKIP_DIRS.contains(&n))
                    .unwrap_or(false))
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.file_stem().and_then(|s| s.to_str()) != Some(stem.as_str()) {
            continue;
        }
        if let Ok(text) = fs::read_to_string(path) {
            if text.contains("require('electron')")
                || text.contains("require(\"electron\")")
                || text.contains("from 'electron'")
                || text.contains("from \"electron\"")
            {
                return true;
            }
        }
    }
    false
}

/// The source directory holding the renderer, if any: the first of
/// `src/` / `renderer/` / `app/` / `web/` / `ts/` with a renderer entry
/// file directly inside it or one level down (`src/renderer/`,
/// `app/src/ui/`, ...), then a bounded recursive search for monorepo
/// layouts, then a dense-renderer fallback, then nothing. Returns the
/// directory relative to the app root, with `/` separators.
///
/// The recursive search ranks candidates by `.tsx`/`.jsx` density
/// first (the renderer holds the most renderer files), then a strict
/// entry file (`.tsx`/`.jsx` stem or `index.html`), then shallowest,
/// then lexicographic order. Three guards keep decoys out, all
/// campaign-proven (2026-11-25): example/demo/sample trees are never
/// descended into; dirs inside a foreign package (a sibling sub-app
/// or library, not the app package) are not candidates; and container
/// dirs that merely hold another candidate's files collapse to the
/// deepest dir (`packages/app-desktop/` → its `gui/`,
/// `packages/` → `packages/insomnia/src/`). A dir with no entry file
/// still counts when it is dense (≥10 `.tsx`/`.jsx` files): Joplin's
/// `gui/` and Insomnia's `src/` have no entry file at all. This is the
/// single code path both `convert` and `simulate` use
/// (`detect_renderer`): the v2 scan and the v2 convert agreed on the
/// same wrong picks (Joplin `packages/pdf-viewer`, Signal
/// `sticker-creator/src`, Insomnia
/// `examples/insomnia-plugin-sandbox-demo`) because they shared this
/// function — there was no parallel heuristic to unify, only this
/// function to fix.
fn find_source_dir(
    app_dir: &Path,
    main_entry: Option<&str>,
    preloads: &[String],
) -> Option<String> {
    for dir in SOURCE_DIRS {
        let base = app_dir.join(dir);
        if !base.is_dir() {
            continue;
        }
        if !is_in_foreign_package(&base, app_dir, main_entry)
            && dir_has_renderer_entry(app_dir, &base, main_entry, preloads, RENDERER_ENTRY_EXTS)
        {
            return Some(dir.to_string());
        }
        for sub in NESTED_SOURCE_DIRS {
            let nested = base.join(sub);
            if nested.is_dir()
                && !is_in_foreign_package(&nested, app_dir, main_entry)
                && dir_has_renderer_entry(
                    app_dir,
                    &nested,
                    main_entry,
                    preloads,
                    RENDERER_ENTRY_EXTS,
                )
            {
                return Some(dir.to_string());
            }
        }
    }

    // Recursive search (depth ≤ 3) for monorepo layouts the fixed sets
    // above miss. A bare `index.html` inside a static-shell dir
    // (`static/`, `public/`) resolves to its parent dir (Mailspring's
    // `app/static/index.html` → `app/`).
    let mut candidates: Vec<(bool, usize, usize, String)> = Vec::new();
    for entry in WalkDir::new(app_dir)
        .max_depth(3)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            if !e.file_type().is_dir() {
                return true;
            }
            e.file_name()
                .to_str()
                .map(|n| !is_renderer_search_skip_dir(n))
                .unwrap_or(false)
        })
        .flatten()
    {
        if !entry.file_type().is_dir() || entry.depth() == 0 {
            continue;
        }
        let dir = entry.path();
        if is_standalone_subpackage(dir) {
            continue;
        }
        if is_in_foreign_package(dir, app_dir, main_entry) {
            continue;
        }
        // Strict entry only: a barrel `index.ts` is a module entry,
        // not a renderer entry (see `dir_has_renderer_entry`).
        let has_stem = RENDERER_ENTRY_STEMS.iter().any(|stem| {
            ["tsx", "jsx"]
                .iter()
                .any(|ext| dir.join(format!("{stem}.{ext}")).is_file())
        });
        let has_entry = dir_has_renderer_entry(app_dir, dir, main_entry, preloads, &["tsx", "jsx"]);
        // Cheap density probe first (stops at 10); only dense or
        // entry-bearing dirs pay for the accurate ranking count.
        let tsx_probe = count_tsx_files(dir, DENSE_RENDERER_MIN_TSX, app_dir, main_entry);
        if !has_entry && tsx_probe < DENSE_RENDERER_MIN_TSX {
            continue;
        }
        let tsx = if tsx_probe >= DENSE_RENDERER_MIN_TSX {
            count_tsx_files(dir, RENDERER_COUNT_CAP, app_dir, main_entry)
        } else {
            tsx_probe
        };
        let mut rel = dir
            .strip_prefix(app_dir)
            .unwrap_or(dir)
            .to_string_lossy()
            .replace('\\', "/");
        if !has_stem {
            if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
                if STATIC_SHELL_DIRS.contains(&name) {
                    if let Some(parent) = dir.parent() {
                        if let Ok(parent_rel) = parent.strip_prefix(app_dir) {
                            let parent_rel = parent_rel.to_string_lossy().replace('\\', "/");
                            if !parent_rel.is_empty() {
                                rel = parent_rel;
                            }
                        }
                    }
                }
            }
        }
        candidates.push((has_stem, tsx, entry.depth(), rel));
    }
    let candidates = collapse_renderer_containers(candidates);
    let mut candidates = candidates;
    candidates.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(b.0.cmp(&a.0))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });
    if let Some((_, _, _, rel)) = candidates.into_iter().next() {
        return Some(rel);
    }

    // Dense-renderer fallback: a dir (depth ≤ 2) holding at least 10
    // `.tsx`/`.jsx` files is renderer source even without an entry file
    // (Signal's `ts/`). It must not be the main process's home (see
    // `dir_contains_main_source`): VS Code's `src/` is dense with `.tsx`
    // but holds `src/main.ts`, so it stays unmatched rather than being
    // misreported as renderer source. Foreign packages are excluded
    // here too (a sibling sub-app's dense dir is not the renderer).
    let mut dense: Vec<(usize, usize, String)> = Vec::new();
    for entry in WalkDir::new(app_dir)
        .max_depth(2)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            if !e.file_type().is_dir() {
                return true;
            }
            e.file_name()
                .to_str()
                .map(|n| !is_renderer_search_skip_dir(n))
                .unwrap_or(false)
        })
        .flatten()
    {
        if !entry.file_type().is_dir() || entry.depth() == 0 {
            continue;
        }
        let dir = entry.path();
        if is_standalone_subpackage(dir) {
            continue;
        }
        if is_in_foreign_package(dir, app_dir, main_entry) {
            continue;
        }
        if dir_contains_main_source(dir, main_entry) {
            continue;
        }
        let count = count_tsx_files(dir, DENSE_RENDERER_MIN_TSX, app_dir, main_entry);
        if count >= DENSE_RENDERER_MIN_TSX {
            let rel = dir
                .strip_prefix(app_dir)
                .unwrap_or(dir)
                .to_string_lossy()
                .replace('\\', "/");
            dense.push((count, entry.depth(), rel));
        }
    }
    dense.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    dense.into_iter().next().map(|(_, _, rel)| rel)
}

/// Whether a root-level script imports Electron — main-process code that
/// must not be copied into `dist/` as a renderer asset.
fn is_main_process_script(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    text.contains("require('electron')")
        || text.contains("require(\"electron\")")
        || text.contains("from 'electron'")
        || text.contains("from \"electron\"")
}

/// Whether a build script only packages the app (electron-builder /
/// electron-packager) instead of building the renderer — such a script
/// must not turn a static app into a "source" app.
fn is_packaging_only_build_script(script: &str) -> bool {
    let lower = script.to_lowercase();
    let mentions_packaging = ["electron-builder", "electron-packager", "electron-rebuild"]
        .iter()
        .any(|k| lower.contains(k));
    mentions_packaging && !BUNDLER_BUILD_KEYWORDS.iter().any(|k| lower.contains(k))
}

/// Whether a build script references Electron tooling (packaging or
/// electron-vite) that needs manual adaptation in the Tauri project.
fn script_contains_electron_tooling(script: &str) -> bool {
    [
        "electron-builder",
        "electron-packager",
        "electron-rebuild",
        "electron-vite",
        "electron ",
    ]
    .iter()
    .any(|k| script.contains(k))
}

/// Detect the frontend bundler from dependencies and scripts.
/// electron-vite is checked first: those apps also depend on vite.
fn detect_bundler(pkg: &serde_json::Value) -> Option<Bundler> {
    let mut names: Vec<&str> = Vec::new();
    for section in ["dependencies", "devDependencies"] {
        if let Some(deps) = pkg.get(section).and_then(|d| d.as_object()) {
            names.extend(deps.keys().map(String::as_str));
        }
    }
    let script_text: String = pkg
        .get("scripts")
        .and_then(|s| s.as_object())
        .map(|s| {
            s.values()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let words: Vec<&str> = script_text
        .split(|c: char| c.is_whitespace() || c == '&' || c == ';' || c == '|')
        .collect();
    if names.contains(&"electron-vite") || script_text.contains("electron-vite") {
        return Some(Bundler::ElectronVite);
    }
    if names.contains(&"rolldown") || words.contains(&"rolldown") {
        return Some(Bundler::Rolldown);
    }
    if names.contains(&"vite") || words.contains(&"vite") {
        return Some(Bundler::Vite);
    }
    if names.contains(&"next") || words.contains(&"next") {
        return Some(Bundler::Next);
    }
    if names.contains(&"react-scripts") {
        return Some(Bundler::ReactScripts);
    }
    if names.contains(&"webpack-dev-server")
        || names.contains(&"webpack")
        || words.contains(&"webpack")
    {
        return Some(Bundler::Webpack);
    }
    if names.contains(&"parcel") || words.contains(&"parcel") {
        return Some(Bundler::Parcel);
    }
    None
}

/// Script names that can run a frontend dev server, in priority order.
/// Monorepos name the Vite script `start:dev-server` (Insomnia) rather
/// than `dev`; `start`/`serve` only count when the command itself names
/// a dev server (the keyword check in [`find_dev_script`] decides).
const DEV_SCRIPT_NAMES: &[&str] = &["dev", "start:dev-server", "dev:server", "start", "serve"];

/// Find a dev-server script in one `package.json`'s scripts object: the
/// first name in [`DEV_SCRIPT_NAMES`] whose command contains a
/// [`DEV_SERVER_KEYWORDS`] entry. A script that just launches Electron
/// (`electron .`, `electron ./app --dev`) is not a dev server.
fn find_dev_script(scripts: &serde_json::Value) -> Option<(&str, String)> {
    let scripts = scripts.as_object()?;
    for name in DEV_SCRIPT_NAMES {
        if let Some(cmd) = scripts.get(*name).and_then(|v| v.as_str()) {
            let lower = cmd.to_lowercase();
            if DEV_SERVER_KEYWORDS.iter().any(|k| lower.contains(k)) {
                return Some((name, cmd.to_string()));
            }
        }
    }
    None
}

/// Directories under `app_dir` (depth ≤ `max_depth`, root excluded)
/// that contain a `package.json` — the workspace packages of a
/// monorepo. `node_modules`, `.git` and build-output dirs are never
/// descended into.
fn collect_package_dirs(app_dir: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let walker = WalkDir::new(app_dir)
        .max_depth(max_depth + 1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Never descend into dependency, VCS, build-output or
            // hidden directories (pruned, not just skipped).
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            !(name == "node_modules"
                || name == ".git"
                || name == "dist"
                || name == "build"
                || name == "out"
                || name.starts_with('.'))
        });
    for entry in walker.filter_map(Result::ok) {
        let path = entry.path();
        if !entry.file_type().is_dir() || entry.depth() == 0 {
            continue;
        }
        if path.join("package.json").is_file() {
            out.push(path.to_path_buf());
        }
    }
    out
}

/// Detect the frontend dev server from the `dev` script — the root
/// package first, then workspace packages (N10). A script that just
/// launches Electron is not a dev server.
///
/// Port priority: `--port` in the dev script (it overrides config at
/// runtime) → the Vite config's `server.port` → the bundler's default
/// port → the Tauri default 1420, flagged as a fallback.
fn detect_dev_server(app_dir: &Path, pkg: &serde_json::Value) -> Option<DevServer> {
    if let Some(scripts) = pkg.get("scripts") {
        if let Some((_name, script)) = find_dev_script(scripts) {
            return Some(dev_server_from_script(app_dir, pkg, &script));
        }
    }
    // Workspace packages (Insomnia: the root has no dev script; the
    // real one is `packages/insomnia`'s `start:dev-server` running
    // `vite dev`). Deterministic order; depth ≤ 4.
    let mut dirs = collect_package_dirs(app_dir, 4);
    dirs.sort();
    for dir in dirs {
        let Some(ws_pkg) = read_package_json_value(&dir) else {
            continue;
        };
        let Some(scripts) = ws_pkg.get("scripts") else {
            continue;
        };
        if let Some((_name, script)) = find_dev_script(scripts) {
            return Some(dev_server_from_script(&dir, &ws_pkg, &script));
        }
    }
    None
}

/// Build a [`DevServer`] from one script command in one package
/// directory: port from `--port` in the script, else the Vite config
/// in that package's directory, else the bundler default, else 1420.
fn dev_server_from_script(pkg_dir: &Path, pkg: &serde_json::Value, script: &str) -> DevServer {
    let port_re = Regex::new(r"--port[= ](\d{2,5})").ok();
    let mut port: Option<(u16, String)> = port_re
        .as_ref()
        .and_then(|re| re.captures(script))
        .and_then(|c| c[1].parse().ok())
        .map(|p| (p, "the dev script (`--port`)".to_string()));
    if port.is_none() {
        if let Some((name, config)) = read_vite_config(pkg_dir) {
            if let Some(p) = vite_config_port(&config) {
                port = Some((p, format!("`{name}`")));
            }
        }
    }
    if port.is_none() {
        if let Some(bundler) = detect_bundler(pkg) {
            port = Some((
                bundler.default_dev_port(),
                format!("the {} default", bundler.name()),
            ));
        }
    }
    let (port, port_source, port_fallback) = match port {
        Some((p, source)) => (p, source, false),
        None => (
            1420,
            "the Tauri default (no port detected)".to_string(),
            true,
        ),
    };
    DevServer {
        url: format!("http://localhost:{port}"),
        port_fallback,
        port_source,
        script: script.to_string(),
    }
}

/// The Vite config file's name and contents, if one exists at the app root.
fn read_vite_config(app_dir: &Path) -> Option<(String, String)> {
    for name in VITE_CONFIG_FILES {
        if let Ok(text) = fs::read_to_string(app_dir.join(name)) {
            return Some((name.to_string(), text));
        }
    }
    None
}

/// The `server.port` from a Vite config's text (a simple regex — the
/// config is code, not data, so this is a heuristic).
fn vite_config_port(config: &str) -> Option<u16> {
    let re = Regex::new(r"port\s*:\s*(\d{2,5})").ok()?;
    let cap = re.captures(config)?;
    cap[1].parse().ok()
}

/// The `build.outDir` from a Vite config's text, if set.
fn vite_config_out_dir(config: &str) -> Option<String> {
    let re = Regex::new(r#"outDir\s*:\s*['"]([^'"]+)['"]"#).ok()?;
    re.captures(config).map(|c| c[1].to_string())
}

/// Whether the Next.js config enables static export (`output: 'export'`).
fn next_static_export(app_dir: &Path) -> bool {
    let Ok(re) = Regex::new(r#"output\s*:\s*['"]export['"]"#) else {
        return false;
    };
    NEXT_CONFIG_FILES
        .iter()
        .any(|name| fs::read_to_string(app_dir.join(name)).is_ok_and(|text| re.is_match(&text)))
}

/// The frontend output directory a rebuild writes to: the `outDir` from
/// the Vite config when set, `out/` for a statically-exported Next.js
/// app, `build/` for Create React App, `dist/` otherwise.
fn detect_out_dir(app_dir: &Path, bundler: Option<Bundler>) -> String {
    if let Some((_, config)) = read_vite_config(app_dir) {
        if let Some(out) = vite_config_out_dir(&config) {
            // Only a plain single-segment directory name is safe to wire
            // into `frontendDist`; anything else (absolute paths, `..`)
            // falls back to `dist/`.
            if !out.is_empty()
                && out != "."
                && out != ".."
                && !out.contains('/')
                && !out.contains('\\')
            {
                return out;
            }
        }
    }
    if bundler == Some(Bundler::Next) && next_static_export(app_dir) {
        return "out".to_string();
    }
    if bundler == Some(Bundler::ReactScripts) {
        return "build".to_string();
    }
    "dist".to_string()
}

/// Carry the original dependencies (minus Electron tooling) into the
/// plan so the preserved build/dev scripts can actually run.
fn merge_deps(pkg: &serde_json::Value, plan: &mut RendererPlan) {
    for (section, target) in [
        ("dependencies", &mut plan.extra_dependencies),
        ("devDependencies", &mut plan.extra_dev_dependencies),
    ] {
        let Some(deps) = pkg.get(section).and_then(|d| d.as_object()) else {
            continue;
        };
        for (name, version) in deps {
            if DROPPED_DEPENDENCIES.contains(&name.as_str()) || name.starts_with("@electron/") {
                continue;
            }
            if let Some(version) = version.as_str() {
                target.push((name.clone(), version.to_string()));
            }
        }
        target.sort();
        target.dedup();
    }
}

/// Whether any renderer source file is newer than the newest file in the
/// build-output directory (beyond [`STALE_MARGIN`]) — the output may be
/// stale.
fn source_newer_than_build(app_dir: &Path, build_dir: &str) -> bool {
    let Some(build_mtime) = newest_mtime(&app_dir.join(build_dir)) else {
        return false;
    };
    let mut candidates: Vec<PathBuf> = ["src", "renderer", "app", "web", "index.html"]
        .iter()
        .map(|c| app_dir.join(c))
        .collect();
    candidates.extend(BUILD_CONFIG_FILES.iter().map(|c| app_dir.join(c)));
    candidates.iter().any(|p| {
        newest_mtime(p).is_some_and(|t| {
            t > build_mtime
                && t.duration_since(build_mtime)
                    .is_ok_and(|d| d > STALE_MARGIN)
        })
    })
}

/// Newest modification time under `path` (a file or a directory tree),
/// skipping `node_modules` / `.git`. `None` when nothing is readable.
fn newest_mtime(path: &Path) -> Option<SystemTime> {
    if path.is_file() {
        return path.metadata().ok().and_then(|m| m.modified().ok());
    }
    if !path.is_dir() {
        return None;
    }
    let mut newest: Option<SystemTime> = None;
    for entry in WalkDir::new(path)
        .into_iter()
        .filter_entry(|e| !is_copy_skipped_dir(e))
    {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                newest = Some(newest.map_or(modified, |n| n.max(modified)));
            }
        }
    }
    newest
}

/// Whether a walked directory is one the copy step never descends into.
fn is_copy_skipped_dir(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return false;
    }
    entry
        .file_name()
        .to_str()
        .is_some_and(|n| COPY_SKIP_DIRS.contains(&n))
}

/// Copy every entry in the plan into the output directory, recording the
/// written files. Returns (files copied, bytes copied, skipped entries).
/// A file that cannot be read is skipped with a note rather than failing
/// the whole conversion; a missing top-level source is skipped the same
/// way (it may have vanished between detection and copy).
fn execute_renderer_plan(
    app_dir: &Path,
    out_dir: &Path,
    plan: &RendererPlan,
    files_written: &mut Vec<PathBuf>,
) -> Result<(usize, u64, Vec<String>)> {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut skipped = Vec::new();
    for spec in &plan.copies {
        let (f, b) = copy_entry(app_dir, out_dir, spec, files_written, &mut skipped)?;
        files += f;
        bytes += b;
    }
    Ok((files, bytes, skipped))
}

/// Copy one [`CopySpec`]. Directories are copied recursively, skipping
/// `node_modules` / version-control dirs, `.env*` files (never copied —
/// they may hold secrets), and symlinks (never followed).
fn copy_entry(
    app_dir: &Path,
    out_dir: &Path,
    spec: &CopySpec,
    files_written: &mut Vec<PathBuf>,
    skipped: &mut Vec<String>,
) -> Result<(usize, u64)> {
    let src = app_dir.join(&spec.from);
    let dst = out_dir.join(&spec.to);
    if src.is_dir() {
        let mut files = 0usize;
        let mut bytes = 0u64;
        for entry in WalkDir::new(&src)
            .into_iter()
            .filter_entry(|e| !is_copy_skipped_dir(e))
        {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type().is_dir() {
                continue;
            }
            let rel_to_app = path.strip_prefix(app_dir).unwrap_or(path);
            if entry.file_type().is_symlink() {
                skipped.push(format!("{} (symlink — not followed)", rel_to_app.display()));
                continue;
            }
            if entry.file_name().to_string_lossy().starts_with(".env") {
                skipped.push(format!(
                    "{} (environment file — never copied)",
                    rel_to_app.display()
                ));
                continue;
            }
            let rel = path.strip_prefix(&src).unwrap_or(path);
            let dest = dst.join(rel);
            match copy_one_file(path, &dest) {
                Ok(n) => {
                    files += 1;
                    bytes += n;
                    files_written.push(dest.strip_prefix(out_dir).unwrap_or(&dest).to_path_buf());
                }
                Err(err) => skipped.push(format!(
                    "{} (could not copy: {err:#})",
                    rel_to_app.display()
                )),
            }
        }
        Ok((files, bytes))
    } else if src.is_file() {
        let n = copy_one_file(&src, &dst)?;
        files_written.push(spec.to.clone());
        Ok((1, n))
    } else {
        skipped.push(format!("{} (not found at copy time)", spec.from.display()));
        Ok((0, 0))
    }
}

/// Copy one file, creating parent directories. Returns the byte count.
fn copy_one_file(src: &Path, dst: &Path) -> Result<u64> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory for {}", dst.display()))?;
    }
    let bytes = fs::copy(src, dst)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dst.display()))?;
    Ok(bytes)
}

/// Human-readable byte count (`"42 B"`, `"1.2 KiB"`, `"3.4 MiB"`).
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// App name → valid Cargo package name: lowercase, runs of non-alphanumeric
/// characters become a single `-`, leading/trailing dashes trimmed. Falls
/// back to `"app"` when nothing is left, and prefixes `app-` when the result
/// would start with a digit (crate names may not).
pub fn sanitize_package_name(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true; // trims leading dashes
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        return "app".to_string();
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return format!("app-{out}");
    }
    out
}

/// Package name → Rust library crate name: `-` becomes `_`, and a trailing
/// `_app` is added if the result would be a Rust keyword (`type::run()`
/// does not compile).
fn lib_crate_name(pkg: &str) -> String {
    let snake = pkg.replace('-', "_");
    if is_rust_keyword(&snake) {
        format!("{snake}_app")
    } else {
        snake
    }
}

/// Channel name → valid snake_case Rust identifier: lowercase, runs of
/// non-alphanumeric characters become a single `_`, leading/trailing
/// underscores trimmed. Prefixed with `_` when it would start with a digit,
/// suffixed with `_cmd` when it would be a Rust keyword.
pub fn sanitize_channel_name(name: &str) -> String {
    let mut out = String::new();
    let mut last_underscore = true; // trims leading underscores
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        return "channel".to_string();
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if is_rust_keyword(&out) {
        out.push_str("_cmd");
    }
    out
}

/// Channel name → camelCase TypeScript key: `"read-file"` → `"readFile"`.
/// Existing case is preserved — only separators transform — so
/// `"appStateUpdate"` stays `"appStateUpdate"` (lowercasing it would
/// break the drop-in replacement claim). The first character is
/// lowercased, and the character after a separator is uppercased.
/// Prefixed with `_` when it would start with a digit.
pub fn to_camel_case(name: &str) -> String {
    let mut out = String::new();
    let mut capitalize_next = false;
    let mut first = true;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if first {
                out.push(c.to_ascii_lowercase());
                first = false;
            } else if capitalize_next {
                out.push(c.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                out.push(c);
            }
        } else {
            capitalize_next = true;
        }
    }
    if out.is_empty() {
        return "channel".to_string();
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Channel name → PascalCase TypeScript key: `"wp-login"` → `"WpLogin"`,
/// `"appStateUpdate"` → `"AppStateUpdate"`. Used for the `mainEvents`
/// `on…` wrappers.
fn to_pascal_case(name: &str) -> String {
    let camel = to_camel_case(name);
    let mut chars = camel.chars();
    match chars.next() {
        Some(first) => {
            let mut out = first.to_ascii_uppercase().to_string();
            out.push_str(chars.as_str());
            out
        }
        None => camel,
    }
}

/// Whether `s` is a Rust keyword (strict, reserved, or weak keywords that
/// cannot be used as a plain identifier).
fn is_rust_keyword(s: &str) -> bool {
    matches!(
        s,
        "as" | "break"
            | "const"
            | "continue"
            | "crate"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "async"
            | "await"
            | "dyn"
            | "abstract"
            | "become"
            | "box"
            | "do"
            | "final"
            | "macro"
            | "override"
            | "priv"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
            | "try"
            | "gen"
    )
}

/// Escape a string for embedding in a Rust `"..."` literal (also valid for
/// a TypeScript `"..."` literal for the characters we escape).
fn rust_str_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

/// Make a string safe to embed in a `//` or `/* */` comment: a channel name
/// containing `*/` or a newline must not break the generated source.
fn comment_safe(s: &str) -> String {
    s.replace("*/", "* /").replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(api: &str) -> Finding {
        Finding {
            api: api.to_string(),
            category: String::new(),
            file: "main.js".to_string(),
            line: 1,
            classification: Classification::Auto,
            tauri_target: None,
            note: String::new(),
            remote_mediated: false,
            occurrences: 1,
        }
    }

    #[test]
    fn package_name_sanitization() {
        assert_eq!(
            sanitize_package_name("minimal-electron-app"),
            "minimal-electron-app"
        );
        assert_eq!(sanitize_package_name("My Cool App!"), "my-cool-app");
        assert_eq!(sanitize_package_name(""), "app");
        assert_eq!(sanitize_package_name("___"), "app");
        assert_eq!(sanitize_package_name("9lives"), "app-9lives");
        assert_eq!(sanitize_package_name("  spaced  out  "), "spaced-out");
        assert_eq!(sanitize_package_name("a--b__c"), "a-b-c");
    }

    #[test]
    fn channel_name_sanitization() {
        assert_eq!(sanitize_channel_name("read-file"), "read_file");
        assert_eq!(sanitize_channel_name("9lives"), "_9lives");
        assert_eq!(sanitize_channel_name("match"), "match_cmd");
        assert_eq!(sanitize_channel_name("type"), "type_cmd");
        assert_eq!(sanitize_channel_name(""), "channel");
        assert_eq!(sanitize_channel_name("a--b"), "a_b");
        assert_eq!(sanitize_channel_name("get_version"), "get_version");
        assert_eq!(sanitize_channel_name("---"), "channel");
    }

    #[test]
    fn camel_case_conversion() {
        assert_eq!(to_camel_case("read-file"), "readFile");
        assert_eq!(to_camel_case("get-version"), "getVersion");
        // Existing camelCase is preserved, not flattened.
        assert_eq!(to_camel_case("appStateUpdate"), "appStateUpdate");
        assert_eq!(to_camel_case("wpLogin"), "wpLogin");
        assert_eq!(to_camel_case("AppState"), "appState");
        assert_eq!(to_camel_case("get_version"), "getVersion");
        assert_eq!(to_camel_case("9lives"), "_9lives");
        assert_eq!(to_camel_case(""), "channel");
    }

    #[test]
    fn pascal_case_conversion() {
        assert_eq!(to_pascal_case("wpLogin"), "WpLogin");
        assert_eq!(to_pascal_case("app-command"), "AppCommand");
        assert_eq!(to_pascal_case("appStateUpdate"), "AppStateUpdate");
    }

    #[test]
    fn lib_crate_name_avoids_keywords() {
        assert_eq!(
            lib_crate_name("minimal-electron-app"),
            "minimal_electron_app"
        );
        assert_eq!(lib_crate_name("match"), "match_app");
        assert_eq!(lib_crate_name("type"), "type_app");
    }

    #[test]
    fn plugin_detection_from_findings() {
        let findings = vec![
            finding("dialog.showOpenDialog"),
            finding("electron-store"),
            finding("BrowserWindow"),
        ];
        let plugins = detect_plugins(&findings, &[], &[]);
        assert!(plugins.dialog);
        assert!(plugins.store);
        assert!(!plugins.opener);
        assert!(!plugins.updater);
        assert!(!plugins.notification);
    }

    #[test]
    fn plugin_detection_from_native_modules() {
        // Native addons have no plugin equivalent, so they drive nothing —
        // but the pure-JS port-needing deps still pull in their plugins.
        let plugins = detect_plugins(
            &[finding("BrowserWindow")],
            &["serialport".to_string()],
            &["electron-store".to_string(), "electron-updater".to_string()],
        );
        assert!(plugins.store);
        assert!(plugins.updater);
        assert!(!plugins.dialog);
    }

    #[test]
    fn plugin_detection_covers_new_b13_plugins() {
        let findings = vec![
            finding("app.requestSingleInstanceLock"),
            finding("app.setAsDefaultProtocolClient"),
            finding("electron-window-state"),
            finding("globalShortcut"),
            finding("clipboard"),
        ];
        let plugins = detect_plugins(&findings, &[], &[]);
        assert!(plugins.single_instance);
        assert!(plugins.deep_link);
        assert!(plugins.window_state);
        assert!(plugins.global_shortcut);
        assert!(plugins.clipboard);
        assert!(!plugins.opener);
    }

    #[test]
    fn plugin_detection_is_empty_without_matches() {
        let plugins = detect_plugins(
            &[finding("BrowserWindow"), finding("ipcMain.handle")],
            &["serialport".to_string()],
            &[],
        );
        assert_eq!(plugins, Plugins::default());
    }

    #[test]
    fn tray_does_not_pull_in_a_v1_plugin() {
        // The mapping table names the Tauri v1 `tauri-plugin-system-tray`;
        // including it would break the generated build.
        let plugins = detect_plugins(&[finding("Tray")], &[], &[]);
        assert_eq!(plugins, Plugins::default());
    }

    #[test]
    fn version_sanitization() {
        assert_eq!(sanitize_version(None), "0.1.0");
        assert_eq!(sanitize_version(Some("")), "0.1.0");
        assert_eq!(sanitize_version(Some("2.27.1")), "2.27.1");
        assert_eq!(sanitize_version(Some("1.0")), "1.0.0");
        assert_eq!(sanitize_version(Some("3")), "3.0.0");
        assert_eq!(sanitize_version(Some("1.2.3-beta.1")), "1.2.3-beta.1");
        assert_eq!(sanitize_version(Some("not-a-version")), "0.1.0");
        assert_eq!(sanitize_version(Some("1.2.3.4")), "0.1.0");
    }

    #[test]
    fn capabilities_grant_core_and_included_plugins() {
        let plugins = Plugins {
            dialog: true,
            updater: true,
            ..Plugins::default()
        };
        let json = gen_capabilities_json(&plugins, &test_report()).expect("capabilities");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(value["identifier"], "default");
        let permissions: Vec<&str> = value["permissions"]
            .as_array()
            .expect("permissions array")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(permissions.contains(&"core:default"));
        assert!(permissions.contains(&"dialog:default"));
        assert!(permissions.contains(&"updater:default"));
        assert!(!permissions.contains(&"opener:default"));
        assert_eq!(value["windows"], serde_json::json!(["main"]));
    }

    #[test]
    fn capabilities_skip_single_instance_permission() {
        // Regression: tauri-plugin-single-instance exposes no frontend
        // commands, so it has no permission set — emitting
        // "single-instance:default" made the Tauri build script fail on the
        // Simplenote battle-test re-run (2026-11-25). Every other plugin
        // keeps its :default set.
        let plugins = Plugins {
            clipboard: true,
            deep_link: true,
            dialog: true,
            global_shortcut: true,
            notification: true,
            opener: true,
            single_instance: true,
            store: true,
            updater: true,
            window_state: true,
        };
        let json = gen_capabilities_json(&plugins, &test_report()).expect("capabilities");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let permissions: Vec<&str> = value["permissions"]
            .as_array()
            .expect("permissions array")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(!permissions.contains(&"single-instance:default"));
        for expected in [
            "core:default",
            "clipboard-manager:default",
            "deep-link:default",
            "dialog:default",
            "global-shortcut:default",
            "notification:default",
            "opener:default",
            "store:default",
            "updater:default",
            "window-state:default",
        ] {
            assert!(permissions.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn listeners_rs_registers_every_on_channel() {
        let channels = vec![
            IpcChannel {
                name: "appStateUpdate".to_string(),
                direction: "on".to_string(),
                file: "desktop/app.js".to_string(),
                line: 114,
            },
            IpcChannel {
                name: "reload".to_string(),
                direction: "on".to_string(),
                file: "desktop/app.js".to_string(),
                line: 160,
            },
        ];
        let rs = gen_listeners_rs(&channels);
        assert!(rs.contains("use tauri::Listener;"));
        assert!(rs.contains("pub fn register_listeners(app: &tauri::App)"));
        assert!(rs.contains("app.listen(\"appStateUpdate\""));
        assert!(rs.contains("app.listen(\"reload\""));
        assert!(rs.contains("desktop/app.js:114"));
        assert!(rs.contains("TODO(e2r)"));
    }

    #[test]
    fn lib_rs_wires_listeners_and_theme_command() {
        let handle_cmds = vec![(
            IpcChannel {
                name: "read-file".to_string(),
                direction: "handle".to_string(),
                file: "main.js".to_string(),
                line: 1,
            },
            "read_file".to_string(),
        )];
        let rs = gen_lib_rs(&handle_cmds, &Plugins::default(), false, true, true);
        assert!(rs.contains("mod listeners;"));
        assert!(rs.contains("listeners::register_listeners(app);"));
        assert!(rs.contains("commands::read_file, commands::set_theme"));
        let rs = gen_lib_rs(&[], &Plugins::default(), false, false, false);
        assert!(!rs.contains("mod listeners;"));
        assert!(!rs.contains("invoke_handler"));
        assert!(!rs.contains(".setup"));
    }

    #[test]
    fn commands_rs_theme_command_uses_set_theme() {
        let rs = gen_commands_rs(&[], true);
        assert!(rs.contains("pub fn set_theme(window: tauri::Window, theme: String)"));
        assert!(rs.contains("window.set_theme(parsed)"));
        assert!(rs.contains("tauri::Theme::Dark"));
        assert!(rs.contains("tauri::Theme::Light"));
        let rs = gen_commands_rs(&[], false);
        assert!(!rs.contains("set_theme"));
    }

    #[test]
    fn comment_safe_neutralizes_comment_terminators() {
        assert_eq!(comment_safe("a*/b\nc"), "a* /b c");
    }

    #[test]
    fn placeholder_icon_is_a_valid_png() {
        let png = gen_placeholder_icon_png();
        // PNG signature.
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        // First chunk is IHDR: 128x128, 8-bit RGBA.
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 128);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 128);
        assert_eq!(png[24], 8);
        assert_eq!(png[25], 6);
        // Last chunk is IEND (with its well-known CRC).
        assert_eq!(
            &png[png.len() - 12..],
            &[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82]
        );
    }

    // ---- Renderer carry-over tests ----

    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A fresh empty directory under the system temp dir, unique per call.
    /// Test fixtures here are tiny text files, so the 512 MB tmpfs is fine.
    /// Callers remove it when done.
    fn fresh_test_dir(tag: &str) -> PathBuf {
        let n = TEST_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("e2r-convert-test-{tag}-{}-{n}", std::process::id()));
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

    fn test_report() -> ScanReport {
        ScanReport {
            app_name: "test-app".to_string(),
            app_version: None,
            product_name: None,
            app_id: None,
            electron_version: None,
            main_entry: Some("main.js".to_string()),
            files_scanned: 0,
            ipc_channels: Vec::new(),
            main_to_renderer: Vec::new(),
            findings: Vec::new(),
            preload: crate::model::PreloadInfo::default(),
            native_modules: Vec::new(),
            port_needing_deps: Vec::new(),
            build_tooling: Vec::new(),
            dev_url: None,
            feasibility_score: 100,
            verdict: "Good fit — mostly automatic".to_string(),
            windows: Vec::new(),
            menu: None,
            menus: Vec::new(),
        }
    }

    fn pkg_json(text: &str) -> Option<serde_json::Value> {
        serde_json::from_str(text).ok()
    }

    #[test]
    fn renderer_detection_prefers_build_output() {
        let app = fresh_test_dir("build-output");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","scripts":{"build":"vite build","dev":"vite"},"devDependencies":{"vite":"^5.0.0"}}"#,
        );
        write_test_file(&app, "dist/index.html", "<h1>built</h1>");
        write_test_file(&app, "dist/assets/app.js", "console.log(1);");
        write_test_file(&app, "src/main.js", "console.log(2);");
        write_test_file(&app, "vite.config.js", "export default {}");
        let pkg = read_package_json_value(&app);
        let plan = detect_renderer(&app, &pkg, &test_report());
        assert_eq!(plan.kind, RendererKind::BuiltOutput);
        assert_eq!(plan.frontend_dir, "dist");
        assert_eq!(plan.frontend_dist, "../dist");
        // The source comes along too, so rebuilds work.
        assert!(plan.source_carried);
        assert_eq!(plan.before_build_command, "npm run build:frontend");
        assert!(plan
            .copies
            .iter()
            .any(|c| c.from.as_path() == Path::new("dist")));
        assert!(plan
            .copies
            .iter()
            .any(|c| c.from.as_path() == Path::new("src")));
        // Vite default port (no --port, no server.port in the config).
        assert_eq!(plan.dev_url.as_deref(), Some("http://localhost:5173"));
        assert!(!plan.dev_port_fallback);
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn renderer_detection_source_only() {
        let app = fresh_test_dir("source-only");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","scripts":{"build":"vite build"},"devDependencies":{"vite":"^5.0.0"}}"#,
        );
        write_test_file(&app, "src/index.html", "<h1>src</h1>");
        write_test_file(
            &app,
            "vite.config.js",
            "export default { build: { outDir: 'build' } }",
        );
        let pkg = read_package_json_value(&app);
        let plan = detect_renderer(&app, &pkg, &test_report());
        assert_eq!(plan.kind, RendererKind::Source);
        // outDir comes from the Vite config, not the default.
        assert_eq!(plan.frontend_dir, "build");
        assert_eq!(plan.frontend_dist, "../build");
        assert_eq!(plan.before_build_command, "npm run build:frontend");
        assert!(plan
            .extra_scripts
            .iter()
            .any(|(n, c)| n == "build:frontend" && c == "vite build"));
        assert!(plan.extra_dev_dependencies.iter().any(|(n, _)| n == "vite"));
        assert_eq!(plan.dev_url, None); // no dev script
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn renderer_detection_static_only() {
        let app = fresh_test_dir("static-only");
        write_test_file(&app, "package.json", r#"{"name":"t","main":"main.js"}"#);
        write_test_file(&app, "index.html", "<h1>static</h1>");
        write_test_file(&app, "styles.css", "body {}");
        write_test_file(&app, "renderer.js", "console.log(1);");
        write_test_file(&app, "main.js", "require('electron');");
        write_test_file(&app, "preload.js", "console.log(2);");
        let mut report = test_report();
        report.preload.files = vec!["preload.js".to_string()];
        let pkg = read_package_json_value(&app);
        let plan = detect_renderer(&app, &pkg, &report);
        assert_eq!(plan.kind, RendererKind::Static);
        assert_eq!(plan.frontend_dir, "dist");
        let dests: Vec<PathBuf> = plan.copies.iter().map(|c| c.to.clone()).collect();
        assert!(dests.contains(&PathBuf::from("dist/index.html")));
        assert!(dests.contains(&PathBuf::from("dist/styles.css")));
        assert!(dests.contains(&PathBuf::from("dist/renderer.js")));
        assert!(
            !dests.iter().any(|d| d.ends_with("main.js")),
            "main entry must not be copied into dist/: {dests:?}"
        );
        assert!(
            !dests.iter().any(|d| d.ends_with("preload.js")),
            "preload must not be copied into dist/: {dests:?}"
        );
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn renderer_detection_nothing_found() {
        let app = fresh_test_dir("nothing");
        write_test_file(&app, "package.json", r#"{"name":"t","main":"main.js"}"#);
        write_test_file(&app, "main.js", "console.log(1);");
        let pkg = read_package_json_value(&app);
        let plan = detect_renderer(&app, &pkg, &test_report());
        assert_eq!(plan.kind, RendererKind::Placeholder);
        assert!(plan.copies.is_empty());
        assert_eq!(plan.frontend_dir, "dist");
        // The clear note lives in the summary (and `convert` adds a
        // matching warning); there are no caveats for a bare app.
        assert!(plan.summary.contains("No renderer files were found"));
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn dev_port_from_script_beats_config_and_defaults() {
        let app = fresh_test_dir("port-script");
        write_test_file(
            &app,
            "vite.config.js",
            "export default { server: { port: 5199 } }",
        );
        let pkg = pkg_json(
            r#"{"scripts":{"dev":"vite --port 4000"},"devDependencies":{"vite":"^5.0.0"}}"#,
        )
        .unwrap();
        let dev = detect_dev_server(&app, &pkg).expect("dev server");
        assert_eq!(dev.url, "http://localhost:4000");
        assert!(!dev.port_fallback);
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn dev_port_from_vite_config() {
        let app = fresh_test_dir("port-config");
        write_test_file(
            &app,
            "vite.config.ts",
            "export default { server: { port: 5199 } }",
        );
        let pkg =
            pkg_json(r#"{"scripts":{"dev":"vite"},"devDependencies":{"vite":"^5.0.0"}}"#).unwrap();
        let dev = detect_dev_server(&app, &pkg).expect("dev server");
        assert_eq!(dev.url, "http://localhost:5199");
        assert!(dev.port_source.contains("vite.config.ts"));
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn dev_port_falls_back_to_bundler_default_then_1420() {
        let app = fresh_test_dir("port-default");
        let pkg = pkg_json(
            r#"{"scripts":{"dev":"webpack serve"},"devDependencies":{"webpack-dev-server":"^5.0.0"}}"#,
        )
        .unwrap();
        let dev = detect_dev_server(&app, &pkg).expect("dev server");
        assert_eq!(dev.url, "http://localhost:8080");
        assert!(!dev.port_fallback);
        let pkg = pkg_json(r#"{"scripts":{"dev":"serve public"}}"#).unwrap();
        let dev = detect_dev_server(&app, &pkg).expect("dev server");
        assert_eq!(dev.url, "http://localhost:1420");
        assert!(dev.port_fallback);
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn electron_dev_script_is_not_a_dev_server() {
        let app = fresh_test_dir("port-electron");
        let pkg = pkg_json(r#"{"scripts":{"dev":"electron ."}}"#).unwrap();
        assert!(detect_dev_server(&app, &pkg).is_none());
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn dev_server_is_found_in_a_workspace_package() {
        // Insomnia shape: the root package has no dev script; the real
        // one is `packages/insomnia`'s `start:dev-server` running
        // `vite dev` (port 3334 from its Vite config).
        let app = fresh_test_dir("port-workspace");
        write_test_file(
            &app,
            "packages/insomnia/package.json",
            r#"{"name":"insomnia","scripts":{"start:dev-server":"vite dev"},"devDependencies":{"vite":"^5.0.0"}}"#,
        );
        write_test_file(
            &app,
            "packages/insomnia/vite.config.ts",
            "export default { server: { port: 3334 } }",
        );
        let pkg = pkg_json(r#"{"name":"root","scripts":{"start":"electron ."}}"#).unwrap();
        let dev = detect_dev_server(&app, &pkg).expect("workspace dev server");
        assert_eq!(dev.url, "http://localhost:3334");
        assert!(dev.port_source.contains("vite.config.ts"));
        assert_eq!(dev.script, "vite dev");
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn electron_only_start_script_in_workspace_is_not_a_dev_server() {
        // Joplin shape: `packages/app-desktop`'s `start` launches
        // Electron, and the root has no dev script — no dev server.
        let app = fresh_test_dir("port-joplin");
        write_test_file(
            &app,
            "packages/app-desktop/package.json",
            r#"{"name":"app-desktop","scripts":{"start":"gulp before-start && electron . --env dev"}}"#,
        );
        let pkg = pkg_json(r#"{"name":"root","scripts":{}}"#).unwrap();
        assert!(detect_dev_server(&app, &pkg).is_none());
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn stale_build_output_is_flagged() {
        use std::fs::File;
        use std::time::Duration;
        let app = fresh_test_dir("stale");
        write_test_file(&app, "package.json", r#"{"name":"t","main":"main.js"}"#);
        write_test_file(&app, "dist/index.html", "<h1>built</h1>");
        write_test_file(&app, "src/main.js", "console.log(1);");
        // dist/ is an hour older than src/: set explicit mtimes so the
        // test does not depend on write order or filesystem granularity.
        let now = SystemTime::now();
        let old = now - Duration::from_secs(3600);
        File::open(app.join("dist/index.html"))
            .expect("open dist index")
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .expect("set old mtime");
        File::open(app.join("src/main.js"))
            .expect("open src main")
            .set_times(std::fs::FileTimes::new().set_modified(now))
            .expect("set new mtime");
        let pkg = read_package_json_value(&app);
        let plan = detect_renderer(&app, &pkg, &test_report());
        assert!(plan.stale_build_output);
        assert!(plan.caveats.iter().any(|c| c.contains("stale")));
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn fresh_build_output_is_not_flagged() {
        use std::fs::File;
        use std::time::Duration;
        let app = fresh_test_dir("fresh");
        write_test_file(&app, "package.json", r#"{"name":"t","main":"main.js"}"#);
        write_test_file(&app, "dist/index.html", "<h1>built</h1>");
        write_test_file(&app, "src/main.js", "console.log(1);");
        // dist/ is newer than src/ (the normal just-built case).
        let now = SystemTime::now();
        let old = now - Duration::from_secs(3600);
        File::open(app.join("src/main.js"))
            .expect("open src main")
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .expect("set old mtime");
        File::open(app.join("dist/index.html"))
            .expect("open dist index")
            .set_times(std::fs::FileTimes::new().set_modified(now))
            .expect("set new mtime");
        let pkg = read_package_json_value(&app);
        let plan = detect_renderer(&app, &pkg, &test_report());
        assert!(!plan.stale_build_output);
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn copy_entry_skips_node_modules_and_env_files() {
        let app = fresh_test_dir("copy-skip");
        let out = fresh_test_dir("copy-skip-out");
        write_test_file(&app, "dist/index.html", "<h1>x</h1>");
        write_test_file(&app, "dist/node_modules/foo/index.js", "x");
        write_test_file(&app, "dist/.env", "SECRET=1");
        write_test_file(&app, "dist/.env.local", "SECRET=1");
        let spec = CopySpec {
            from: PathBuf::from("dist"),
            to: PathBuf::from("dist"),
        };
        let mut files_written = Vec::new();
        let mut skipped = Vec::new();
        let (count, _) =
            copy_entry(&app, &out, &spec, &mut files_written, &mut skipped).expect("copy");
        assert_eq!(count, 1);
        assert!(out.join("dist/index.html").is_file());
        assert!(!out.join("dist/node_modules").exists());
        assert!(!out.join("dist/.env").exists());
        assert!(!out.join("dist/.env.local").exists());
        assert_eq!(skipped.len(), 2, "both .env files should be reported");
        fs::remove_dir_all(&app).ok();
        fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn tauri_conf_uses_renderer_plan() {
        let plan = RendererPlan {
            frontend_dist: "../build".to_string(),
            dev_url: Some("http://localhost:5199".to_string()),
            before_dev_command: "npm run dev:frontend".to_string(),
            before_build_command: "npm run build:frontend".to_string(),
            ..RendererPlan::default()
        };
        let conf = gen_tauri_conf("test-app", &test_report(), &plan).expect("conf");
        let value: serde_json::Value = serde_json::from_str(&conf).expect("parse");
        assert_eq!(value["build"]["frontendDist"], "../build");
        assert_eq!(value["build"]["devUrl"], "http://localhost:5199");
        assert_eq!(value["build"]["beforeDevCommand"], "npm run dev:frontend");
        assert_eq!(
            value["build"]["beforeBuildCommand"],
            "npm run build:frontend"
        );
    }

    #[test]
    fn tauri_conf_dev_url_is_null_without_dev_server() {
        let plan = RendererPlan::default();
        let conf = gen_tauri_conf("test-app", &test_report(), &plan).expect("conf");
        let value: serde_json::Value = serde_json::from_str(&conf).expect("parse");
        assert!(value["build"]["devUrl"].is_null());
    }

    #[test]
    fn package_json_merges_frontend_scripts_deps_and_type() {
        let mut plan = RendererPlan::default();
        plan.extra_scripts
            .push(("build:frontend".into(), "vite build".into()));
        plan.extra_scripts
            .push(("dev:frontend".into(), "vite".into()));
        plan.extra_dependencies
            .push(("react".into(), "^18.0.0".into()));
        plan.extra_dev_dependencies
            .push(("vite".into(), "^5.0.0".into()));
        plan.module_type = Some("commonjs".to_string());
        let text = gen_package_json("test-app", &plan, Some("2.27.1")).expect("package.json");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(value["type"], "commonjs");
        assert_eq!(value["version"], "2.27.1");
        assert_eq!(value["scripts"]["build"], "tauri build");
        assert_eq!(value["scripts"]["build:frontend"], "vite build");
        assert_eq!(value["scripts"]["dev:frontend"], "vite");
        assert_eq!(value["dependencies"]["react"], "^18.0.0");
        assert_eq!(value["dependencies"]["@tauri-apps/api"], "^2");
        assert_eq!(value["devDependencies"]["vite"], "^5.0.0");
    }

    #[test]
    fn carried_deps_drop_electron_tooling() {
        let pkg = pkg_json(
            r#"{"dependencies":{"electron":"^30.0.0","react":"^18.0.0"},"devDependencies":{"electron-builder":"^24.0.0","vite":"^5.0.0","@electron/packager":"^1.0.0"}}"#,
        )
        .unwrap();
        let mut plan = RendererPlan::default();
        merge_deps(&pkg, &mut plan);
        assert!(plan.extra_dependencies.iter().any(|(n, _)| n == "react"));
        assert!(!plan.extra_dependencies.iter().any(|(n, _)| n == "electron"));
        assert!(plan.extra_dev_dependencies.iter().any(|(n, _)| n == "vite"));
        assert!(!plan
            .extra_dev_dependencies
            .iter()
            .any(|(n, _)| n == "electron-builder"));
        assert!(!plan
            .extra_dev_dependencies
            .iter()
            .any(|(n, _)| n == "@electron/packager"));
    }

    #[test]
    fn convert_carries_static_renderer_end_to_end() {
        let app = fresh_test_dir("e2e-static");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"static-test","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\nnew BrowserWindow({ width: 800, height: 600 });\n",
        );
        write_test_file(&app, "index.html", "<h1>Real Renderer</h1>");
        let out = fresh_test_dir("e2e-static-out");
        let report = convert(&app, &out).expect("convert");
        let dist = fs::read_to_string(out.join("dist/index.html")).expect("read dist");
        assert!(
            dist.contains("Real Renderer"),
            "dist should be the real renderer, got: {dist}"
        );
        let notes = fs::read_to_string(out.join("MIGRATION-NOTES.md")).expect("read notes");
        assert!(!notes.contains("was NOT copied"));
        assert!(notes.contains("static renderer"));
        assert!(report
            .files_written
            .iter()
            .any(|f| f == Path::new("dist/index.html")));
        fs::remove_dir_all(&app).ok();
        fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn convert_without_renderer_writes_placeholder_with_clear_note() {
        let app = fresh_test_dir("e2e-placeholder");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"no-renderer","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(&app, "main.js", "const { app } = require('electron');\n");
        let out = fresh_test_dir("e2e-placeholder-out");
        let report = convert(&app, &out).expect("convert");
        let dist = fs::read_to_string(out.join("dist/index.html")).expect("read dist");
        assert!(dist.contains("No renderer files were found"));
        let notes = fs::read_to_string(out.join("MIGRATION-NOTES.md")).expect("read notes");
        assert!(notes.contains("No renderer files were found"));
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("No renderer files")));
        fs::remove_dir_all(&app).ok();
        fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn tauri_conf_carries_identity_and_omits_unknown_dimensions() {
        let mut report = test_report();
        report.app_name = "simplenote".to_string();
        report.app_version = Some("2.27.1".to_string());
        report.product_name = Some("Simplenote".to_string());
        report.app_id = Some("com.automattic.simplenote".to_string());
        report.windows = vec![crate::model::WindowInfo {
            width: 1024,
            height: 768,
            width_known: true,
            height_known: true,
            from_window_state: true,
            min_width: Some(370),
            min_height: Some(520),
            background_color: Some("#fff".to_string()),
            icon: None,
            title: None,
            resizable: true,
            frame: true,
            file: "desktop/app.js".to_string(),
            line: 75,
        }];
        let conf = gen_tauri_conf("simplenote", &report, &RendererPlan::default()).expect("conf");
        let value: serde_json::Value = serde_json::from_str(&conf).expect("parse");
        assert_eq!(value["productName"], "Simplenote");
        assert_eq!(value["version"], "2.27.1");
        assert_eq!(value["identifier"], "com.automattic.simplenote");
        let win = &value["app"]["windows"][0];
        assert_eq!(win["width"], 1024);
        assert_eq!(win["height"], 768);
        assert_eq!(win["minWidth"], 370);
        assert_eq!(win["minHeight"], 520);
        assert_eq!(win["backgroundColor"], "#fff");

        // Unknown dimensions are omitted, not faked as 800x600; a named
        // background color is dropped (Tauri only parses hex).
        let mut report = test_report();
        report.windows = vec![crate::model::WindowInfo {
            width: 800,
            height: 600,
            width_known: false,
            height_known: false,
            from_window_state: false,
            min_width: None,
            min_height: None,
            background_color: Some("white".to_string()),
            icon: None,
            title: None,
            resizable: true,
            frame: true,
            file: "main.js".to_string(),
            line: 1,
        }];
        let conf = gen_tauri_conf("test-app", &report, &RendererPlan::default()).expect("conf");
        let value: serde_json::Value = serde_json::from_str(&conf).expect("parse");
        let win = &value["app"]["windows"][0];
        assert!(win.get("width").is_none(), "unknown width must be omitted");
        assert!(
            win.get("height").is_none(),
            "unknown height must be omitted"
        );
        assert!(
            win.get("backgroundColor").is_none(),
            "named colors are dropped"
        );
        assert_eq!(value["version"], "0.1.0", "no version declared → fallback");
        assert_eq!(value["identifier"], "com.e2r.test-app");
    }

    #[test]
    fn bridge_has_no_proxy_and_uses_typed_preload_shapes() {
        // Simplenote-shaped report: camelCase .on channels, a webContents.send
        // channel, and an `electron` preload key with a recovered shape.
        let mut report = test_report();
        report.ipc_channels = vec![
            IpcChannel {
                name: "appStateUpdate".to_string(),
                direction: "on".to_string(),
                file: "desktop/app.js".to_string(),
                line: 114,
            },
            IpcChannel {
                name: "wpLogin".to_string(),
                direction: "listen".to_string(),
                file: "desktop/preload.js".to_string(),
                line: 45,
            },
        ];
        report.main_to_renderer = vec![IpcChannel {
            name: "appCommand".to_string(),
            direction: "emit".to_string(),
            file: "desktop/app.js".to_string(),
            line: 190,
        }];
        report.preload.uses_context_bridge = true;
        report.preload.exposed_keys = vec!["api".to_string(), "electron".to_string()];
        report.preload.shapes = vec![crate::model::PreloadShape {
            key: "electron".to_string(),
            members: vec![
                crate::model::PreloadMember {
                    name: "confirmLogout".to_string(),
                    kind: crate::model::PreloadMemberKind::Function,
                },
                crate::model::PreloadMember {
                    name: "send".to_string(),
                    kind: crate::model::PreloadMemberKind::Function,
                },
                crate::model::PreloadMember {
                    name: "receive".to_string(),
                    kind: crate::model::PreloadMemberKind::Function,
                },
                crate::model::PreloadMember {
                    name: "isMac".to_string(),
                    kind: crate::model::PreloadMemberKind::PlatformMac,
                },
                crate::model::PreloadMember {
                    name: "isLinux".to_string(),
                    kind: crate::model::PreloadMemberKind::PlatformLinux,
                },
            ],
            file: "desktop/preload.js".to_string(),
            line: 62,
        }];
        let ts = gen_bridge_ts(&report, &[]);
        // No generic Proxy anywhere.
        assert!(
            !ts.contains("new Proxy"),
            "the bridge must never emit a generic Proxy; got:\n{ts}"
        );
        // camelCase channel keys are preserved.
        assert!(ts.contains("appStateUpdate: (payload?: unknown) => emit(\"appStateUpdate\""));
        // Main → renderer events get listen() wrappers.
        assert!(ts.contains("export const mainEvents = {"));
        assert!(ts.contains("onWpLogin:"));
        assert!(ts.contains("onAppCommand:"));
        assert!(ts.contains("listen(\"wpLogin\""));
        // The typed `electron` object: send/receive wired, booleans kept
        // booleans, confirmLogout a loud TODO stub.
        assert!(ts.contains("export const electron = {"));
        assert!(ts.contains("send: (channel: string, data?: unknown) => emit(channel, data)"));
        assert!(ts.contains("isMac: platform() === \"macos\""));
        assert!(ts.contains("isLinux: platform() === \"linux\""));
        assert!(ts.contains("confirmLogout:"));
        assert!(ts.contains("TODO(e2r)"));
        assert!(ts.contains("from \"@tauri-apps/api/os\""));
    }

    #[test]
    fn bridge_emits_no_wrapper_for_unknown_preload_shape() {
        let mut report = test_report();
        report.preload.uses_context_bridge = true;
        report.preload.exposed_keys = vec!["mystery".to_string()];
        let ts = gen_bridge_ts(&report, &[]);
        assert!(!ts.contains("new Proxy"));
        assert!(!ts.contains("export const mystery"));
        assert!(ts.contains("TODO(e2r)"));
    }

    #[test]
    fn dev_url_from_main_process_load_url_is_used_without_a_dev_script() {
        let app = fresh_test_dir("dev-url-loadurl");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"main.js","scripts":{"build":"vite build"},"devDependencies":{"vite":"^5.0.0"}}"#,
        );
        write_test_file(&app, "src/index.html", "<h1>src</h1>");
        let mut report = test_report();
        report.dev_url = Some(crate::model::DevUrlInfo {
            url: "http://localhost:4000".to_string(),
            file: "main.js".to_string(),
            line: 60,
        });
        let pkg = read_package_json_value(&app);
        let plan = detect_renderer(&app, &pkg, &report);
        assert_eq!(plan.dev_url.as_deref(), Some("http://localhost:4000"));
        assert!(!plan.dev_port_fallback);
        assert!(plan.before_dev_command.is_empty(), "no dev script to run");
        assert!(plan.caveats.iter().any(|c| c.contains("start it manually")));
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn convert_copies_the_real_window_icon_when_it_is_a_png() {
        let app = fresh_test_dir("e2e-icon");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"icon-test","main":"main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow } = require('electron');\n\
             new BrowserWindow({ width: 800, height: 600, icon: `${__dirname}/assets/icon.png` });\n",
        );
        // A real (tiny) PNG: the placeholder generator's output reused as
        // the app's icon bytes, so the copy is verifiable by content.
        let icon_bytes = gen_placeholder_icon_png();
        let icon_path = app.join("assets/icon.png");
        fs::create_dir_all(icon_path.parent().unwrap()).expect("mkdir assets");
        fs::write(&icon_path, &icon_bytes).expect("write icon");
        let out = fresh_test_dir("e2e-icon-out");
        let report = convert(&app, &out).expect("convert");
        let copied = fs::read(out.join("src-tauri/icons/icon.png")).expect("read icon");
        assert_eq!(
            copied, icon_bytes,
            "the real icon must be copied, not the placeholder"
        );
        let notes = fs::read_to_string(out.join("MIGRATION-NOTES.md")).expect("read notes");
        assert!(notes.contains("assets/icon.png"));
        assert!(
            !report
                .warnings
                .iter()
                .any(|w| w.contains("could not be used")),
            "no icon warning when the real icon was copied; warnings: {:?}",
            report.warnings
        );
        fs::remove_dir_all(&app).ok();
        fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn convert_generates_listeners_and_capabilities_end_to_end() {
        let app = fresh_test_dir("e2e-listeners");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"listener-test","main":"main.js","version":"1.0.0","dependencies":{"electron-updater":"^6.0.0"}}"#,
        );
        write_test_file(
            &app,
            "main.js",
            "const { app, BrowserWindow, ipcMain, dialog } = require('electron');\n\
             const { autoUpdater } = require('electron-updater');\n\
             autoUpdater.checkForUpdates();\n\
             ipcMain.on('appStateUpdate', () => {});\n\
             ipcMain.on('reload', () => {});\n\
             new BrowserWindow({ width: 800, height: 600 });\n",
        );
        let out = fresh_test_dir("e2e-listeners-out");
        convert(&app, &out).expect("convert");
        let listeners =
            fs::read_to_string(out.join("src-tauri/src/listeners.rs")).expect("listeners.rs");
        assert!(listeners.contains("app.listen(\"appStateUpdate\""));
        assert!(listeners.contains("app.listen(\"reload\""));
        let lib = fs::read_to_string(out.join("src-tauri/src/lib.rs")).expect("lib.rs");
        assert!(lib.contains("mod listeners;"));
        assert!(lib.contains("listeners::register_listeners(app);"));
        let cap = fs::read_to_string(out.join("src-tauri/capabilities/default.json"))
            .expect("capabilities");
        let cap_value: serde_json::Value = serde_json::from_str(&cap).expect("parse cap");
        let permissions: Vec<&str> = cap_value["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(permissions.contains(&"core:default"));
        assert!(permissions.contains(&"updater:default"));
        let bridge = fs::read_to_string(out.join("src/lib/e2r-bridge.ts")).expect("bridge");
        assert!(!bridge.contains("new Proxy"));
        assert!(bridge.contains("appStateUpdate:"));
        fs::remove_dir_all(&app).ok();
        fs::remove_dir_all(&out).ok();
    }

    // ---- Proof-campaign fix round: renderer detection ----------------

    #[test]
    fn renderer_detection_finds_joplin_layout() {
        // Joplin: the renderer lives in packages/app-desktop/ with its
        // index.html at the package root and the UI in gui/.
        let app = fresh_test_dir("renderer-joplin");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"joplin","main":"packages/app-desktop/main.js","version":"3.0.0"}"#,
        );
        write_test_file(
            &app,
            "packages/app-desktop/index.html",
            "<!doctype html><html><body></body></html>\n",
        );
        write_test_file(
            &app,
            "packages/app-desktop/gui/MainScreen.tsx",
            "export function MainScreen() { return null; }\n",
        );
        write_test_file(
            &app,
            "packages/app-desktop/main.js",
            "const { app } = require('electron');\n",
        );
        let pkg = read_package_json_value(&app);
        let mut report = test_report();
        report.main_entry = Some("packages/app-desktop/main.js".to_string());
        let plan = detect_renderer(&app, &pkg, &report);
        assert_eq!(plan.source_dir.as_deref(), Some("packages/app-desktop"));
        assert!(matches!(plan.kind, RendererKind::Source));
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn renderer_detection_finds_github_desktop_layout() {
        // GitHub Desktop: the renderer is app/src/ui/ (app.tsx); the
        // only index.html is the static shell app/static/index.html,
        // which must resolve to app/ — and must lose to the real entry.
        let app = fresh_test_dir("renderer-ghdesktop");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"github-desktop","main":"app/main.js","version":"3.0.0"}"#,
        );
        write_test_file(
            &app,
            "app/src/ui/app.tsx",
            "import React from 'react';\nexport function App() { return null; }\n",
        );
        write_test_file(
            &app,
            "app/static/index.html",
            "<!doctype html><html><body></body></html>\n",
        );
        write_test_file(
            &app,
            "app/src/main-process/main.ts",
            "import { app } from 'electron';\n",
        );
        let pkg = read_package_json_value(&app);
        let mut report = test_report();
        report.main_entry = Some("app/main.js".to_string());
        let plan = detect_renderer(&app, &pkg, &report);
        assert_eq!(plan.source_dir.as_deref(), Some("app/src/ui"));
        assert!(matches!(plan.kind, RendererKind::Source));
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn renderer_detection_finds_signal_ts_layout() {
        // Signal: the renderer is ts/ — no entry file, just a dense
        // tree of .tsx files (the main process lives elsewhere).
        let app = fresh_test_dir("renderer-signal");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"signal","main":"app/main.main.ts","version":"7.0.0"}"#,
        );
        for name in ["A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L"] {
            write_test_file(
                &app,
                &format!("ts/{name}.tsx"),
                "export function Component() { return null; }\n",
            );
        }
        write_test_file(
            &app,
            "ts/main/preload.ts",
            "import { contextBridge } from 'electron';\n",
        );
        let pkg = read_package_json_value(&app);
        let mut report = test_report();
        report.main_entry = Some("app/main.main.ts".to_string());
        let plan = detect_renderer(&app, &pkg, &report);
        assert_eq!(plan.source_dir.as_deref(), Some("ts"));
        assert!(matches!(plan.kind, RendererKind::Source));
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn renderer_detection_excludes_decoy_sub_apps() {
        // Campaign regression (2026-11-25): the v2 scan/convert copied
        // the WRONG renderer for 3/5 apps — Joplin `packages/pdf-viewer`
        // (a sibling sub-package with its own `main`), Signal
        // `sticker-creator/src` (a mini-app inside a standalone package
        // with no `main`), Insomnia
        // `examples/insomnia-plugin-sandbox-demo` (an examples dir) —
        // while the real renderer (Joplin's `packages/app-desktop/gui/`,
        // 154 of the package's 158 `.tsx` files, no entry file at all)
        // went unpicked. All three decoy shapes are here at once; the
        // real renderer is dense but entry-less.
        let app = fresh_test_dir("renderer-decoys");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"decoys","private":true,"version":"1.0.0"}"#,
        );
        // The app package: main entry + electron dep.
        write_test_file(
            &app,
            "packages/app-desktop/package.json",
            r#"{"name":"@test/app-desktop","main":"main.js","version":"1.0.0","dependencies":{"electron":"^43.0.0"}}"#,
        );
        write_test_file(
            &app,
            "packages/app-desktop/main.js",
            "const { app, BrowserWindow } = require('electron');\n",
        );
        // The real renderer: 12 `.tsx` files under gui/, 1 plugin
        // webview outside it (12/13 = 92% — above the collapse bar,
        // mirroring Joplin's 154/158).
        for i in 0..12 {
            write_test_file(
                &app,
                &format!("packages/app-desktop/gui/Screen{i}.tsx"),
                "export function Screen() { return null; }\n",
            );
        }
        write_test_file(
            &app,
            "packages/app-desktop/plugins/UserWebview.tsx",
            "export function UserWebview() { return null; }\n",
        );
        // Decoy 1: sibling sub-package WITH a main field and a strict
        // entry file (index.tsx) — the old code ranked it first.
        write_test_file(
            &app,
            "packages/pdf-viewer/package.json",
            r#"{"name":"@test/pdf-viewer","main":"dist/main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "packages/pdf-viewer/index.tsx",
            "export function PdfViewer() { return null; }\n",
        );
        write_test_file(
            &app,
            "packages/pdf-viewer/Viewer.tsx",
            "export function Viewer() { return null; }\n",
        );
        // Decoy 2: a mini-app inside a standalone package (no main) —
        // its src/ has strict entries but no package.json of its own.
        write_test_file(
            &app,
            "sticker-creator/package.json",
            r#"{"name":"sticker-creator","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "sticker-creator/src/App.tsx",
            "export function App() { return null; }\n",
        );
        write_test_file(
            &app,
            "sticker-creator/src/main.tsx",
            "export function main() {}\n",
        );
        // Decoy 3: an examples dir with an index.html and a strict entry.
        write_test_file(
            &app,
            "examples/plugin-demo/package.json",
            r#"{"name":"plugin-demo","main":"index.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "examples/plugin-demo/index.html",
            "<!doctype html><html><body></body></html>\n",
        );
        write_test_file(
            &app,
            "examples/plugin-demo/App.tsx",
            "export function App() { return null; }\n",
        );
        let pkg = read_package_json_value(&app);
        let mut report = test_report();
        report.main_entry = Some("packages/app-desktop/main.js".to_string());
        let plan = detect_renderer(&app, &pkg, &report);
        assert_eq!(
            plan.source_dir.as_deref(),
            Some("packages/app-desktop/gui"),
            "decoy sub-apps must lose to the real renderer"
        );
        assert!(matches!(plan.kind, RendererKind::Source));
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn renderer_detection_keeps_internal_packages_of_the_app_package() {
        // Mailspring shape (2026-11-25): the app's own plugin UI lives
        // in `internal_packages/*` — 53 sub-packages, each with its own
        // `package.json` AND a `main` field — nested INSIDE the app
        // package. They are the shipped UI, not sibling sub-apps: the
        // first version of the foreign-package rule excluded them and
        // narrowed Mailspring's renderer from `app/` to `app/src/`,
        // dropping 215 touch points. A sibling package outside the app
        // package (here `packages/pdf-viewer`) must still lose.
        let app = fresh_test_dir("renderer-internal-pkgs");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"t","main":"app/src/browser/main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "app/package.json",
            r#"{"name":"app","main":"./src/browser/main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "app/src/browser/main.js",
            "const { app, BrowserWindow } = require('electron');\n",
        );
        for i in 0..12 {
            write_test_file(
                &app,
                &format!("app/src/Screen{i}.tsx"),
                "export function Screen() { return null; }\n",
            );
        }
        for pkg in ["account-sidebar", "activity"] {
            write_test_file(
                &app,
                &format!("app/internal_packages/{pkg}/package.json"),
                &format!(r#"{{"name":"{pkg}","main":"./lib/main","version":"1.0.0"}}"#),
            );
            // Ten files each, one level DEEPER than the old fixture
            // (`lib/deep/` — file depth 5 from `app/`): the density
            // count must not stop at depth 4, or `app/` undercounts
            // (12) while `internal_packages/` counts all 20 and the
            // container collapse wrongly drops `app/` (the real
            // Mailspring bug: 269 vs 260 counted, 464 vs 308 true).
            for i in 0..10 {
                write_test_file(
                    &app,
                    &format!("app/internal_packages/{pkg}/lib/deep/Panel{i}.tsx"),
                    "export function Panel() { return null; }\n",
                );
            }
        }
        // Decoy: a sibling package outside the app package.
        write_test_file(
            &app,
            "packages/pdf-viewer/package.json",
            r#"{"name":"pdf-viewer","main":"dist/main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "packages/pdf-viewer/index.tsx",
            "export function PdfViewer() { return null; }\n",
        );
        let pkg = read_package_json_value(&app);
        let mut report = test_report();
        report.main_entry = Some("app/src/browser/main.js".to_string());
        let plan = detect_renderer(&app, &pkg, &report);
        assert_eq!(
            plan.source_dir.as_deref(),
            Some("app"),
            "the whole app package — including its internal plugin packages — is the renderer"
        );
        assert!(matches!(plan.kind, RendererKind::Source));
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn renderer_detection_excludes_main_entry_from_renderer() {
        // VS Code shape: src/main.ts IS the main process (it imports
        // Electron and the package main is ./out/main.js). The old
        // stem match reported all of src/ (16,131 files) as renderer
        // source. The fix reports no renderer rather than a wrong one.
        let app = fresh_test_dir("renderer-vscode");
        write_test_file(
            &app,
            "package.json",
            r#"{"name":"vscode","main":"./out/main.js","version":"1.0.0"}"#,
        );
        write_test_file(
            &app,
            "src/main.ts",
            "import { app } from 'electron';\nexport function main() {}\n",
        );
        write_test_file(
            &app,
            "src/vs/code/electron-main/main.ts",
            "import { app } from 'electron';\n",
        );
        write_test_file(&app, "src/vs/workbench/workbench.ts", "export {};\n");
        let pkg = read_package_json_value(&app);
        let mut report = test_report();
        report.main_entry = Some("./out/main.js".to_string());
        let plan = detect_renderer(&app, &pkg, &report);
        assert_eq!(
            plan.source_dir, None,
            "src/ must not be reported as renderer source"
        );
        assert!(matches!(plan.kind, RendererKind::Placeholder));
        fs::remove_dir_all(&app).ok();
    }

    #[test]
    fn bundler_detection_ignores_vite_substring_and_finds_rolldown() {
        // VS Code's scripts mention build/vite paths (component-explorer
        // install scripts) while the workbench builds with gulp/esbuild:
        // a substring hit reported "vite" for it.
        let vite_path = pkg_json(
            r#"{"name":"t","scripts":{"build":"node build/vite/install.mjs","postinstall":"node build/vite/download.mjs"}}"#,
        )
        .unwrap();
        assert_eq!(
            detect_bundler(&vite_path),
            None,
            "a build/vite *path* is not the vite bundler"
        );
        // Real vite still detects (dep or whole-word script token).
        let real_vite = pkg_json(
            r#"{"name":"t","devDependencies":{"vite":"^5.0.0"},"scripts":{"build":"vite build"}}"#,
        )
        .unwrap();
        assert_eq!(detect_bundler(&real_vite), Some(Bundler::Vite));
        // Signal's bundler: rolldown, by dep or by script token.
        let rolldown_dep =
            pkg_json(r#"{"name":"t","devDependencies":{"rolldown":"^1.0.0"}}"#).unwrap();
        assert_eq!(detect_bundler(&rolldown_dep), Some(Bundler::Rolldown));
        let rolldown_script =
            pkg_json(r#"{"name":"t","scripts":{"build":"rolldown -c rolldown.config.ts"}}"#)
                .unwrap();
        assert_eq!(detect_bundler(&rolldown_script), Some(Bundler::Rolldown));
    }
}
