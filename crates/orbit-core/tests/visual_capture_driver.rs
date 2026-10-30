//! Regression check: the visual capture driver runs a UI rule only at the
//! widths the rule declares, as the canonical ux-rules runner does.
#![allow(clippy::unwrap_used)]

use std::process::Command;

#[cfg(unix)]
use std::process::{Child, Stdio};

#[cfg(unix)]
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const DRIVER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/visual/capture-driver.mjs"
);

// Loads the real driver and runs its per-mock rule loop for each case with
// stub checks that report the width they ran at. Prints the [rule, width]
// pairs per case, which is what rules.json would record for that mock.
// The driver path comes last so the driver's own entry-point guard stays off.
const PROBE: &str = r#"
import { pathToFileURL } from "node:url";
const cases = JSON.parse(process.argv[1]);
const driver = await import(pathToFileURL(process.argv[2]).href);
const out = [];
for (const { rules, mock } of cases) {
  const modules = rules.map((rule) => ({
    rule,
    check: async (ctx) => ({ rule: rule.id, status: "pass", width: ctx.target.width }),
  }));
  const results = await driver.runRulesForMock(modules, { blocked: [] }, mock, null, {}, "");
  out.push(results.map((result) => [result.rule, result.width]));
}
process.stdout.write(JSON.stringify(out));
"#;

