//! The host-side GitHub run-log cache keeps no secret the response redactor
//! removes (ORB-25900).
//!
//! It persisted each completed log as GitHub served it, outside the agent's
//! bounded, redacted projection, and kept doing so in a directory no agent
//! sandbox masked. The registered `github.run.logs` tool runs here against a
//! stand-in `gh` in an isolated child, with its own `ORBIT_ROOT`.

use std::path::Path;

use orbit_tools::{ToolContext, ToolRegistry};
use serde_json::json;
use tempfile::TempDir;

const MARKER: &str = "ORBIT_TEST_GITHUB_LOG_CACHE_CHILD";
const TEST: &str = "github_log_cache::a_cached_log_keeps_no_secret_the_redactor_removes";

const GH_STUB: &str = r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/calls"
case "$*" in
  *actions/jobs/9*) printf '{"id":9,"run_id":123,"status":"completed"}\n'; exit 0 ;;
  *"run view"*) /bin/cat "$dir/log"; exit 0 ;;
esac
exit 1
"#;

fn secret() -> String {
    format!("ghp_{}", "Q7".repeat(18))
}

/// Every byte of every file under `dir`, recursively, as lossy text.
fn persisted_text(dir: &Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            text.push_str(&persisted_text(&path));
        } else {
            text.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
        }
    }
    text
}

#[test]
fn a_cached_log_keeps_no_secret_the_redactor_removes() {
    if std::env::var_os(MARKER).is_none() {
        run_isolated();
        return;
    }
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let root = home.join("orbit-root");
    let secret = secret();
    let mut log: String = (1..=40)
        .map(|line| format!("build\tTest\t2026-11-20T02:00:00.0000000Z line {line}\n"))
        .collect();
    log.push_str(&format!(
        "build\tTest\t2026-11-20T02:00:00.0000000Z token {secret} leaked\n"
    ));
    std::fs::write(home.join("bin/log"), &log).unwrap();
    // What an earlier release left: a raw entry and its lock.
    let legacy = root.join("cache/github-run-logs");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("0123.log"), &log).unwrap();
    std::fs::write(root.join("cache/github-run-logs.lock"), "").unwrap();

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let read = || {
        registry
            .execute(
                "github.run.logs",
                &ToolContext::default(),
                json!({"run": "123", "job": "9", "scope": "all", "repo": "acme/orbit"}),
            )
            .unwrap()
    };
    let cold = read();
    let warm = read();

    let calls = std::fs::read_to_string(home.join("bin/calls")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|call| call.contains("run view"))
            .count(),
        1,
        "the completed log is downloaded once and then served from the cache: {calls}"
    );
    for output in [&cold, &warm] {
        let text = output["log"].as_str().unwrap();
        assert!(
            !text.contains(&secret),
            "the response leaks the secret: {text}"
        );
        assert!(
            text.contains("leaked"),
            "the secret's line is returned: {text}"
        );
        assert_eq!(
            output["total_bytes"].as_u64().unwrap(),
            log.len() as u64,
            "coordinates stay those of the source log"
        );
    }
    assert_eq!(cold, warm, "a warm read matches the cold one");

    let cache = root.join(orbit_tools::GITHUB_RUN_LOG_CACHE_DIR);
    assert!(
        std::fs::read_dir(&cache).unwrap().next().is_some(),
        "the completed log is cached"
    );
    assert!(
        !persisted_text(&root).contains(&secret),
        "the cache persisted the secret"
    );
    assert!(!legacy.exists(), "the unredacted legacy cache is removed");
    assert!(!root.join("cache/github-run-logs.lock").exists());
}

fn run_isolated() {
    let home = TempDir::new().unwrap();
    let bin = home.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let program = bin.join("gh");
    std::fs::write(&program, GH_STUB).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(MARKER, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("ORBIT_ROOT", home.path().join("orbit-root"))
        .env("PATH", path)
        .current_dir(home.path());
    let output = orbit_common::test_env::run_child_test(&mut command, TEST, home.path());
    orbit_common::test_env::assert_child_test_passed(
        TEST,
        output.status,
        output.stdout,
        output.stderr,
    );
}
