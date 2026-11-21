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
const RERUN_MARKER: &str = "ORBIT_TEST_GITHUB_LOG_CACHE_RERUN_CHILD";
const RERUN_TEST: &str =
    "github_log_cache::a_rerun_between_metadata_and_download_never_crosses_attempt_keys";

const GH_STUB: &str = r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/calls"
case "$*" in
  *actions/jobs/9*) printf '{"id":9,"run_id":123,"status":"completed","url":"https://api.github.com/repos/acme/orbit/actions/jobs/9"}\n'; exit 0 ;;
  *"run view"*) /bin/cat "$dir/log"; exit 0 ;;
esac
exit 1
"#;

const GH_RERUN_STUB: &str = r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/calls"
case "$*" in
  *actions/runs/123*)
    attempt=$(/bin/cat "$dir/attempt_123")
    printf '{"id":123,"status":"completed","run_attempt":%s,"url":"https://api.github.com/repos/acme/orbit/actions/runs/123"}\n' "$attempt"
    if [ -f "$dir/advance_123" ]; then
      /bin/rm -f "$dir/advance_123"
      printf '2' > "$dir/attempt_123"
    fi
    exit 0 ;;
  *actions/runs/124*)
    attempt=$(/bin/cat "$dir/attempt_124")
    printf '{"id":124,"status":"completed","run_attempt":%s,"url":"https://api.github.com/repos/acme/orbit/actions/runs/124"}\n' "$attempt"
    if [ -f "$dir/advance_124" ]; then
      /bin/rm -f "$dir/advance_124"
      printf '2' > "$dir/attempt_124"
    fi
    exit 0 ;;
  *"run view 123"*)
    attempt=$(/bin/cat "$dir/attempt_123")
    /bin/cat "$dir/log_123_attempt_${attempt}"
    exit 0 ;;
  *"run view 124"*)
    case "$*" in
      *"--attempt 1"*) /bin/cat "$dir/log_124_attempt_1" ;;
      *)
        attempt=$(/bin/cat "$dir/attempt_124")
        /bin/cat "$dir/log_124_attempt_${attempt}" ;;
    esac
    exit 0 ;;
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
        run_isolated(MARKER, TEST, GH_STUB);
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

fn sample_attempt_log(run: &str, attempt: u64) -> String {
    (1..=40)
        .map(|line| {
            format!("build\tTest\t2026-11-20T02:00:00.0000000Z run {run} attempt {attempt} line {line}\n")
        })
        .collect()
}

