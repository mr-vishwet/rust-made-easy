//! Golden tests: scan + convert the fixture apps and verify the
//! generated Tauri 2 projects.
//!
//! `minimal-electron-app` is a plain static app (root `index.html`, no
//! build step); `built-output-electron-app` is a Vite app with a real
//! renderer build output in `dist/` plus its source;
//! `full-apis-electron-app` is a tray utility using `autoUpdater`,
//! `shell.openExternal`, and `Notification`, plus a preload that exposes
//! a second key (`versions`); it also uses `nativeTheme`,
//! `globalShortcut`, and `clipboard`, so the generated `set_theme`
//! command and those two plugin inits are proven to compile too.
//!
//! The `cargo check` step in `test_convert_fixture` and
//! `test_convert_full_apis_fixture` needs Tauri's Linux system
//! prerequisites (GTK + WebKit dev libraries); see
//! https://v2.tauri.app/start/prerequisites/ — without them the check fails
//! in `gdk-sys`/`webkit2gtk-sys` build scripts, not in generated code.
//! On this machine those libraries live in a repo-local sysroot
//! (`target/tauri-sysroot/`); `cargo_check` points pkg-config at it when
//! it exists, so the tests work without manual environment setup.
//! Only those two tests run `cargo check`: the built-output fixture
//! differs only in `tauri.conf.json` / `package.json`, which `cargo
//! check` does not validate beyond `frontendDist` existing — asserted
//! directly in `test_convert_built_output_fixture`. The full-APIs
//! fixture runs its own check because its generated `lib.rs` is a
//! different shape: the updater / opener / notification plugin init
//! expressions are generated only for apps that use those APIs.

use e2r_lib::convert::convert;
use e2r_lib::scan::scan;
use e2r_lib::simulate::{
    format_report, simulate_with_options, CalibrationInput, ReportFormat, SimulateOptions,
    TargetPlatform,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The fixture app, relative to this crate's manifest dir.
fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/minimal-electron-app")
}

/// The built-output fixture app, relative to this crate's manifest dir.
fn built_output_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/built-output-electron-app")
}

/// The full-APIs fixture app, relative to this crate's manifest dir.
fn full_apis_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/full-apis-electron-app")
}

/// A fresh output dir per test run (no `tempfile` dependency: process id +
/// timestamp is unique enough for a test).
///
/// NOTE: intentionally NOT `std::env::temp_dir()`. On this machine `/tmp`
/// is a 512 MB tmpfs and a Tauri `cargo check` target dir is gigabytes —
/// the check dies with "No space left on device" there (golden run 2).
/// The workspace `target/` dir (repo root, four levels up from this crate)
/// is real disk, and `cargo clean` removes leftovers. The dir is removed on
/// success anyway; on failure it is left in place for debugging (its path
/// is printed by the test).
fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../target")
        .join(format!("e2r-golden-{tag}-{}-{nanos}", std::process::id()))
}

#[test]
fn test_scan_fixture() {
    let report = scan(&fixture_dir()).expect("scan should succeed on the fixture");

    assert_eq!(report.app_name, "minimal-electron-app");

    let handle_channels: Vec<&str> = report
        .ipc_channels
        .iter()
        .filter(|c| c.direction == "handle")
        .map(|c| c.name.as_str())
        .collect();
    for expected in ["read-file", "save-settings", "get-version"] {
        assert!(
            handle_channels.contains(&expected),
            "missing handle channel {expected:?}; found {handle_channels:?}"
        );
    }

    assert_eq!(
        report.windows.len(),
        2,
        "fixture creates two BrowserWindows"
    );
    let main = report
        .windows
        .iter()
        .find(|w| w.title.as_deref() == Some("Minimal App"))
        .expect("main window (titled 'Minimal App') should be found");
    assert_eq!(main.width, 1024);
    assert_eq!(main.height, 768);
    assert!(main.frame);
    let settings = report
        .windows
        .iter()
        .find(|w| w.title.as_deref() == Some("Settings"))
        .expect("settings window (titled 'Settings') should be found");
    assert_eq!(settings.width, 600);
    assert_eq!(settings.height, 400);
    assert!(!settings.frame, "settings window is frameless");

    let apis: Vec<String> = report
        .findings
        .iter()
        .map(|f| f.api.to_lowercase())
        .collect();
    for expected in ["tray", "dialog", "menu"] {
        assert!(
            apis.iter().any(|a| a.contains(expected)),
            "no finding mentions {expected:?}; findings: {apis:?}"
        );
    }

    assert!(
        report.preload.uses_context_bridge,
        "fixture preload uses contextBridge"
    );
    assert!(
        report.preload.exposed_keys.iter().any(|k| k == "api"),
        "preload should expose key 'api'; exposed: {:?}",
        report.preload.exposed_keys
    );
}

