//! Convert a scanned Electron app into a working Tauri 2 project.
//!
//! [`convert`] runs the scanner over an Electron app directory and writes a
//! fresh Tauri 2 project (Rust backend, TypeScript bridge, migration notes)
//! into the output directory. IPC handlers become compiling `TODO` stubs —
//! porting their bodies is the human's job (see `MIGRATION-NOTES.md`).
//!
//! The generated Rust is deliberately minimal and only uses stable Tauri 2
//! APIs, so `cargo check` passes on the generated `src-tauri` project.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{Classification, Finding, IpcChannel, MenuInfo, ScanReport};
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
    let plugins = detect_plugins(&report.findings, &report.native_modules);

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

    let mut files_written: Vec<PathBuf> = Vec::new();
    write_output_file(
        out_dir,
        &mut files_written,
        "src-tauri/Cargo.toml",
        &gen_cargo_toml(&pkg, &lib_snake, &plugins),
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
        &gen_tauri_conf(&pkg, &report)?,
    )?;
    // Tauri's `generate_context!()` reads `icons/icon.png` at compile time,
    // so the project does not build without it.
    write_output_file_bytes(
        out_dir,
        &mut files_written,
        "src-tauri/icons/icon.png",
        &gen_placeholder_icon_png(),
    )?;
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
        &gen_lib_rs(&handle_cmds, &plugins, report.menu.is_some()),
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        "src-tauri/src/commands.rs",
        &gen_commands_rs(&handle_cmds),
    )?;
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
        &gen_package_json(&pkg)?,
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        "README.md",
        &gen_readme(&report),
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        ".gitignore",
        "# Generated by electron-to-rust (e2r)\nnode_modules/\ndist/\nsrc-tauri/target/\n",
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        "dist/index.html",
        DIST_PLACEHOLDER,
    )?;
    write_output_file(
        out_dir,
        &mut files_written,
        "MIGRATION-NOTES.md",
        &gen_migration_notes(&report, &plugins, &handle_cmds),
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
        // `electron-store` / `electron-updater` are pure-JS deps the scanner
        // lists as native modules, but they DO have Tauri plugins — the
        // plugin is included (see detect_plugins), only the API calls need
        // porting.
        match m.as_str() {
            "electron-store" => warnings.push(
                "Native module 'electron-store' is covered by tauri-plugin-store (included); port its calls to the plugin API".to_string(),
            ),
            "electron-updater" => warnings.push(
                "Native module 'electron-updater' is covered by tauri-plugin-updater (included); port its calls to the plugin API".to_string(),
            ),
            _ => warnings.push(format!(
                "Native module '{m}' has no Tauri equivalent; find a Rust crate or rewrite it"
            )),
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
    warnings.push(
        "Frontend source files were not copied; copy your renderer source into src/ (see MIGRATION-NOTES.md)"
            .to_string(),
    );

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

/// Placeholder frontend so `cargo check` passes on the generated project
/// before the real renderer source is copied in: Tauri's build script
/// requires `build.frontendDist` to exist.
const DIST_PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>Placeholder — generated by electron-to-rust</title>
  </head>
  <body>
    <h1>Placeholder frontend</h1>
    <p>
      electron-to-rust does not copy renderer source files yet. Replace this
      file with your real frontend build output (or point
      <code>build.frontendDist</code> in
      <code>src-tauri/tauri.conf.json</code> at your existing
      <code>dist</code> directory). See <code>MIGRATION-NOTES.md</code>.
    </p>
  </body>
</html>
"#;

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
}

impl Plugins {
    /// Cargo dependency names (e.g. `tauri-plugin-dialog`), in a fixed order.
    fn cargo_deps(&self) -> Vec<&'static str> {
        let mut deps = Vec::new();
        if self.dialog {
            deps.push("tauri-plugin-dialog");
        }
        if self.notification {
            deps.push("tauri-plugin-notification");
        }
        if self.opener {
            deps.push("tauri-plugin-opener");
        }
        if self.store {
            deps.push("tauri-plugin-store");
        }
        if self.updater {
            deps.push("tauri-plugin-updater");
        }
        deps
    }

    /// `.plugin(...)` expressions for `lib.rs`, in the same order as
    /// [`Plugins::cargo_deps`].
    fn init_exprs(&self) -> Vec<&'static str> {
        let mut exprs = Vec::new();
        if self.dialog {
            exprs.push("tauri_plugin_dialog::init()");
        }
        if self.notification {
            exprs.push("tauri_plugin_notification::init()");
        }
        if self.opener {
            exprs.push("tauri_plugin_opener::init()");
        }
        if self.store {
            exprs.push("tauri_plugin_store::Builder::default().build()");
        }
        if self.updater {
            exprs.push("tauri_plugin_updater::Builder::new().build()");
        }
        exprs
    }
}