const IMAGE_LIMIT_PROBE: &str = r#"
import { writeFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";
const outDir = process.argv[1];
const driver = await import(pathToFileURL(process.argv[2]).href);
const mockFile = path.join(outDir, "mock.png");
await writeFile(mockFile, Buffer.alloc(128, 2));
const makeBrowser = (captureBytes, sideBySideBytes) => ({
  newContext: async () => {
    const page = {
      on() {},
      route: async () => {},
      clock: { install: async () => {}, pauseAt: async () => {}, runFor: async () => {} },
      addInitScript: async () => {},
      goto: async () => {},
      addStyleTag: async () => {},
      locator() { return { first() { return this; }, waitFor: async () => {} }; },
      waitForLoadState: async () => {},
      evaluate: async (_fn, args) => {
        if (args?.quality !== undefined) {
          return `data:image/webp;base64,${Buffer.alloc(2048, 7).toString("base64")}`;
        }
        if (args?.mockUrl) return { width: 100, height: 100 };
        return undefined;
      },
      screenshot: async (options = {}) => {
        const bytes = Buffer.alloc(options.path ? sideBySideBytes : captureBytes, 1);
        if (options.path) await writeFile(options.path, bytes);
        return bytes;
      },
      setContent: async () => {},
      setViewportSize: async () => {},
    };
    return { newPage: async () => page, close: async () => {} };
  },
});
const cases = [
  { name: "capture", captureBytes: 2048, sideBySideBytes: 512 },
  { name: "side-by-side", captureBytes: 512, sideBySideBytes: 2048 },
];
const rows = [];
for (const testCase of cases) {
  const name = testCase.name;
  const request = {
    out_dir: path.join(outDir, name),
    captures_per_mock: 1,
    frozen_instant: "2026-01-01T12:00:00.000Z",
    max_image_bytes: 1024,
  };
  const mock = {
    artifact: `mocks/${name}.png`, name, view: "example", state: "default",
    theme: "dark", width: 1440, mock_ext: "png", mock_file: mockFile,
    view_def: { fixture: "fixture", route: "/", ready: "body", responsive: true },
    app_def: { name: "example" },
  };
  const entry = await driver.captureMock({
    browser: makeBrowser(testCase.captureBytes, testCase.sideBySideBytes),
    PNG: {}, request, mock, server: { baseUrl: "http://127.0.0.1:1234", placeholders: {} },
    ruleModules: [], ruleResults: [],
  });
  const captureJson = JSON.parse(JSON.stringify({ mocks: [entry] }));
  const reported = captureJson.mocks[0];
  rows.push({
    error: reported.error,
    claims_capture: Object.hasOwn(reported, "capture"),
    claims_side_by_side: Object.hasOwn(reported, "side_by_side"),
  });
}
process.stdout.write(JSON.stringify(rows));
"#;

fn mock(width: u64, responsive: bool) -> Value {
    json!({
        "artifact": format!("tasks-list-dark-{width}.png"),
        "view": "tasks-list",
        "state": "default",
        "theme": "dark",
        "width": width,
        "view_def": { "id": "tasks-list", "responsive": responsive },
        "app_def": { "name": "orbit-web" },
    })
}

fn rule(id: &str, widths: Option<Value>) -> Value {
    let mut targets = json!({ "views": "*" });
    if let Some(widths) = widths {
        targets["widths"] = widths;
    }
    json!({ "id": id, "targets": targets })
}

fn run_probe(cases: &[Value]) -> Vec<Value> {
    let output = Command::new("node")
        .arg("--input-type=module")
        .arg("-e")
        .arg(PROBE)
        .arg(Value::Array(cases.to_vec()).to_string())
        .arg(DRIVER)
        .output()
        .unwrap_or_else(|error| panic!("node must be on PATH to load the capture driver: {error}"));
    assert!(
        output.status.success(),
        "rule probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn rules_run_only_at_declared_widths() {
    let two_widths = rule("ui.two-widths", Some(json!([1440, 375])));
    let three_widths = rule("ui.three-widths", Some(json!([1440, 1166, 375])));
    let default_width = rule("ui.default-width", None);
    // (label, rules, mock, expected [rule, width] pairs)
    let cases = [
        (
            "undeclared width 1166 is not run",
            vec![two_widths.clone()],
            mock(1166, true),
            json!([]),
        ),
        (
            "declared width 1440 is run",
            vec![two_widths.clone()],
            mock(1440, true),
            json!([["ui.two-widths", 1440]]),
        ),
        (
            "declared width 375 is run",
            vec![two_widths.clone()],
            mock(375, true),
            json!([["ui.two-widths", 375]]),
        ),
        (
            "width 1166 is run when the rule declares it",
            vec![three_widths],
            mock(1166, true),
            json!([["ui.three-widths", 1166]]),
        ),
        (
            "omitted widths default to 1440, so 375 is not run",
            vec![default_width.clone()],
            mock(375, true),
            json!([]),
        ),
        (
            "omitted widths default to 1440, which is run",
            vec![default_width],
            mock(1440, true),
            json!([["ui.default-width", 1440]]),
        ),
        (
            "non-responsive view is not run at a width past its first declared width",
            vec![two_widths.clone()],
            mock(375, false),
            json!([]),
        ),
        (
            "non-responsive view is run at its first declared width",
            vec![two_widths],
            mock(1440, false),
            json!([["ui.two-widths", 1440]]),
        ),
    ];
    let payload: Vec<Value> = cases
        .iter()
        .map(|(_, rules, mock, _)| json!({ "rules": rules, "mock": mock }))
        .collect();
    let recorded = run_probe(&payload);
    assert_eq!(recorded.len(), cases.len());
    for ((label, _, _, expected), actual) in cases.iter().zip(&recorded) {
        assert_eq!(actual, expected, "{label}");
    }
}

#[test]
fn images_over_the_limit_are_reported_without_capture_paths() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new("node")
        .arg("--input-type=module")
        .arg("-e")
        .arg(IMAGE_LIMIT_PROBE)
        .arg(temp.path())
        .arg(DRIVER)
        .output()
        .unwrap_or_else(|error| panic!("node must be on PATH to load the capture driver: {error}"));
    assert!(
        output.status.success(),
        "image limit probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.len(), 2, "one row per probe case: {rows:?}");
    for row in &rows {
        assert_eq!(row["error"]["code"], "image_over_budget", "row: {row}");
        assert_eq!(
            row["claims_capture"], false,
            "over-limit capture was claimed: {row}"
        );
        assert_eq!(
            row["claims_side_by_side"], false,
            "over-limit side-by-side was claimed: {row}"
        );
    }
}

#[test]
fn timed_out_fixture_setup_process_is_stopped_before_rejection() {
    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("setup.pid");
    let probe = r#"
import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
const pidFile = process.argv[1];
const driver = await import(pathToFileURL(process.argv[2]).href);
const setup = "require('node:fs').writeFileSync(process.argv[1], String(process.pid)); setInterval(() => {}, 1000);";
let failure;
try {
  await driver.startFixtureServer(
    { name: "leaky", setup: ["node", "-e", setup, pidFile], setup_timeout_s: 1 },
    "fixture",
    process.cwd(),
  );
} catch (error) {
  failure = error;
}
if (!failure || !failure.message.includes("timed out")) {
  throw new Error(`expected setup timeout, received ${failure?.message ?? "success"}`);
}
const pid = Number(await readFile(pidFile, "utf8"));
let alive = false;
try {
  process.kill(pid, 0);
  alive = true;
} catch (error) {
  if (error.code !== "ESRCH") throw error;
}
if (alive) {
  try {
    process.kill(-pid, "SIGKILL");
  } catch {
    try { process.kill(pid, "SIGKILL"); } catch {}
  }
  throw new Error(`setup process ${pid} remained alive after timeout rejection`);
}
"#;

    let output = Command::new("node")
        .arg("--input-type=module")
        .arg("-e")
        .arg(probe)
        .arg(&pid_file)
        .arg(DRIVER)
        .output()
        .unwrap_or_else(|error| panic!("node must be on PATH to load the capture driver: {error}"));
    assert!(
        output.status.success(),
        "setup cleanup probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ORB-20091: a fixture descendant that escapes the child's process group
// (detached) and inherits its stdio pipes must not hang stop(): Node only
// emits the child's "close" once those pipes close, so awaiting it (or
// leaving the pipes open) blocks the setup-timeout rejection and keeps the
// driver process alive. The probe deliberately does not call
// process.exit(), so it only finishes if stop() releases the pipes.
#[cfg(unix)]
#[test]
fn timed_out_fixture_setup_with_pipe_holding_detached_descendant_rejects_and_exits() {
    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("setup.pid");
    let descendant_pid_file = temp.path().join("descendant.pid");
    let probe = r#"
import { pathToFileURL } from "node:url";
const pidFile = process.argv[1];
const descendantPidFile = process.argv[2];
const driver = await import(pathToFileURL(process.argv[3]).href);
const setup = `
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const descendant = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000);'], { detached: true, stdio: 'inherit' });
fs.writeFileSync(process.argv[2], String(descendant.pid));
fs.writeFileSync(process.argv[1], String(process.pid));
setInterval(() => {}, 1000);
`;
const started = Date.now();
let failure;
try {
  await driver.startFixtureServer(
    { name: "leaky", setup: ["node", "-e", setup, pidFile, descendantPidFile], setup_timeout_s: 1 },
    "fixture",
    process.cwd(),
  );
} catch (error) {
  failure = error;
}
if (!failure || !failure.message.includes("timed out")) {
  throw new Error(`expected setup timeout, received ${failure?.message ?? "success"}`);
}
const elapsedMs = Date.now() - started;
if (elapsedMs >= 10000) {
  throw new Error(`setup timeout rejection took ${elapsedMs}ms; stop() must complete within a bound`);
}
"#;

    let mut child = Command::new("node")
        .arg("--input-type=module")
        .arg("-e")
        .arg(probe)
        .arg(&pid_file)
        .arg(&descendant_pid_file)
        .arg(DRIVER)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("node must be on PATH to load the capture driver: {error}"));

    let deadline = Duration::from_secs(20);
    let started = Instant::now();
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        if started.elapsed() >= deadline {
            cleanup_child(&mut child);
            kill_pid_file(&descendant_pid_file);
            panic!(
                "capture driver probe did not exit within {deadline:?}; a pipe-holding \
                 detached descendant hung stop() or kept the driver alive (ORB-20091)"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let descendant_alive = pid_file_alive(&descendant_pid_file);
    let fixture_alive = pid_file_alive(&pid_file);
    kill_pid_file(&descendant_pid_file);
    let _ = child.wait();
    assert!(
        status.success(),
        "pipe-holding descendant probe failed with {status:?} (ORB-20091)"
    );
    assert!(
        descendant_alive,
        "detached descendant must survive the fixture process-group kill, \
         otherwise this test does not cover an escaped pipe holder (ORB-20091)"
    );
    assert!(
        !fixture_alive,
        "fixture setup process must be dead after the timeout rejection (ORB-20091)"
    );
}

// ORB-20101: stop() left its post-KILL bound timer armed after the fixture had
// already closed, so a driver whose fixture stopped normally lingered for the
// full bound before exiting. The probe does not call process.exit(); it
// reports how long the event loop stayed alive after stop() resolved.
#[cfg(unix)]
#[test]
fn stopped_fixture_lets_driver_process_exit_promptly() {
    use std::io::Read as _;

    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("setup.pid");
    let probe = r#"
import { writeSync } from "node:fs";
import { pathToFileURL } from "node:url";
const pidFile = process.argv[1];
const driver = await import(pathToFileURL(process.argv[2]).href);
const setup = `
const fs = require('node:fs');
fs.writeFileSync(process.argv[1], String(process.pid));
console.log('ORBIT_VISUAL_URL=http://127.0.0.1:9/');
setInterval(() => {}, 1000);
`;
const server = await driver.startFixtureServer(
  { name: "prompt", setup: ["node", "-e", setup, pidFile], setup_timeout_s: 10 },
  "fixture",
  process.cwd(),
);
await server.stop();
const stoppedAt = Date.now();
process.on("exit", () => writeSync(1, String(Date.now() - stoppedAt)));
"#;

    let mut child = Command::new("node")
        .arg("--input-type=module")
        .arg("-e")
        .arg(probe)
        .arg(&pid_file)
        .arg(DRIVER)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("node must be on PATH to load the capture driver: {error}"));

    let deadline = Duration::from_secs(20);
    let started = Instant::now();
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        if started.elapsed() >= deadline {
            cleanup_child(&mut child);
            kill_pid_file(&pid_file);
            panic!("capture driver probe did not exit within {deadline:?} of stop() (ORB-20101)");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    kill_pid_file(&pid_file);
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(
        status.success(),
        "stop() probe failed with {status:?}: {stderr} (ORB-20101)"
    );
    let lingering_ms: u64 = stdout.trim().parse().unwrap_or_else(|_| {
        panic!("probe reported no exit timing; stdout: {stdout:?} (ORB-20101)")
    });
    assert!(
        lingering_ms < 2000,
        "driver process stayed alive {lingering_ms}ms after a promptly stopped fixture; \
         the post-KILL bound timer must not outlive a fixture that already closed (ORB-20101)"
    );
}

#[cfg(unix)]
fn pid_file_alive(pid_file: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(pid_file) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<i32>() else {
        return false;
    };
    (unsafe { libc::kill(pid, 0) }) == 0
}

#[cfg(unix)]
fn kill_pid_file(pid_file: &std::path::Path) {
    if let Ok(text) = std::fs::read_to_string(pid_file)
        && let Ok(pid) = text.trim().parse::<i32>()
    {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

// ORB-20090: the driver installed SIGTERM/SIGINT handlers that only flagged
// shutdown and never exited, so a supervisor SIGTERM during a long capture
// waited out the whole mock loop. The driver must exit promptly after
// fixture cleanup instead. These tests hang the browser inside the first
// capture, signal the driver, and assert it exits quickly with fixture
// cleanup, rather than running to natural completion.
#[cfg(unix)]
#[test]
fn sigterm_terminates_hanging_driver_after_fixture_cleanup() {
    run_signal_case(libc::SIGTERM, "SIGTERM", 143);
}

#[cfg(unix)]
#[test]
fn sigint_terminates_hanging_driver_after_fixture_cleanup() {
    run_signal_case(libc::SIGINT, "SIGINT", 130);
}

#[cfg(unix)]
fn run_signal_case(signal: i32, signal_name: &str, expected_code: i32) {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("capture-started");
    let pid_file = temp.path().join("setup.pid");
    let kit_dir = temp.path().join("kit");
    let png_dir = kit_dir.join("node_modules").join("pngjs");
    std::fs::create_dir_all(&png_dir).unwrap();
    std::fs::write(kit_dir.join("package.json"), "{}\n").unwrap();
    std::fs::write(
        png_dir.join("package.json"),
        "{\"name\":\"pngjs\",\"version\":\"0.0.0\",\"main\":\"index.js\"}\n",
    )
    .unwrap();
    std::fs::write(png_dir.join("index.js"), "exports.PNG = {};\n").unwrap();

    let stub = temp.path().join("stub-playwright.mjs");
    std::fs::write(
        &stub,
        r#"export const chromium = {
  launch: async () => ({
    version: () => "stub",
    close: async () => {},
    newContext: async () => {
      const marker = process.env.DRIVER_TEST_READY_MARKER;
      if (marker) {
        const { writeFile } = await import("node:fs/promises");
        await writeFile(marker, "ready");
      }
      // Hold the event loop open like a real browser wait (goto 60s,
      // waitFor 30s): a bare pending promise would let Node drain and exit
      // 0 even with the old swallow-the-signal handlers, hiding the hang.
      setInterval(() => {}, 1000);
      await new Promise(() => {});
    },
  }),
};
"#,
    )
    .unwrap();

    let out_dir = temp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mock_file = temp.path().join("mock.png");
    std::fs::write(&mock_file, [0x89, 0x50, 0x4e, 0x47]).unwrap();

    let setup_script = "const fs = require('node:fs'); \
        fs.writeFileSync(process.argv[1], String(process.pid)); \
        console.log('ORBIT_VISUAL_URL=http://127.0.0.1:9/'); \
        setInterval(() => {}, 1000);";
    let request = serde_json::json!({
        "repo_root": temp.path().display().to_string(),
        "out_dir": out_dir.display().to_string(),
        "kit_npm_project": kit_dir.display().to_string(),
        "captures_per_mock": 1,
        "frozen_instant": "2026-01-01T12:00:00.000Z",
        "rules_dir": null,
        "mocks": [
            {
                "artifact": "mocks/a.png",
                "name": "a",
                "mock_file": mock_file.display().to_string(),
                "mock_ext": "png",
                "view": "tasks-list",
                "state": "default",
                "theme": "dark",
                "width": 1440,
                "view_def": {
                    "id": "tasks-list",
                    "app": "orbit-web",
                    "title": "tasks",
                    "route": "/",
                    "fixture": "tasks",
                    "ready": "body",
                    "responsive": true,
                    "states": ["default"]
                },
                "app_def": {
                    "name": "orbit-web",
                    "setup": ["node", "-e", setup_script, pid_file.display().to_string()],
                    "setup_timeout_s": 10,
                    "themes": [],
                    "color_scheme_invariant": false
                }
            }
        ],
    });
    let request_path = temp.path().join("request.json");
    std::fs::write(&request_path, serde_json::to_string(&request).unwrap()).unwrap();

    let mut child = Command::new("node")
        .arg(DRIVER)
        .arg("--request")
        .arg(&request_path)
        .env("PLAYWRIGHT_MODULE", &stub)
        .env("DRIVER_TEST_READY_MARKER", &marker)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("node must be on PATH to run the capture driver: {error}"));

    let mut wait_for = |path: &std::path::Path, what: &str| {
        let start = Instant::now();
        let limit = Duration::from_secs(30);
        while !path.exists() {
            if start.elapsed() >= limit {
                cleanup_child(&mut child);
                panic!("capture driver did not reach {what} within {limit:?} (ORB-20090)");
            }
            if let Ok(Some(status)) = child.try_wait() {
                let stderr = child.stderr.take().map(|mut pipe| {
                    use std::io::Read as _;
                    let mut text = String::new();
                    let _ = pipe.read_to_string(&mut text);
                    text
                });
                panic!(
                    "capture driver exited {status:?} before reaching {what}; stderr: {}",
                    stderr.unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    wait_for(&marker, "in-flight capture");
    wait_for(&pid_file, "fixture pid");

    let pid_text = std::fs::read_to_string(&pid_file).unwrap();
    let fixture_pid: i32 = pid_text.trim().parse().unwrap();
    // The fixture must be alive when the signal lands; otherwise the test
    // would not prove cleanup ran after the signal.
    assert_eq!(
        unsafe { libc::kill(fixture_pid, 0) },
        0,
        "fixture server {fixture_pid} must be alive before {signal_name} (ORB-20090)"
    );

    let before_signal = Instant::now();
    let sent = unsafe { libc::kill(child.id() as libc::pid_t, signal) };
    assert_eq!(
        sent, 0,
        "failed to send {signal_name} to the capture driver"
    );

    let deadline = Duration::from_secs(15);
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        if before_signal.elapsed() >= deadline {
            let fixture_status = unsafe { libc::kill(fixture_pid, 0) };
            cleanup_child(&mut child);
            unsafe {
                libc::kill(-fixture_pid, libc::SIGKILL);
                libc::kill(fixture_pid, libc::SIGKILL);
            }
            panic!(
                "capture driver did not exit within {deadline:?} of {signal_name} \
                 (fixture alive: {fixture_status}); it continued the capture loop \
                 instead of terminating after fixture cleanup (ORB-20090)"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let shutdown_duration = before_signal.elapsed();
    assert!(
        !status.success(),
        "signalled driver must not exit 0 with a normal capture report \
         after {shutdown_duration:?} (ORB-20090)"
    );
    assert_eq!(
        status.code(),
        Some(expected_code),
        "signalled driver must exit {expected_code} for {signal_name}, got {status:?} \
         after {shutdown_duration:?} (ORB-20090)"
    );

    // Fixture cleanup is part of the prompt exit: the server the driver
    // started must be gone once the driver is gone.
    let start = Instant::now();
    let gone = loop {
        if unsafe { libc::kill(fixture_pid, 0) } != 0 {
            break true;
        }
        if start.elapsed() >= Duration::from_secs(5) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !gone {
        unsafe {
            libc::kill(-fixture_pid, libc::SIGKILL);
            libc::kill(fixture_pid, libc::SIGKILL);
        }
        let _ = child.wait();
        panic!(
            "fixture server {fixture_pid} remained alive after signalled driver exit (ORB-20090)"
        );
    }
    let _ = child.wait();
}

#[cfg(unix)]
fn cleanup_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
