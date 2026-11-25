#!/usr/bin/env bash
# smoke-test.sh — prove the e2r MCP server answers, with no agent involved.
#
# Starts `e2r serve`, speaks the MCP stdio protocol (initialize,
# notifications/initialized, tools/list, one tools/call against the repo's
# minimal fixture), prints PASS/FAIL per step, and exits non-zero on any
# failure. A hung server fails loudly instead of hanging forever.
#
# Usage:
#   ./smoke-test.sh
#
# Environment:
#   E2R_BIN      path to the e2r binary (default: `e2r` on PATH; falls back to
#                target/debug/e2r then target/release/e2r in this repo)
#   E2R_FIXTURE  app dir for the tools/call step (default: the repo fixture
#                converters/electron/fixtures/minimal-electron-app)
#   E2R_TIMEOUT  seconds for the whole conversation (default: 30)
#
# Needs only bash + python3 (no jq).

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

E2R_BIN="${E2R_BIN:-e2r}"
E2R_FIXTURE="${E2R_FIXTURE:-$REPO_ROOT/converters/electron/fixtures/minimal-electron-app}"
E2R_TIMEOUT="${E2R_TIMEOUT:-30}"

fail() { echo "FAIL: $1" >&2; exit 1; }

# --- prerequisites ---------------------------------------------------------

command -v python3 >/dev/null 2>&1 || fail "python3 not found on PATH"

# Resolve the binary: an explicit E2R_BIN wins as-is; otherwise prefer PATH,
# then the repo's debug/release builds (handy before `cargo install`).
if [ "$E2R_BIN" = "e2r" ] && ! command -v e2r >/dev/null 2>&1; then
    if [ -x "$REPO_ROOT/target/debug/e2r" ]; then
        E2R_BIN="$REPO_ROOT/target/debug/e2r"
        echo "note: e2r not on PATH, using $E2R_BIN"
    elif [ -x "$REPO_ROOT/target/release/e2r" ]; then
        E2R_BIN="$REPO_ROOT/target/release/e2r"
        echo "note: e2r not on PATH, using $E2R_BIN"
    else
        fail "e2r not found on PATH and no target/debug/e2r or target/release/e2r in $REPO_ROOT (install it: cargo install --path converters/electron/crates/e2r-cli)"
    fi
fi

[ -d "$E2R_FIXTURE" ] || fail "fixture dir not found: $E2R_FIXTURE"

# --- MCP conversation (python3 drives the stdio protocol) ------------------

python3 - "$E2R_BIN" "$E2R_FIXTURE" "$E2R_TIMEOUT" <<'PYEOF'
import json
import os
import select
import subprocess
import sys
import tempfile
import time

E2R_BIN, FIXTURE, TIMEOUT = sys.argv[1], sys.argv[2], float(sys.argv[3])

EXPECTED_TOOLS = {
    "analyse_electron_app",
    "get_migration_plan",
    "get_rust_replacement",
    "simulate_migration",
    "validate_migration",
    "get_decision_log",
}

# Five steps: initialize, notifications/initialized, tools/list,
# tools/call analyse_electron_app, server exits on stdin EOF.
TOTAL_STEPS = 5

passed = []
failed = []