/// Decide which Tauri plugins to include from the scan findings and the
/// native-module list. Matching is deliberately conservative: a plugin is
/// included only when its Electron API (or category) appears in the
/// findings — except `electron-store` / `electron-updater`, which the
/// scanner reports as native modules (pure JS, but still needing a port),
/// so those are matched against `native_modules` too.
fn detect_plugins(findings: &[Finding], native_modules: &[String]) -> Plugins {
    let mut plugins = Plugins::default();
    for f in findings {
        let api = f.api.to_lowercase();
        let category = f.category.to_lowercase();
        let haystack = format!("{api} {category}");
        if haystack.contains("shell") {
            plugins.opener = true;
        }
        if haystack.contains("dialog") {
            plugins.dialog = true;
        }
        if haystack.contains("electron-store") || category == "store" {
            plugins.store = true;
        }
        if haystack.contains("autoupdater") || haystack.contains("auto-updater") {
            plugins.updater = true;
        }
        if haystack.contains("notification") {
            plugins.notification = true;
        }
    }
    for m in native_modules {
        match m.as_str() {
            "electron-store" => plugins.store = true,
            "electron-updater" => plugins.updater = true,
            _ => {}
        }
    }
    plugins
}

/// `src-tauri/Cargo.toml`: package, lib + bin targets, `tauri-build`, and
/// only the plugins the scan showed are used.
fn gen_cargo_toml(pkg: &str, lib_snake: &str, plugins: &Plugins) -> String {
    let mut out = format!(
        "[package]\nname = \"{pkg}\"\nversion = \"0.1.0\"\ndescription = \"Generated by electron-to-rust (e2r) from an Electron app\"\nedition = \"2021\"\n\n[lib]\nname = \"{lib_snake}\"\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"{pkg}\"\npath = \"src/main.rs\"\n\n# The generated project is its own workspace, so it builds standalone even\n# when the output lands inside another Cargo workspace.\n[workspace]\n\n[build-dependencies]\ntauri-build = {{ version = \"2\", features = [] }}\n\n[dependencies]\ntauri = {{ version = \"2\", features = [] }}\nserde = {{ version = \"1\", features = [\"derive\"] }}\nserde_json = \"1\"\n"
    );
    for dep in plugins.cargo_deps() {
        out.push_str(&format!("{dep} = \"2\"\n"));
    }
    out
}