fn cache_entry_count(cache_dir: &Path) -> usize {
    std::fs::read_dir(cache_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .count()
}

#[test]
fn a_rerun_between_metadata_and_download_never_crosses_attempt_keys() {
    if std::env::var_os(RERUN_MARKER).is_none() {
        run_isolated(RERUN_MARKER, RERUN_TEST, GH_RERUN_STUB);
        return;
    }
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let bin = home.join("bin");
    let root = home.join("orbit-root");
    let cache = root.join(orbit_tools::GITHUB_RUN_LOG_CACHE_DIR);

    for run in ["123", "124"] {
        std::fs::write(bin.join(format!("attempt_{run}")), "1").unwrap();
        std::fs::write(bin.join(format!("advance_{run}")), "").unwrap();
        std::fs::write(
            bin.join(format!("log_{run}_attempt_1")),
            sample_attempt_log(run, 1),
        )
        .unwrap();
        std::fs::write(
            bin.join(format!("log_{run}_attempt_2")),
            sample_attempt_log(run, 2),
        )
        .unwrap();
    }

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let read_run = |run: &str| {
        registry
            .execute(
                "github.run.logs",
                &ToolContext::default(),
                json!({"run": run, "scope": "all", "repo": "acme/orbit"}),
            )
            .unwrap()
    };
    let run_view_calls = |run: &str| -> Vec<String> {
        std::fs::read_to_string(bin.join("calls"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with(&format!("run view {run} ")))
            .map(ToString::to_string)
            .collect()
    };

    // Run 123: metadata observes attempt 1, then a rerun advances to attempt 2
    // before download; even if `gh` ignores `--attempt` and returns attempt 2
    // bytes, post-download verification sees attempt 2 != 1 and stores nothing
    // under attempt 1's key.
    let _raced = read_run("123");
    assert_eq!(
        cache_entry_count(&cache),
        0,
        "raced read must not persist attempt 2 bytes under attempt 1's key"
    );
    let calls_123 = run_view_calls("123");
    assert_eq!(calls_123.len(), 1);
    assert!(
        calls_123[0].contains("--attempt 1"),
        "download must bind to the observed attempt: {calls_123:?}"
    );

    // An overlapping caller still observing attempt 1 must miss (never served
    // attempt 2's bytes from cache) and populate attempt 1's key with attempt 1.
    std::fs::write(bin.join("attempt_123"), "1").unwrap();
    let attempt_1 = read_run("123");
    let text_1 = attempt_1["log"].as_str().unwrap();
    assert!(text_1.contains("run 123 attempt 1 line 1"), "{text_1}");
    assert!(!text_1.contains("attempt 2"), "{text_1}");
    assert_eq!(cache_entry_count(&cache), 1);

    // Once the rerun completes at attempt 2, it misses attempt 1's cache entry,
    // downloads attempt 2 once, and caches it separately.
    std::fs::write(bin.join("attempt_123"), "2").unwrap();
    let cold_2 = read_run("123");
    let warm_2 = read_run("123");
    for output in [&cold_2, &warm_2] {
        let text = output["log"].as_str().unwrap();
        assert!(text.contains("run 123 attempt 2 line 1"), "{text}");
        assert!(!text.contains("attempt 1"), "{text}");
    }
    assert_eq!(cold_2, warm_2);
    assert_eq!(cache_entry_count(&cache), 2);
    assert_eq!(
        run_view_calls("123").len(),
        3,
        "warm attempt 2 read hits cache without another download: {:?}",
        run_view_calls("123")
    );

    // Run 124: metadata observes attempt 1, rerun advances to attempt 2 before
    // download, and `gh` honors `--attempt 1`: the read returns attempt 1's
    // bytes, never attempt 2's, and never crosses attempt keys.
    let raced_bound = read_run("124");
    let bound_text = raced_bound["log"].as_str().unwrap();
    assert!(
        bound_text.contains("run 124 attempt 1 line 1"),
        "{bound_text}"
    );
    assert!(!bound_text.contains("attempt 2"), "{bound_text}");
    assert_eq!(
        cache_entry_count(&cache),
        2,
        "raced run 124 must not write a cache entry after attempt advanced"
    );

    let run_124_cold_2 = read_run("124");
    let run_124_warm_2 = read_run("124");
    for output in [&run_124_cold_2, &run_124_warm_2] {
        let text = output["log"].as_str().unwrap();
        assert!(text.contains("run 124 attempt 2 line 1"), "{text}");
        assert!(!text.contains("attempt 1"), "{text}");
    }
    assert_eq!(run_124_cold_2, run_124_warm_2);
    assert_eq!(cache_entry_count(&cache), 3);
    let calls_124 = run_view_calls("124");
    assert_eq!(calls_124.len(), 2, "{calls_124:?}");
    assert!(calls_124[0].contains("--attempt 1"), "{calls_124:?}");
    assert!(calls_124[1].contains("--attempt 2"), "{calls_124:?}");
}

fn run_isolated(marker: &str, test_name: &str, gh_stub: &str) {
    let home = TempDir::new().unwrap();
    let bin = home.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let program = bin.join("gh");
    std::fs::write(&program, gh_stub).unwrap();
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
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(marker, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("ORBIT_ROOT", home.path().join("orbit-root"))
        .env("PATH", path)
        .current_dir(home.path());
    let output = orbit_common::test_env::run_child_test(&mut command, test_name, home.path());
    orbit_common::test_env::assert_child_test_passed(
        test_name,
        output.status,
        output.stdout,
        output.stderr,
    );
}
