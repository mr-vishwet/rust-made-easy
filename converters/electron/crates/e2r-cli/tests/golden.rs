//! Golden tests: scan + convert the `minimal-electron-app` fixture and
//! verify the generated Tauri 2 project compiles.
//!
//! The `cargo check` step in `test_convert_fixture` needs Tauri's Linux
//! system prerequisites (GTK + WebKit dev libraries); see
//! https://v2.tauri.app/start/prerequisites/ — without them the check fails
//! in `gdk-sys`/`webkit2gtk-sys` build scripts, not in generated code.

use e2r_lib::convert::convert;
use e2r_lib::scan::scan;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The fixture app, relative to this crate's manifest dir.
fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/minimal-electron-app")
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

    // The generated project must compile. First run downloads Tauri + plugins
    // from crates.io, so this takes minutes — that is expected.
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
    let mut child: Child = Command::new("cargo")
        .args(["check", "--manifest-path"])
        .arg(src_tauri.join("Cargo.toml"))
        .env("CARGO_TERM_COLOR", "never")
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