/// `src-tauri/tauri.conf.json`: minimal but schema-valid Tauri 2 config.
/// Every window from the scan goes in `app.windows` (the first labelled
/// `main`, the rest `window-2`, `window-3`, ...); with no windows at all a
/// single default 800×600 window is emitted.
///
/// Note: Electron's `frame` option maps to Tauri's `decorations` — there is
/// no `frame` key in the Tauri 2 window config.
fn gen_tauri_conf(pkg: &str, report: &ScanReport) -> Result<String> {
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
            win.insert("width".to_string(), serde_json::json!(w.width));
            win.insert("height".to_string(), serde_json::json!(w.height));
            win.insert(
                "resizable".to_string(),
                serde_json::Value::Bool(w.resizable),
            );
            win.insert("decorations".to_string(), serde_json::Value::Bool(w.frame));
            windows.push(serde_json::Value::Object(win));
        }
    }
    let conf = serde_json::json!({
        "$schema": "https://schema.tauri.app/config/2",
        "productName": report.app_name,
        "version": "0.1.0",
        "identifier": format!("com.e2r.{pkg}"),
        "build": {
            "frontendDist": "../dist",
            "devUrl": "http://localhost:1420",
            "beforeDevCommand": "",
            "beforeBuildCommand": ""
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
/// (when the app has a menu) the menu wiring.
///
/// All windows already live in `tauri.conf.json`, so `setup` only wires the
/// menu — it does not create extra windows (that would duplicate labels).
fn gen_lib_rs(handle_cmds: &[(IpcChannel, String)], plugins: &Plugins, has_menu: bool) -> String {
    let mut out = String::from(
        "//! Tauri application entry point — generated by electron-to-rust (e2r).\n\n",
    );
    out.push_str("mod commands;\n");
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
    if !handle_cmds.is_empty() {
        let names: Vec<String> = handle_cmds
            .iter()
            .map(|(_, name)| format!("commands::{name}"))
            .collect();
        out.push_str("        .invoke_handler(tauri::generate_handler![");
        out.push_str(&names.join(", "));
        out.push_str("])\n");
    }
    if has_menu {
        out.push_str(
            "        .setup(|app| {\n            let menu = menu::build_menu(app.handle())?;\n            app.set_menu(menu)?;\n            Ok(())\n        })\n",
        );
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
fn gen_commands_rs(handle_cmds: &[(IpcChannel, String)]) -> String {
    let mut out = String::from(
        "//! Tauri commands generated by electron-to-rust (e2r) from Electron\n//! `ipcMain.handle` channels. Each command is a `TODO` stub: port the\n//! original Electron main-process handler into the function body.\n",
    );
    if handle_cmds.is_empty() {
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
/// - `"on"` channels become `emit` wrappers; the Rust side still needs an
///   event listener (flagged with a `TODO`).
/// - `"send"` channels that match a `"handle"` channel are the renderer's
///   `ipcRenderer.invoke` side of the same channel, already covered by the
///   `invoke` wrapper, so they are skipped; other `"send"` channels become
///   `emit` wrappers.
fn gen_bridge_ts(report: &ScanReport, handle_cmds: &[(IpcChannel, String)]) -> String {
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
        "\nimport { invoke } from \"@tauri-apps/api/core\";\nimport { emit, listen } from \"@tauri-apps/api/event\";\n\n/** Low-level Tauri primitives, re-exported for hand-written wrappers. */\nexport const e2rApi = { invoke, listen, emit };\n",
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
            "  /** Send-only channel `{}` (original: {}:{}). TODO(e2r): the Rust side needs an event listener for this channel. */\n  {}: (payload?: unknown) => emit(\"{}\", payload),",
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
            "  /** Renderer-side channel `{}` (original: {}:{}). TODO(e2r): check the original call site — if this is a main-to-renderer event, use `listen` instead of `emit`. */\n  {}: (payload?: unknown) => emit(\"{}\", payload),",
            comment_safe(&ch.name),
            ch.file,
            ch.line,
            key,
            rust_str_escape(&ch.name),
        ));
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

    // Preload keys other than `api` (covered by the `api` object above):
    // best-effort generic proxies, since e2r cannot know their shape.
    for key in &report.preload.exposed_keys {
        if key == "api" {
            continue;
        }
        let ident = to_camel_case(key);
        if ident == "api" || ident == "e2rApi" {
            continue;
        }
        out.push_str(&format!(
            "\n// Preload key `{}`: e2r could not determine its shape, so this is a\n// generic proxy over `invoke`. TODO(e2r): replace it with typed wrappers\n// matching the original preload.\nexport const {} = new Proxy({{}}, {{\n  get: (_target, prop) => (payload?: unknown) => invoke(String(prop), {{ payload }}),\n}});\n",
            comment_safe(key),
            ident,
        ));
    }
    out
}

/// `package.json` for the generated project.
fn gen_package_json(pkg: &str) -> Result<String> {
    let doc = serde_json::json!({
        "name": pkg,
        "version": "0.1.0",
        "private": true,
        "type": "module",
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
    Ok(serde_json::to_string_pretty(&doc)?)
}

/// `README.md` for the generated project.
fn gen_readme(report: &ScanReport) -> String {
    format!(
        "# {} (Tauri 2)\n\nGenerated by [electron-to-rust](https://github.com/mr-vishwet/rust-made-easy) (e2r) from an Electron app.\n\n## Prerequisites (Linux)\n\nTauri needs GTK and WebKit system libraries. On Debian/Ubuntu:\n\n```sh\nsudo apt install libwebkit2gtk-4.1-dev build-essential libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev\n```\n\nSee <https://v2.tauri.app/start/prerequisites/> for other platforms.\n\n## Run it\n\n```sh\nnpm install\nnpm run tauri dev\n```\n\n## What still needs doing\n\n- Port the `TODO(e2r)` commands in `src-tauri/src/commands.rs` — each one\n  is a stub for an `ipcMain.handle` channel from the original app.\n- Copy your renderer source into `src/` (or point `build.frontendDist` in\n  `src-tauri/tauri.conf.json` at your existing build output) and delete the\n  placeholder `dist/index.html`.\n- See `MIGRATION-NOTES.md` for the full list.\n\n## Layout\n\n- `src-tauri/` — Rust backend (Tauri 2)\n- `src/lib/e2r-bridge.ts` — TypeScript bridge replacing the Electron preload\n",
        report.app_name
    )
}

/// `MIGRATION-NOTES.md`: what was converted, what needs hand work, and the
/// next steps.
fn gen_migration_notes(
    report: &ScanReport,
    plugins: &Plugins,
    handle_cmds: &[(IpcChannel, String)],
) -> String {
    let mut out = format!(
        "# Migration notes — {}\n\nGenerated by electron-to-rust (e2r) from an Electron app.\n\n",
        report.app_name
    );
    if let Some(version) = &report.electron_version {
        out.push_str(&format!("Electron version: {version}\n\n"));
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
            out.push_str(&format!(
                "  - `{label}`: {}×{}{title}{flags} ({}:{})\n",
                w.width, w.height, w.file, w.line
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
    // Deduplicated by name: the same channel often appears on both sides
    // (e.g. `ipcMain.on` in main.js and `ipcRenderer.on` in preload.js).
    let mut seen_events: BTreeSet<&str> = BTreeSet::new();
    let event_channels: Vec<&IpcChannel> = report
        .ipc_channels
        .iter()
        .filter(|c| {
            c.direction == "on"
                || (c.direction == "send" && !handle_cmds.iter().any(|(h, _)| h.name == c.name))
        })
        .filter(|c| seen_events.insert(c.name.as_str()))
        .collect();
    if !event_channels.is_empty() {
        out.push_str("- **Event channels** (via `emit` in `src/lib/e2r-bridge.ts`; the Rust side still needs listeners):\n");
        for ch in event_channels {
            out.push_str(&format!("  - `{}` ({}:{})\n", ch.name, ch.file, ch.line));
        }
    }

    let deps = plugins.cargo_deps();
    if deps.is_empty() {
        out.push_str("- **Plugins:** none needed.\n");
    } else {
        out.push_str(&format!("- **Plugins:** {}\n", deps.join(", ")));
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
    if manual.is_empty() && assisted.is_empty() && report.native_modules.is_empty() {
        out.push_str("Nothing flagged beyond the `TODO(e2r)` command stubs above.\n");
    }
    if !manual.is_empty() {
        out.push_str("### Manual ports (no direct Tauri equivalent)\n\n");
        for f in &manual {
            out.push_str(&format!("- `{}` ({}:{})", f.api, f.file, f.line));
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
            match m.as_str() {
                "electron-store" => out.push_str(
                    "- `electron-store`: covered by `tauri-plugin-store` (included) — port its calls to the plugin API.\n",
                ),
                "electron-updater" => out.push_str(
                    "- `electron-updater`: covered by `tauri-plugin-updater` (included) — port its calls to the plugin API.\n",
                ),
                _ => out.push_str(&format!(
                    "- `{m}`: no Tauri equivalent — find a Rust crate or rewrite it.\n"
                )),
            }
        }
        out.push('\n');
    }
    out.push_str("### Frontend\n\nThe original renderer source was NOT copied. Copy it into `src/` (or point `build.frontendDist` in `src-tauri/tauri.conf.json` at your existing build output) and delete the placeholder `dist/index.html`.\n\n### Icon\n\n`src-tauri/icons/icon.png` is a placeholder — replace it with the app's real icon (and generate the platform sizes with `cargo tauri icon`).\n\n");
    out.push_str("## Next steps\n\n1. `npm install`\n2. Copy your renderer source in (see above).\n3. Port each `TODO(e2r)` command in `src-tauri/src/commands.rs`.\n4. `npm run tauri dev`\n");
    out
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
/// Prefixed with `_` when it would start with a digit.
pub fn to_camel_case(name: &str) -> String {
    let mut out = String::new();
    let mut capitalize_next = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if capitalize_next {
                out.push(c.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                out.push(c.to_ascii_lowercase());
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
        assert_eq!(to_camel_case("9lives"), "_9lives");
        assert_eq!(to_camel_case(""), "channel");
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
        let plugins = detect_plugins(&findings, &[]);
        assert!(plugins.dialog);
        assert!(plugins.store);
        assert!(!plugins.opener);
        assert!(!plugins.updater);
        assert!(!plugins.notification);
    }

    #[test]
    fn plugin_detection_from_native_modules() {
        // The scanner reports electron-store / electron-updater as native
        // modules, not findings — the plugins must still be included.
        let plugins = detect_plugins(
            &[finding("BrowserWindow")],
            &["electron-store".to_string(), "electron-updater".to_string()],
        );
        assert!(plugins.store);
        assert!(plugins.updater);
        assert!(!plugins.dialog);
    }

    #[test]
    fn plugin_detection_is_empty_without_matches() {
        let plugins = detect_plugins(
            &[finding("BrowserWindow"), finding("ipcMain.handle")],
            &["serialport".to_string()],
        );
        assert_eq!(plugins, Plugins::default());
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
}