#[test]
fn test_convert_fixture() {
    let out_dir = unique_temp_dir("convert");
    println!("golden output dir: {}", out_dir.display());

    let report = convert(&fixture_dir(), &out_dir).expect("convert should succeed on the fixture");
    assert!(
        !report.files_written.is_empty(),
        "convert should report the files it wrote"
    );

    for rel in [
        "src-tauri/Cargo.toml",
        "src-tauri/tauri.conf.json",
        "src-tauri/src/lib.rs",
        "src-tauri/src/commands.rs",
        "src-tauri/src/main.rs",
        "src-tauri/icons/icon.png",
        "src/lib/e2r-bridge.ts",
        "package.json",
        "MIGRATION-NOTES.md",
    ] {
        assert!(out_dir.join(rel).is_file(), "missing output file {rel}");
    }

    let commands =
        fs::read_to_string(out_dir.join("src-tauri/src/commands.rs")).expect("read commands.rs");
    for expected in ["read_file", "save_settings", "get_version"] {
        assert!(
            commands.contains(expected),
            "commands.rs should define {expected}"
        );
    }

    let conf_text = fs::read_to_string(out_dir.join("src-tauri/tauri.conf.json"))
        .expect("read tauri.conf.json");
    let conf: serde_json::Value =
        serde_json::from_str(&conf_text).expect("tauri.conf.json should parse as JSON");
    let windows = conf["app"]["windows"]
        .as_array()
        .expect("app.windows should be an array");
    assert_eq!(
        windows.len(),
        2,
        "tauri.conf.json should carry both windows"
    );
    assert_eq!(windows[0]["label"], "main");
    assert_eq!(windows[1]["label"], "window-2");

    // The minimal fixture is a static app (root index.html, no build
    // step): its real renderer must be carried into dist/, not replaced
    // with the placeholder — and with no dev server, devUrl is null so
    // `tauri dev` serves frontendDist.
    assert_eq!(conf["build"]["frontendDist"], "../dist");
    assert!(
        conf["build"]["devUrl"].is_null(),
        "no dev server in the fixture, so devUrl must be null; got {}",
        conf["build"]["devUrl"]
    );
    let dist_index =
        fs::read_to_string(out_dir.join("dist/index.html")).expect("read dist/index.html");
    assert!(
        dist_index.contains("Minimal App"),
        "dist/index.html should be the fixture's real renderer, got: {dist_index}"
    );
    assert!(
        !dist_index.contains("Placeholder frontend"),
        "dist/index.html must not be the placeholder"
    );
    // The fixture's main process has one `ipcMain.on` channel, so the
    // generated project must have a Rust listener stub for it — and a
    // capability file granting the dialog/store plugin permissions.
    let listeners =
        fs::read_to_string(out_dir.join("src-tauri/src/listeners.rs")).expect("read listeners.rs");
    assert!(
        listeners.contains("app.listen(\"log-message\""),
        "listeners.rs should register the log-message channel; got:\n{listeners}"
    );
    let lib_rs = fs::read_to_string(out_dir.join("src-tauri/src/lib.rs")).expect("read lib.rs");
    assert!(
        lib_rs.contains("listeners::register_listeners(app);"),
        "lib.rs should register the listeners; got:\n{lib_rs}"
    );
    let cap = fs::read_to_string(out_dir.join("src-tauri/capabilities/default.json"))
        .expect("read capabilities/default.json");
    let cap_value: serde_json::Value =
        serde_json::from_str(&cap).expect("capabilities should parse as JSON");
    let permissions: Vec<&str> = cap_value["permissions"]
        .as_array()
        .expect("permissions should be an array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(permissions.contains(&"core:default"));
    assert!(permissions.contains(&"dialog:default"));
    assert!(permissions.contains(&"store:default"));
    assert!(
        !permissions.contains(&"updater:default"),
        "the minimal fixture uses no updater, so its capability must not grant it"
    );
    let notes =
        fs::read_to_string(out_dir.join("MIGRATION-NOTES.md")).expect("read MIGRATION-NOTES.md");
    assert!(
        notes.contains("Copied the static renderer"),
        "notes should record the static carry-over; got:\n{notes}"
    );
    assert!(
        !notes.contains("was NOT copied"),
        "the 'renderer NOT copied' caveat must be gone; got:\n{notes}"
    );

    // The generated project must compile. First run downloads Tauri + plugins
    // from crates.io, so this takes minutes — that is expected.
    cargo_check(&out_dir.join("src-tauri"), &out_dir);

    // Clean up on success; on failure the directory is left in place (its
    // path was printed above) for debugging.
    fs::remove_dir_all(&out_dir).ok();
}

#[test]
fn test_convert_built_output_fixture() {
    let out_dir = unique_temp_dir("built-output");
    println!("golden output dir: {}", out_dir.display());

    let report = convert(&built_output_fixture_dir(), &out_dir)
        .expect("convert should succeed on the fixture");
    assert!(
        !report.files_written.is_empty(),
        "convert should report the files it wrote"
    );

    // The built output is copied verbatim into dist/ — the BUILT
    // index.html (with its marker text), not the source root index.html.
    let dist_index =
        fs::read_to_string(out_dir.join("dist/index.html")).expect("read dist/index.html");
    assert!(
        dist_index.contains("This is the app's real built renderer output."),
        "dist/index.html should be the built output, got: {dist_index}"
    );
    assert!(
        !dist_index.contains("/src/main.js"),
        "dist/index.html must be the built version, not the source index.html"
    );
    let dist_asset =
        fs::read_to_string(out_dir.join("dist/assets/app.js")).expect("read dist/assets/app.js");
    assert!(dist_asset.contains("built output app"));

    // The renderer source comes along too, so the frontend can be rebuilt.
    let src_main = fs::read_to_string(out_dir.join("src/main.js")).expect("read src/main.js");
    assert!(src_main.contains("Renderer source entry"));
    assert!(
        out_dir.join("src/style.css").is_file(),
        "src/style.css copied"
    );
    let root_index = fs::read_to_string(out_dir.join("index.html")).expect("read root index.html");
    assert!(
        root_index.contains("/src/main.js"),
        "root index.html (Vite layout) should be copied, got: {root_index}"
    );
    assert!(
        out_dir.join("vite.config.js").is_file(),
        "vite.config.js copied"
    );

    // The bridge still lands in src/lib/ alongside the copied source.
    assert!(
        out_dir.join("src/lib/e2r-bridge.ts").is_file(),
        "e2r-bridge.ts should be generated next to the copied source"
    );

    let conf_text = fs::read_to_string(out_dir.join("src-tauri/tauri.conf.json"))
        .expect("read tauri.conf.json");
    let conf: serde_json::Value =
        serde_json::from_str(&conf_text).expect("tauri.conf.json should parse as JSON");
    assert_eq!(conf["build"]["frontendDist"], "../dist");
    // The port comes from vite.config.js (5199), not the Vite default.
    assert_eq!(conf["build"]["devUrl"], "http://localhost:5199");
    assert_eq!(conf["build"]["beforeDevCommand"], "npm run dev:frontend");
    assert_eq!(
        conf["build"]["beforeBuildCommand"],
        "npm run build:frontend"
    );

    let pkg_text = fs::read_to_string(out_dir.join("package.json")).expect("read package.json");
    let pkg: serde_json::Value =
        serde_json::from_str(&pkg_text).expect("package.json should parse as JSON");
    assert_eq!(pkg["scripts"]["build"], "tauri build");
    assert_eq!(pkg["scripts"]["build:frontend"], "vite build");
    assert_eq!(pkg["scripts"]["dev:frontend"], "vite");
    assert!(
        pkg["devDependencies"]["vite"].is_string(),
        "vite must be carried over so the preserved scripts can run"
    );
    assert!(
        pkg["devDependencies"]["electron"].is_null() && pkg["dependencies"]["electron"].is_null(),
        "electron must not be carried over into the Tauri project"
    );

    let notes =
        fs::read_to_string(out_dir.join("MIGRATION-NOTES.md")).expect("read MIGRATION-NOTES.md");
    assert!(
        notes.contains("Copied the built frontend"),
        "notes should record the built-output carry-over; got:\n{notes}"
    );
    assert!(
        !notes.contains("was NOT copied"),
        "the 'renderer NOT copied' caveat must be gone; got:\n{notes}"
    );

    // No cargo check here (see the module docs): the generated Rust is
    // the same shape as the minimal fixture's, which test_convert_fixture
    // already checks.
    fs::remove_dir_all(&out_dir).ok();
}

#[test]
fn test_scan_full_apis_fixture() {
    let report = scan(&full_apis_fixture_dir()).expect("scan should succeed on the fixture");

    assert_eq!(report.app_name, "full-apis-electron-app");

    // The three plugin APIs the fixture exists to exercise. The
    // updater here is `electron-updater` (the library), not Electron's
    // built-in `autoUpdater`: the fixture binds the `autoUpdater` local
    // from `require('electron-updater')`, so the finding must say
    // `electron-updater` and there must be no bare `autoUpdater` one.
    let apis: Vec<String> = report
        .findings
        .iter()
        .map(|f| f.api.to_lowercase())
        .collect();
    for expected in [
        "electron-updater",
        "shell.openexternal",
        "notification",
        "nativetheme",
        "globalshortcut",
        "clipboard",
    ] {
        assert!(
            apis.iter().any(|a| a.contains(expected)),
            "no finding mentions {expected:?}; findings: {apis:?}"
        );
    }
    assert!(
        !apis.iter().any(|a| a == "autoupdater"),
        "aliased electron-updater must not be reported as Electron's autoUpdater; findings: {apis:?}"
    );

    // electron-updater / electron-store are pure JS, so the scanner lists
    // them as port-needing dependencies — NOT as native modules (a dep
    // with a 1:1 plugin equivalent must not cost 15 feasibility points).
    assert!(
        report.native_modules.is_empty(),
        "no true native module in this fixture; found {:?}",
        report.native_modules
    );
    for expected in ["electron-updater", "electron-store"] {
        assert!(
            report.port_needing_deps.iter().any(|m| m == expected),
            "port-needing dep {expected:?} missing; found {:?}",
            report.port_needing_deps
        );
    }

    let handle_channels: Vec<&str> = report
        .ipc_channels
        .iter()
        .filter(|c| c.direction == "handle")
        .map(|c| c.name.as_str())
        .collect();
    for expected in ["check-for-updates", "open-external", "show-notification"] {
        assert!(
            handle_channels.contains(&expected),
            "missing handle channel {expected:?}; found {handle_channels:?}"
        );
    }

    assert!(
        report.preload.uses_context_bridge,
        "fixture preload uses contextBridge"
    );
    for expected in ["api", "versions"] {
        assert!(
            report.preload.exposed_keys.iter().any(|k| k == expected),
            "preload should expose key {expected:?}; exposed: {:?}",
            report.preload.exposed_keys
        );
    }
}

#[test]
fn test_convert_full_apis_fixture() {
    let out_dir = unique_temp_dir("full-apis");
    println!("golden output dir: {}", out_dir.display());

    let report =
        convert(&full_apis_fixture_dir(), &out_dir).expect("convert should succeed on the fixture");
    assert!(
        !report.files_written.is_empty(),
        "convert should report the files it wrote"
    );

    for rel in [
        "src-tauri/Cargo.toml",
        "src-tauri/tauri.conf.json",
        "src-tauri/src/lib.rs",
        "src-tauri/src/commands.rs",
        "src-tauri/src/main.rs",
        "src-tauri/icons/icon.png",
        "src/lib/e2r-bridge.ts",
        "package.json",
        "MIGRATION-NOTES.md",
    ] {
        assert!(out_dir.join(rel).is_file(), "missing output file {rel}");
    }

    // The generated Cargo.toml must pull in the updater, opener, and
    // notification plugins — plus dialog (showMessageBox) and store
    // (electron-store), and the global-shortcut and clipboard-manager
    // plugins — so this fixture's lib.rs exercises all seven plugin init
    // expressions at once.
    let cargo_toml =
        fs::read_to_string(out_dir.join("src-tauri/Cargo.toml")).expect("read Cargo.toml");
    for dep in [
        "tauri-plugin-updater",
        "tauri-plugin-opener",
        "tauri-plugin-notification",
        "tauri-plugin-dialog",
        "tauri-plugin-store",
        "tauri-plugin-global-shortcut",
        "tauri-plugin-clipboard-manager",
    ] {
        assert!(
            cargo_toml.contains(dep),
            "Cargo.toml should depend on {dep}; got:\n{cargo_toml}"
        );
    }

    // The generated lib.rs must wire the plugin init expressions.
    let lib_rs = fs::read_to_string(out_dir.join("src-tauri/src/lib.rs")).expect("read lib.rs");
    for expr in [
        "tauri_plugin_updater::Builder::new().build()",
        "tauri_plugin_opener::init()",
        "tauri_plugin_notification::init()",
        "tauri_plugin_dialog::init()",
        "tauri_plugin_store::Builder::default().build()",
        "tauri_plugin_global_shortcut::Builder::new().build()",
        "tauri_plugin_clipboard_manager::init()",
    ] {
        assert!(
            lib_rs.contains(expr),
            "lib.rs should contain the plugin init `{expr}`; got:\n{lib_rs}"
        );
    }

    // nativeTheme produces a real `set_theme` command, registered in the
    // invoke handler and wrapped in the bridge.
    let commands =
        fs::read_to_string(out_dir.join("src-tauri/src/commands.rs")).expect("read commands.rs");
    assert!(
        commands.contains("pub fn set_theme(window: tauri::Window, theme: String)"),
        "commands.rs should define set_theme; got:\n{commands}"
    );
    assert!(
        lib_rs.contains("commands::set_theme"),
        "lib.rs should register set_theme; got:\n{lib_rs}"
    );

    // The capability file must grant core:default plus every included
    // plugin's :default permission set — and nothing else.
    let cap = fs::read_to_string(out_dir.join("src-tauri/capabilities/default.json"))
        .expect("read capabilities/default.json");
    let cap_value: serde_json::Value =
        serde_json::from_str(&cap).expect("capabilities should parse as JSON");
    let permissions: Vec<&str> = cap_value["permissions"]
        .as_array()
        .expect("permissions should be an array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for expected in [
        "core:default",
        "updater:default",
        "opener:default",
        "notification:default",
        "dialog:default",
        "store:default",
        "global-shortcut:default",
        "clipboard-manager:default",
    ] {
        assert!(
            permissions.contains(&expected),
            "capability should grant {expected}; granted: {permissions:?}"
        );
    }
    assert!(
        !permissions.contains(&"single-instance:default"),
        "no single-instance use in this fixture, so its permission must be absent"
    );

    // The config carries the fixture's single window; the plugins live in
    // Cargo.toml + lib.rs, and their runtime permissions live in the
    // capability file above.
    let conf_text = fs::read_to_string(out_dir.join("src-tauri/tauri.conf.json"))
        .expect("read tauri.conf.json");
    let conf: serde_json::Value =
        serde_json::from_str(&conf_text).expect("tauri.conf.json should parse as JSON");
    let windows = conf["app"]["windows"]
        .as_array()
        .expect("app.windows should be an array");
    assert_eq!(windows.len(), 1, "fixture creates one BrowserWindow");
    assert_eq!(windows[0]["label"], "main");
    assert_eq!(windows[0]["title"], "Full APIs App");

    // The preload's second key (`versions`) becomes a TYPED object in the
    // bridge — derived from the recovered preload shape — never a generic
    // Proxy (a Proxy would invoke commands that do not exist).
    let bridge =
        fs::read_to_string(out_dir.join("src/lib/e2r-bridge.ts")).expect("read e2r-bridge.ts");
    assert!(
        !bridge.contains("new Proxy"),
        "bridge must never emit a generic Proxy; got:\n{bridge}"
    );
    assert!(
        bridge.contains("export const versions = {"),
        "bridge should export a typed `versions` object; got:\n{bridge}"
    );
    for expected in ["node:", "chrome:", "electron:"] {
        assert!(
            bridge.contains(expected),
            "typed `versions` object should carry member {expected}; got:\n{bridge}"
        );
    }
    for expected in ["checkForUpdates", "openExternal", "showNotification"] {
        assert!(
            bridge.contains(expected),
            "bridge should wrap channel {expected}; got:\n{bridge}"
        );
    }
    assert!(
        bridge.contains("setTheme:"),
        "bridge should wrap the set_theme command; got:\n{bridge}"
    );

    // The generated project must compile — this is the proof the
    // updater / opener / notification init expressions are real Tauri 2
    // API, not just plausible-looking text. First run downloads
    // tauri-plugin-notification from crates.io, so this takes minutes.
    cargo_check(&out_dir.join("src-tauri"), &out_dir);

    // Clean up on success; on failure the directory is left in place (its
    // path was printed above) for debugging.
    fs::remove_dir_all(&out_dir).ok();
}

/// Run `cargo check` on the generated project.
///
/// Panics with a clear message if cargo is missing. If the check itself
/// fails — including for lack of network to fetch crates — the test FAILS
/// with the captured stdout/stderr shown; it never silently passes.
fn cargo_check(src_tauri: &Path, log_dir: &Path) {
    match Command::new("cargo").arg("--version").output() {
        Ok(out) if out.status.success() => {}
        _ => panic!("cargo not installed, cannot verify generated project compiles"),
    }

    let stdout_log = fs::File::create(log_dir.join("cargo-check.stdout.log"))
        .expect("create cargo check stdout log");
    let stderr_log = fs::File::create(log_dir.join("cargo-check.stderr.log"))
        .expect("create cargo check stderr log");
    // Logs go to files (not pipes) so a chatty `cargo check` cannot deadlock
    // the polling loop below on a full pipe buffer.
    let mut cmd = Command::new("cargo");
    cmd.args(["check", "--manifest-path"])
        .arg(src_tauri.join("Cargo.toml"))
        .env("CARGO_TERM_COLOR", "never");
    // On this machine Tauri's Linux system libraries are not installed;
    // they live in a repo-local sysroot (repo root `target/tauri-sysroot/`,
    // four levels up from this crate). Point pkg-config at it so the check
    // finds gdk-3.0 etc. An explicitly set PKG_CONFIG_SYSROOT_DIR wins.
    if std::env::var_os("PKG_CONFIG_SYSROOT_DIR").is_none() {
        let sysroot =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../target/tauri-sysroot/root");
        if sysroot.is_dir() {
            cmd.env("PKG_CONFIG_SYSROOT_DIR", &sysroot);
            cmd.env(
                "PKG_CONFIG_PATH",
                format!(
                    "{}/usr/lib/x86_64-linux-gnu/pkgconfig:{}/usr/share/pkgconfig",
                    sysroot.display(),
                    sysroot.display()
                ),
            );
        }
    }
    let mut child: Child = cmd
        .stdout(stdout_log)
        .stderr(stderr_log)
        .spawn()
        .expect("failed to spawn cargo check");

    let deadline = Instant::now() + Duration::from_secs(20 * 60);
    loop {
        match child.try_wait().expect("failed to wait on cargo check") {
            Some(status) => {
                if !status.success() {
                    let stdout = fs::read_to_string(log_dir.join("cargo-check.stdout.log"))
                        .unwrap_or_default();
                    let stderr = fs::read_to_string(log_dir.join("cargo-check.stderr.log"))
                        .unwrap_or_default();
                    panic!(
                        "cargo check failed on the generated project (exit {status}):\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
                    );
                }
                return;
            }
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("cargo check on the generated project timed out after 20 minutes");
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Simulate golden tests (`e2r simulate`)
// ---------------------------------------------------------------------------
//
// Simulate generates no code, so these tests never run `cargo check` —
// they are fast. They assert on the report the library builds directly
// (not by shelling out to the binary), with calibration disabled so
// results never depend on the developer's `~/.cache` log.

/// Simulate with calibration disabled (hermetic) for the host platform.
fn simulate_hermetic(app_dir: &Path) -> e2r_lib::model::SimulationReport {
    simulate_with_options(
        app_dir,
        &SimulateOptions {
            target_platform: TargetPlatform::host(),
            calibration: CalibrationInput::Disabled,
        },
    )
    .expect("simulate should succeed on the fixture")
}

fn all_simulate_fixtures() -> Vec<PathBuf> {
    vec![
        fixture_dir(),
        full_apis_fixture_dir(),
        built_output_fixture_dir(),
    ]
}

#[test]
fn test_simulate_all_fixtures_structure() {
    // This VM is Linux, so the per-platform metric ids are the Linux ones.
    assert_eq!(TargetPlatform::host(), TargetPlatform::Linux);
    let expected_metrics = [
        "bundle_size",
        "binary_size",
        "memory_linux",
        "startup_time",
        "process_count_linux",
        "cpu",
    ];
    for fixture in all_simulate_fixtures() {
        let report = simulate_hermetic(&fixture);

        // Identity + model version.
        assert!(!report.app_name.is_empty());
        assert_eq!(report.model_version, "0.1.0");
        assert!(report.feasibility_score <= 100);

        // Breakdown: counts add up, percentages sum to exactly 100.
        assert_eq!(
            report.breakdown.total,
            report.breakdown.auto + report.breakdown.assisted + report.breakdown.manual,
            "breakdown total must be the sum of the classes for {}",
            fixture.display()
        );
        let pct_sum =
            report.breakdown.auto_pct + report.breakdown.assisted_pct + report.breakdown.manual_pct;
        assert!(
            (pct_sum - 100.0).abs() < 1e-6,
            "breakdown percentages must sum to 100, got {pct_sum} for {}",
            fixture.display()
        );

        // UI preservation: an estimate in range, four factors.
        assert!(
            (0.0..=100.0).contains(&report.ui_preservation.estimated_pct),
            "UI estimate must be a percentage for {}",
            fixture.display()
        );
        assert_eq!(report.ui_preservation.factors.len(), 4);

        // Predictions: all six, in fixed order, every range ordered.
        let metrics: Vec<&str> = report
            .predictions
            .iter()
            .map(|p| p.metric.as_str())
            .collect();
        assert_eq!(
            metrics,
            expected_metrics,
            "prediction metric ids must match for {}",
            fixture.display()
        );
        for prediction in &report.predictions {
            for (side, range) in [("before", &prediction.before), ("after", &prediction.after)] {
                if let Some(range) = range {
                    assert!(
                        range.is_valid(),
                        "{} {} range must satisfy low < point < high, got {:?} for {}",
                        prediction.metric,
                        side,
                        range,
                        fixture.display()
                    );
                }
            }
            // Every prediction with numbers carries a basis.
            if prediction.after.is_some() {
                assert!(
                    !prediction.basis.is_empty(),
                    "{} must carry a basis for {}",
                    prediction.metric,
                    fixture.display()
                );
            }
        }
        // CPU is unknown with no numbers; Linux memory is unknown too.
        let cpu = report
            .predictions
            .iter()
            .find(|p| p.metric == "cpu")
            .expect("cpu prediction");
        assert_eq!(cpu.before, None);
        assert_eq!(cpu.after, None);
        let memory = report
            .predictions
            .iter()
            .find(|p| p.metric == "memory_linux")
            .expect("memory prediction");
        assert_eq!(memory.after, None, "Linux memory must carry no numbers");

        // Startup never predicts an improvement.
        let startup = report
            .predictions
            .iter()
            .find(|p| p.metric == "startup_time")
            .expect("startup prediction");
        assert_eq!(startup.after.expect("startup range").point, 1.0);

        // Bundle: the before range is the measured installer range
        // (no fixture ships an installed runtime) and the after range
        // is the measured e2r output model.
        let bundle = report
            .predictions
            .iter()
            .find(|p| p.metric == "bundle_size")
            .expect("bundle prediction");
        let before = bundle.before.expect("bundle before range");
        assert_eq!(
            (before.low, before.point, before.high),
            (90.0, 106.0, 160.0)
        );
        let after = bundle.after.expect("bundle after range");
        assert!(
            after.low >= 11.0,
            "bundle after low must be at least the measured 0-plugin floor for {}",
            fixture.display()
        );
    }
}

#[test]
fn test_simulate_intervals_widen_without_measured_runtime() {
    // The same synthetic app twice: once with a real-sized installed
    // Electron runtime (60 MB sparse file), once without. The
    // before-interval must be much wider without the measurement —
    // missing data widens the interval — while the after range (which
    // does not depend on the runtime) stays identical.
    let base = unique_temp_dir("sim-runtime");
    let with_runtime = base.join("with-runtime");
    let without_runtime = base.join("without-runtime");
    for dir in [&with_runtime, &without_runtime] {
        fs::create_dir_all(dir).expect("create synthetic app");
        fs::write(
            dir.join("package.json"),
            r#"{"name":"synthetic","main":"main.js","version":"1.0.0"}"#,
        )
        .expect("write package.json");
        fs::write(
            dir.join("main.js"),
            "const { app, BrowserWindow } = require('electron');\nnew BrowserWindow({});\n",
        )
        .expect("write main.js");
        fs::write(dir.join("index.html"), "<!doctype html><html></html>\n")
            .expect("write index.html");
    }
    let dist = with_runtime.join("node_modules/electron/dist");
    fs::create_dir_all(&dist).expect("create dist");
    std::fs::File::create(dist.join("electron"))
        .expect("create runtime binary")
        .set_len(60_000_000)
        .expect("size runtime binary");

    let with_report = simulate_hermetic(&with_runtime);
    let without_report = simulate_hermetic(&without_runtime);
    let bundle_with = with_report
        .predictions
        .iter()
        .find(|p| p.metric == "bundle_size")
        .expect("bundle prediction");
    let bundle_without = without_report
        .predictions
        .iter()
        .find(|p| p.metric == "bundle_size")
        .expect("bundle prediction");
    let before_with = bundle_with.before.expect("before range");
    let before_without = bundle_without.before.expect("before range");
    let width_with = before_with.high - before_with.low;
    let width_without = before_without.high - before_without.low;
    assert!(
        width_without > width_with * 5.0,
        "the before interval without a measured runtime ({width_without} MB) must be much wider than with one ({width_with} MB)"
    );
    assert_eq!(
        bundle_with.after, bundle_without.after,
        "the after range must not depend on the installed runtime"
    );
    assert_eq!(
        bundle_with.confidence,
        e2r_lib::model::Confidence::High,
        "measured runtime → high confidence"
    );
    assert_eq!(
        bundle_without.confidence,
        e2r_lib::model::Confidence::Medium,
        "no runtime → medium confidence"
    );
    fs::remove_dir_all(&base).ok();
}

#[test]
fn test_simulate_determinism() {
    for fixture in all_simulate_fixtures() {
        let first = simulate_hermetic(&fixture);
        let second = simulate_hermetic(&fixture);
        let first_json = serde_json::to_string_pretty(&first).expect("serialize");
        let second_json = serde_json::to_string_pretty(&second).expect("serialize");
        assert_eq!(
            first_json,
            second_json,
            "two simulate runs must produce byte-identical JSON for {}",
            fixture.display()
        );
        assert_eq!(
            format_report(&first, ReportFormat::Human),
            format_report(&second, ReportFormat::Human),
            "human reports must be identical for {}",
            fixture.display()
        );
        assert_eq!(
            format_report(&first, ReportFormat::Markdown),
            format_report(&second, ReportFormat::Markdown),
            "markdown reports must be identical for {}",
            fixture.display()
        );
    }
}

#[test]
fn test_simulate_no_planning_chat_sample_figures() {
    // The planning chat's illustrative sample figures must never appear
    // in simulator output — as measurements, as model outputs, or as
    // predictions. The percentages are checked against the predictions
    // specifically: a computed breakdown share could legitimately equal
    // one of them on some other app, but a prediction must never be one.
    const FORBIDDEN_ANYWHERE: &[&str] = &[
        "2.1s",
        "380 MB",
        "265 MB",
        "120 MB",
        "28 MB",
        "120 MB → 28 MB",
    ];
    const FORBIDDEN_AS_PREDICTIONS: &[&str] = &["82%", "94%", "22%", "9%"];
    for fixture in all_simulate_fixtures() {
        let report = simulate_hermetic(&fixture);
        let json = serde_json::to_string_pretty(&report).expect("serialize");
        let human = format_report(&report, ReportFormat::Human);
        let markdown = format_report(&report, ReportFormat::Markdown);
        let predictions_json =
            serde_json::to_string_pretty(&report.predictions).expect("serialize");
        for needle in FORBIDDEN_ANYWHERE {
            for (format, text) in [("JSON", &json), ("human", &human), ("markdown", &markdown)] {
                assert!(
                    !text.contains(needle),
                    "{format} output for {} must not contain the planning-chat sample figure {needle:?}",
                    fixture.display()
                );
            }
        }
        for needle in FORBIDDEN_AS_PREDICTIONS {
            assert!(
                !predictions_json.contains(needle),
                "predictions for {} must not contain the planning-chat sample figure {needle:?}",
                fixture.display()
            );
        }
    }
}

#[test]
fn test_simulate_per_platform_metric_ids() {
    // Every platform gets its own memory/process metric ids, so the
    // platform is explicit in the JSON output.
    let fixture = fixture_dir();
    for (platform, memory_id, process_id) in [
        (TargetPlatform::Macos, "memory_macos", "process_count_macos"),
        (
            TargetPlatform::Windows,
            "memory_windows",
            "process_count_windows",
        ),
        (TargetPlatform::Linux, "memory_linux", "process_count_linux"),
    ] {
        let report = simulate_with_options(
            &fixture,
            &SimulateOptions {
                target_platform: platform,
                calibration: CalibrationInput::Disabled,
            },
        )
        .expect("simulate");
        let metrics: Vec<&str> = report
            .predictions
            .iter()
            .map(|p| p.metric.as_str())
            .collect();
        assert!(
            metrics.contains(&memory_id),
            "{platform:?} report must carry {memory_id}; got {metrics:?}"
        );
        assert!(
            metrics.contains(&process_id),
            "{platform:?} report must carry {process_id}; got {metrics:?}"
        );
        // No bare `memory` / `process_count` ids — the platform is
        // always explicit.
        assert!(
            !metrics.contains(&"memory"),
            "bare memory id must not appear"
        );
        assert!(
            !metrics.contains(&"process_count"),
            "bare process_count id must not appear"
        );
    }
}