def step(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    (passed if ok else failed).append(name)


class Server:
    """One `e2r serve` process, spoken to over newline-delimited JSON-RPC."""

    def __init__(self, argv, deadline, stderr_file):
        self.proc = subprocess.Popen(
            argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=stderr_file, text=True, bufsize=1,
        )
        self.deadline = deadline
        self.buf = b""

    def send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def recv(self, what):
        """Read one JSON-RPC message, or return (None, reason)."""
        fd = self.proc.stdout.fileno()
        while b"\n" not in self.buf:
            remaining = self.deadline - time.monotonic()
            if remaining <= 0:
                return None, f"timed out waiting for {what}"
            r, _, _ = select.select([fd], [], [], min(remaining, 1.0))
            if not r:
                if self.proc.poll() is not None:
                    return None, (
                        f"server exited (code {self.proc.returncode}) "
                        f"while waiting for {what}"
                    )
                continue
            chunk = os.read(fd, 4096)
            if not chunk:
                return None, f"server closed stdout while waiting for {what}"
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        try:
            return json.loads(line.decode("utf-8")), None
        except json.JSONDecodeError as e:
            return None, f"server sent invalid JSON for {what}: {e}: {line[:200]!r}"

    def close(self):
        """Close stdin: a well-behaved stdio server exits on EOF."""
        try:
            self.proc.stdin.close()
        except (BrokenPipeError, ValueError):
            pass
        try:
            self.proc.wait(timeout=5)
            step("server exits on stdin EOF", True,
                 f"exit code {self.proc.returncode}")
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
            step("server exits on stdin EOF", False,
                 "server did not exit within 5s of stdin EOF (killed)")


def content_text(result):
    return "".join(
        b.get("text", "") for b in result.get("content", [])
        if isinstance(b, dict))


def main():
    deadline = time.monotonic() + TIMEOUT
    stderr_file = tempfile.TemporaryFile(mode="w+")

    try:
        server = Server([E2R_BIN, "serve"], deadline, stderr_file)
    except OSError as e:
        step("start server", False, f"could not launch {E2R_BIN!r} serve: {e}")
        return report(stderr_file)

    try:
        run_conversation(server)
    except (BrokenPipeError, OSError) as e:
        step("MCP conversation", False, f"server connection broke: {e}")
    finally:
        server.close()
    return report(stderr_file)


def run_conversation(server):
    # 1. initialize
    server.send({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "e2r-smoke-test", "version": "0.1.0"},
        },
    })
    msg, err = server.recv("initialize response")
    if err:
        step("initialize", False, err)
        return
    if "error" in msg:
        step("initialize", False, f"server error: {msg['error']}")
        return
    result = msg.get("result", {})
    server_name = result.get("serverInfo", {}).get("name")
    proto = result.get("protocolVersion")
    ok = server_name == "e2r" and bool(proto)
    step("initialize", ok,
         f"server={server_name!r} protocolVersion={proto!r}" if ok
         else f"unexpected result: {json.dumps(result)[:200]}")
    if not ok:
        return

    # 2. notifications/initialized (no response expected)
    server.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    step("notifications/initialized", True, "sent (no response expected)")

    # 3. tools/list — all six tools must be present
    server.send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    msg, err = server.recv("tools/list response")
    if err:
        step("tools/list", False, err)
    elif "error" in msg:
        step("tools/list", False, f"server error: {msg['error']}")
    else:
        names = {t.get("name") for t in msg.get("result", {}).get("tools", [])}
        missing = EXPECTED_TOOLS - names
        step("tools/list", not missing,
             f"{len(names)} tools: {sorted(names)}" if not missing
             else f"missing: {sorted(missing)}; got: {sorted(names)}")

    # 4. tools/call analyse_electron_app on the minimal fixture
    server.send({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {
            "name": "analyse_electron_app",
            "arguments": {"app_dir": FIXTURE},
        },
    })
    msg, err = server.recv("tools/call response")
    if err:
        step("tools/call analyse_electron_app", False, err)
    elif "error" in msg:
        step("tools/call analyse_electron_app", False,
             f"server error: {msg['error']}")
    else:
        result = msg.get("result", {})
        text = content_text(result)
        if result.get("isError"):
            step("tools/call analyse_electron_app", False,
                 f"isError: {text[:200]!r}")
        else:
            ok = bool(text) and "feasibility" in text.lower()
            step("tools/call analyse_electron_app", ok,
                 f"scan report returned ({len(text)} chars, mentions feasibility)"
                 if ok else f"unexpected content: {text[:200]!r}")


def report(stderr_file):
    # Steps that never ran (early exit) count as failed, not passed.
    ran = len(passed) + len(failed)
    if ran < TOTAL_STEPS:
        step(f"{TOTAL_STEPS - ran} step(s) skipped after an earlier failure",
             False, "see above")
    if failed:
        stderr_file.seek(0)
        tail = stderr_file.read()[-2000:]
        if tail.strip():
            print("\n--- server stderr (last 2000 chars) ---")
            print(tail.rstrip())
    print(f"\n{len(passed)}/{TOTAL_STEPS} steps passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
PYEOF
