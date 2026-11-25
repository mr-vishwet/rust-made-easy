//! MCP integration tests: drive the real `e2r serve` binary over stdio
//! and prove the wire protocol works end to end — initialize handshake,
//! `tools/list` (exactly the six tools, with input schemas), one
//! `tools/call` per tool against the fixtures, unknown-tool and
//! malformed-input errors — plus direct handler checks for the
//! structured-JSON shape of every tool.
//!
//! All fast: no `cargo check` anywhere. The `serve` binary is spawned
//! via `CARGO_BIN_EXE_e2r` (built by the test harness).

use e2r_lib::convert::convert;
use e2r_lib::decisions;
use e2r_lib::mcp;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A fixture app, relative to this crate's manifest dir.
fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

/// A fresh scratch dir per test run (no `tempfile` dependency: process id
/// + timestamp is unique enough for a test). Under the crate target dir,
///   not `/tmp` (see the golden tests' note on the 512 MB tmpfs).
fn scratch_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-scratch")
        .join(format!("mcp-it-{tag}-{}-{nanos}", std::process::id()))
}

/// Spawn `e2r serve`, feed it `requests` (one JSON-RPC object per line,
/// notifications included), close stdin, and collect every response
/// line until the server exits (it exits on stdin EOF). Returns the
/// responses indexed by their `id`, plus any response without an `id`
/// (there should be none — notifications get no reply).
///
/// Panics when the server does not exit within 60 seconds.
fn serve_exchange(requests: &[Value], extra_env: &[(&str, &Path)]) -> Vec<Value> {
    let mut child: Child = {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_e2r"));
        cmd.arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        cmd.spawn().expect("failed to spawn `e2r serve`")
    };
    let mut stdin = child.stdin.take().expect("server stdin");
    for request in requests {
        writeln!(stdin, "{request}").expect("write request to server stdin");
    }
    drop(stdin); // EOF: the server exits after answering.

    // Read stdout on a thread so a hung server cannot hang the test.
    let stdout = child.stdout.take().expect("server stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let mut lines = Vec::new();
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) if !line.trim().is_empty() => lines.push(line),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let _ = tx.send(lines);
    });
    let lines = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("server did not answer within 60s");
    let status = child.wait().expect("wait for server");
    assert!(
        status.success(),
        "server exited with non-zero status: {status}"
    );
    lines
        .iter()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|_| panic!("server response is not JSON: {line}"))
        })
        .collect()
}

/// Find the response with the given numeric `id`.
fn by_id(responses: &[Value], id: u64) -> &Value {
    responses
        .iter()
        .find(|r| r["id"] == json!(id))
        .unwrap_or_else(|| panic!("no response with id {id} in {responses:?}"))
}

/// The `result` of a response, panicking with the error when the
/// response is an error.
fn result_of(responses: &[Value], id: u64) -> &Value {
    let response = by_id(responses, id);
    assert!(
        response.get("error").is_none(),
        "response {id} is an error: {}",
        response["error"]
    );
    &response["result"]
}

/// The structured content of a successful `tools/call` response.
fn tool_json(responses: &[Value], id: u64) -> &Value {
    let result = result_of(responses, id);
    assert_eq!(
        result["isError"], false,
        "tool call {id} unexpectedly errored: {}",
        result["content"][0]["text"]
    );
    result
        .get("structuredContent")
        .unwrap_or_else(|| panic!("tool call {id} has no structuredContent: {result:?}"))
}

/// The structured error object of a failed (`isError`) `tools/call`.
fn tool_error_json(responses: &[Value], id: u64) -> &Value {
    let result = result_of(responses, id);
    assert_eq!(
        result["isError"], true,
        "tool call {id} unexpectedly succeeded: {result:?}"
    );
    result
        .get("structuredContent")
        .unwrap_or_else(|| panic!("tool call {id} has no structuredContent: {result:?}"))
}

fn initialize_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "mcp-integration-test", "version": "0.1.0" }
        }
    })
}

fn initialized_notification() -> Value {
    json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
}

fn tools_list_request(id: u64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list" })
}

fn tools_call_request(id: u64, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    })
}

#[test]
fn test_stdio_initialize_handshake() {
    let responses = serve_exchange(&[initialize_request(1), initialized_notification()], &[]);
    assert_eq!(responses.len(), 1, "notifications get no response");
    let result = result_of(&responses, 1);
    assert_eq!(result["protocolVersion"], "2025-03-26");
    assert_eq!(result["serverInfo"]["name"], "e2r");
    assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(result["capabilities"]["tools"].is_object());
}

#[test]
fn test_stdio_tools_list_exactly_six_tools() {
    let responses = serve_exchange(
        &[
            initialize_request(1),
            initialized_notification(),
            tools_list_request(2),
        ],
        &[],
    );
    let result = result_of(&responses, 2);
    let tools = result["tools"].as_array().expect("tools is an array");
    let mut names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("tool name is a string"))
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "analyse_electron_app",
            "get_decision_log",
            "get_migration_plan",
            "get_rust_replacement",
            "simulate_migration",
            "validate_migration",
        ]
    );
    for tool in tools {
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object", "tool {}", tool["name"]);
        assert!(schema["properties"].is_object(), "tool {}", tool["name"]);
        assert!(
            tool["description"].as_str().unwrap_or("").len() > 10,
            "tool {} has no useful description",
            tool["name"]
        );
    }
    // Spot-check the required arguments.
    let by_name = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("tool {name} missing"))
    };
    assert_eq!(
        by_name("analyse_electron_app")["inputSchema"]["required"],
        json!(["app_dir"])
    );
    assert_eq!(
        by_name("validate_migration")["inputSchema"]["required"],
        json!(["project_dir"])
    );
    assert_eq!(
        by_name("simulate_migration")["inputSchema"]["required"],
        json!(["app_dir"])
    );
}

#[test]
fn test_stdio_tools_call_all_six_tools() {
    let scratch = scratch_dir("all-six");
    let no_calibration = scratch.join("calibration.jsonl");
    let decisions_log = scratch.join("decisions.jsonl");
    // A converted project for validate_migration / get_decision_log.
    let converted = scratch.join("converted");
    convert(&fixture_dir("minimal-electron-app"), &converted)
        .expect("convert the fixture for the validate test");
    let app_dir = fixture_dir("minimal-electron-app");

    let responses = serve_exchange(
        &[
            initialize_request(1),
            initialized_notification(),
            tools_call_request(
                3,
                "analyse_electron_app",
                json!({ "app_dir": app_dir.display().to_string() }),
            ),
            tools_call_request(
                4,
                "get_migration_plan",
                json!({ "app_dir": app_dir.display().to_string() }),
            ),
            tools_call_request(
                5,
                "get_rust_replacement",
                json!({ "api": "dialog.showOpenDialog" }),
            ),
            tools_call_request(
                6,
                "simulate_migration",
                json!({ "app_dir": app_dir.display().to_string(), "target_platform": "macos" }),
            ),
            tools_call_request(
                7,
                "validate_migration",
                json!({
                    "project_dir": converted.display().to_string(),
                    "original_app_dir": app_dir.display().to_string(),
                }),
            ),
            tools_call_request(
                8,
                "get_decision_log",
                json!({ "project_dir": converted.display().to_string() }),
            ),
            tools_call_request(9, "ping", json!({})),
        ],
        &[
            (
                e2r_lib::calibrate::CALIBRATION_LOG_ENV,
                no_calibration.as_path(),
            ),
            (decisions::DECISIONS_LOG_ENV, decisions_log.as_path()),
        ],
    );

    // (id 9 is a tools/call named "ping", which is not a tool — the
    // real ping is a separate request; assert it errors as unknown.)
    let analyse = tool_json(&responses, 3);
    assert_eq!(analyse["app_name"], "minimal-electron-app");
    assert!(analyse["feasibility_score"].is_number());
    assert!(analyse["findings"].is_array());

    let plan = tool_json(&responses, 4);
    assert_eq!(plan["app_name"], "minimal-electron-app");
    assert_eq!(plan["steps"].as_array().unwrap().len(), 4);
    assert_eq!(plan["steps"][0]["name"], "analyse");
    assert_eq!(plan["steps"][3]["name"], "validate");
    assert!(plan["replacements"].is_array());
    assert!(plan["effort"].is_string());

    let replacement = tool_json(&responses, 5);
    assert_eq!(replacement["electron_api"], "dialog.showOpenDialog");
    assert!(replacement["tauri_target"].is_string());
    assert_eq!(replacement["status"], "auto");

    let simulation = tool_json(&responses, 6);
    assert_eq!(simulation["app_name"], "minimal-electron-app");
    assert!(simulation["breakdown"].is_object());
    assert!(simulation["predictions"].is_array());
    assert_eq!(simulation["model_version"], "0.1.0");

    let validation = tool_json(&responses, 7);
    assert_eq!(validation["is_e2r_project"], true);
    assert!(validation["missing"].as_array().unwrap().is_empty());
    assert_eq!(validation["renderer_carried_over"], true);
    assert!(validation["todo_stubs"].is_number());
    assert!(validation["next_steps"].as_array().unwrap().len() >= 2);

    let log = tool_json(&responses, 8);
    assert!(log["count"].as_u64().unwrap() > 0);
    assert_eq!(
        log["entries"].as_array().unwrap().len() as u64,
        log["count"].as_u64().unwrap()
    );
    assert_eq!(log["entries"][0]["version"], 1);

    let unknown = by_id(&responses, 9);
    assert!(
        unknown.get("error").is_some(),
        "unknown tool must be a JSON-RPC error, got: {unknown:?}"
    );
    assert_eq!(unknown["error"]["code"], -32602);

    fs::remove_dir_all(&scratch).ok();
}

#[test]
fn test_stdio_ping_and_unknown_method() {
    let responses = serve_exchange(
        &[
            initialize_request(1),
            initialized_notification(),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" }),
        ],
        &[],
    );
    assert_eq!(result_of(&responses, 2), &json!({}));
    let unknown = by_id(&responses, 3);
    assert!(unknown.get("error").is_some(), "unknown method must error");
}

#[test]
fn test_stdio_malformed_input_is_error_never_panic() {
    let responses = serve_exchange(
        &[
            initialize_request(1),
            initialized_notification(),
            // Missing required `app_dir`.
            tools_call_request(2, "analyse_electron_app", json!({})),
            // Nonexistent directory.
            tools_call_request(
                3,
                "analyse_electron_app",
                json!({ "app_dir": "/definitely/not/a/real/dir" }),
            ),
            // Unknown API.
            tools_call_request(4, "get_rust_replacement", json!({ "api": "nope.api" })),
            // Bad target platform.
            tools_call_request(
                5,
                "simulate_migration",
                json!({
                    "app_dir": fixture_dir("minimal-electron-app").display().to_string(),
                    "target_platform": "amiga",
                }),
            ),
            // Not an e2r project.
            tools_call_request(
                6,
                "validate_migration",
                json!({ "project_dir": fixture_dir("minimal-electron-app").display().to_string() }),
            ),
        ],
        &[],
    );

    // Missing required argument: rmcp rejects the call as an MCP-shaped
    // error (isError, no structured content) rather than a JSON-RPC
    // protocol error — still an error, never a panic.
    let missing_arg = result_of(&responses, 2);
    assert_eq!(missing_arg["isError"], true);
    assert!(
        missing_arg["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("missing field `app_dir`"),
        "unexpected error text: {}",
        missing_arg["content"][0]["text"]
    );

    // The rest are MCP-shaped errors: isError + structured error object.
    let not_found = tool_error_json(&responses, 3);
    assert_eq!(not_found["code"], "NOT_FOUND");
    let api_not_found = tool_error_json(&responses, 4);
    assert_eq!(api_not_found["code"], "API_NOT_FOUND");
    let bad_platform = tool_error_json(&responses, 5);
    assert_eq!(bad_platform["code"], "INVALID_ARGUMENT");
    let not_e2r = tool_error_json(&responses, 6);
    assert_eq!(not_e2r["code"], "NOT_E2R_PROJECT");

    // The server survived all of it: it exited 0 (asserted in
    // serve_exchange) after answering every request.
}

// --- Direct handler checks (no transport) ----------------------------------

#[test]
fn test_handlers_return_structured_json_for_every_tool() {
    let scratch = scratch_dir("handlers");
    let no_calibration = scratch.join("calibration.jsonl");
    std::env::set_var(e2r_lib::calibrate::CALIBRATION_LOG_ENV, &no_calibration);
    let converted = scratch.join("converted");
    convert(&fixture_dir("full-apis-electron-app"), &converted).expect("convert the fixture");

    let app_dir = fixture_dir("full-apis-electron-app");
    let analyse = mcp::analyse_electron_app(&app_dir).expect("analyse");
    assert_eq!(analyse["app_name"], "full-apis-electron-app");

    let plan = mcp::get_migration_plan(&app_dir).expect("plan");
    assert_eq!(plan["steps"].as_array().unwrap().len(), 4);
    // The full-APIs fixture has no MANUAL findings, so its manual_first
    // is empty; the synthetic app below proves the MANUAL call-out.

    let all = mcp::get_rust_replacement("", true).expect("list_all");
    // 96 mappings entries (48 before the proof-campaign fix round added
    // member-level app.* / net.* and the missing APIs; fix round 2 added
    // the `webview` entry).
    assert_eq!(all["count"], 96);

    let simulation = mcp::simulate_migration(&app_dir, Some("windows")).expect("simulate");
    assert!(simulation["predictions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["metric"] == "process_count_windows"));

    let validation = mcp::validate_migration(&converted, Some(&app_dir)).expect("validate");
    assert_eq!(validation["is_e2r_project"], true);
    assert!(validation["todo_stubs"].as_u64().unwrap() > 0);

    let log = mcp::get_decision_log(Some(&converted)).expect("decision log");
    assert!(log["count"].as_u64().unwrap() > 0);

    fs::remove_dir_all(&scratch).ok();
}

#[test]
fn test_migration_plan_calls_manual_items_out_first() {
    // A synthetic app using two MANUAL APIs (powerMonitor, session):
    // the plan must list both in manual_first, with their replacements
    // marked manual and their Tauri targets null.
    let scratch = scratch_dir("manual-plan");
    let app_dir = scratch.join("manual-app");
    fs::create_dir_all(&app_dir).unwrap();
    fs::write(
        app_dir.join("package.json"),
        r#"{"name":"manual-app","version":"1.0.0","main":"main.js","dependencies":{"electron":"30.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        app_dir.join("main.js"),
        "const { app, BrowserWindow, powerMonitor, session } = require('electron');\n\
         powerMonitor.on('suspend', () => {});\n\
         const ses = session.defaultSession;\n\
         function createWindow() { const w = new BrowserWindow({ width: 800, height: 600 }); }\n\
         app.whenReady().then(createWindow);\n",
    )
    .unwrap();
    std::env::set_var(
        e2r_lib::calibrate::CALIBRATION_LOG_ENV,
        scratch.join("calibration.jsonl"),
    );

    let plan = mcp::get_migration_plan(&app_dir).expect("plan");
    let manual = plan["manual_first"].as_array().unwrap();
    let apis: Vec<&str> = manual.iter().map(|m| m["api"].as_str().unwrap()).collect();
    assert_eq!(apis, ["powerMonitor", "session"]);
    for item in plan["replacements"].as_array().unwrap() {
        if item["api"] == "powerMonitor" || item["api"] == "session" {
            assert_eq!(item["classification"], "manual");
            // No plugin covers a manual API (a manual row may still carry
            // a descriptive tauri_target string — `session` does).
            assert!(item["plugin"].is_null());
        }
    }
    // MANUAL share > 0, so the effort hint is at least medium.
    assert!(matches!(
        plan["effort"].as_str(),
        Some("medium") | Some("large")
    ));
    fs::remove_dir_all(&scratch).ok();
}

#[test]
fn test_convert_writes_decision_entries() {
    let scratch = scratch_dir("convert-decisions");
    let out = scratch.join("out");
    convert(&fixture_dir("minimal-electron-app"), &out).expect("convert");
    let path = decisions::default_path(Some(&out));
    assert!(path.is_file(), "decision log missing at {}", path.display());
    let log = decisions::read_with_stats(&path);
    assert!(!log.entries.is_empty());
    assert_eq!(log.skipped, 0);
    // One entry per finding in the scan.
    let scan = e2r_lib::scan::scan(&fixture_dir("minimal-electron-app")).expect("scan");
    assert_eq!(log.entries.len(), scan.findings.len());
    assert!(log
        .entries
        .iter()
        .all(|e| e.version == decisions::DECISION_FORMAT_VERSION));
    fs::remove_dir_all(&scratch).ok();
}

#[test]
fn test_decision_log_round_trip_and_corrupt_lines() {
    let scratch = scratch_dir("decisions-it");
    let path = scratch.join(".e2r").join("decisions.jsonl");
    let entries = vec![
        decisions::DecisionEntry::new(
            "shell.openExternal",
            Some("tauri-plugin-opener".to_string()),
            e2r_lib::model::Classification::Auto,
            "Direct Tauri equivalent exists.",
            decisions::DecisionConfidence::High,
            decisions::DecisionSource::MappingsDb,
        ),
        decisions::DecisionEntry::new(
            "Tray",
            None,
            e2r_lib::model::Classification::Manual,
            "No direct Tauri equivalent.",
            decisions::DecisionConfidence::Low,
            decisions::DecisionSource::Rule,
        ),
    ];
    assert_eq!(decisions::append(&path, &entries).unwrap(), 2);
    let read = decisions::read(&path);
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].electron_api, "shell.openExternal");
    assert_eq!(read[1].tauri_replacement, None);

    // Corrupt lines are skipped and counted, good lines survive.
    fs::create_dir_all(scratch.join(".e2r")).unwrap();
    let mut contents = fs::read_to_string(&path).unwrap();
    contents.push_str("garbage line\n{\"version\": 99}\n");
    fs::write(&path, contents).unwrap();
    let read = decisions::read_with_stats(&path);
    assert_eq!(read.entries.len(), 2);
    assert_eq!(read.skipped, 2);
    fs::remove_dir_all(&scratch).ok();
}

#[test]
fn test_stdio_server_exits_promptly_on_eof() {
    // A server that hangs on EOF would hang every MCP client; prove the
    // whole exchange (spawn → answer → exit) is fast.
    let started = Instant::now();
    let responses = serve_exchange(&[initialize_request(1)], &[]);
    assert_eq!(responses.len(), 1);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "serve exchange took {:?}, expected < 10s",
        started.elapsed()
    );
}
