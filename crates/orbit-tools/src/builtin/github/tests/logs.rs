use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::log_cache::LogCache;
use super::super::logs::{LogReadBounds, RunLogRequests, read_run_log};

/// A fake `gh` that records every invocation and answers from files beside it.
struct FakeGh {
    dir: TempDir,
}

impl FakeGh {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let program = dir.path().join("gh");
        std::fs::write(
            &program,
            "#!/bin/sh\n\
             [ \"$1\" = --warm-up ] && exit 0\n\
             dir=$(dirname \"$0\")\n\
             printf '%s\\n' \"$*\" >> \"$dir/calls\"\n\
             [ -e \"$dir/denied\" ] && { echo 'HTTP 404: Not Found' >&2; exit 1; }\n\
             case \"$*\" in\n\
               *actions/jobs/*) cat \"$dir/job.json\" ;;\n\
               *actions/runs/*) cat \"$dir/run.json\" ;;\n\
               \"run view\"*) cat \"$dir/log\" ;;\n\
               *) exit 1 ;;\n\
             esac\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        // Another test thread forking while the script was open for writing
        // leaves its descriptor in that child until it execs, and exec of a
        // file open for writing fails with ETXTBSY. Wait that out once.
        let mut attempts = 0;
        while let Err(error) = std::process::Command::new(&program)
            .arg("--warm-up")
            .status()
        {
            attempts += 1;
            assert!(attempts < 200, "fake gh never became executable: {error}");
            std::thread::sleep(Duration::from_millis(10));
        }
        let gh = Self { dir };
        gh.set_log(&numbered_log("first"));
        gh.set_job("completed");
        gh.set_run("completed", 1);
        gh
    }

    fn program(&self) -> String {
        self.dir.path().join("gh").to_string_lossy().into_owned()
    }

    fn set_log(&self, text: &str) {
        std::fs::write(self.dir.path().join("log"), text).unwrap();
    }

    /// The host and repository GitHub resolves for every status answer.
    fn source(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("source"))
            .unwrap_or_else(|_| "https://api.github.com/repos/acme/orbit".to_string())
    }

    /// Point every later status answer at another repository, as a checkout
    /// change does for a request that names `{owner}/{repo}`.
    fn set_source(&self, source: &str) {
        std::fs::write(self.dir.path().join("source"), source).unwrap();
        let (job, run) = (self.job_status(), self.run_status());
        self.set_job(&job);
        self.set_run(&run.0, run.1);
    }

    /// Refuse every later call, as GitHub does a caller that lost access.
    fn deny(&self) {
        std::fs::write(self.dir.path().join("denied"), "").unwrap();
    }

    fn job_status(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("job.json"))
            .ok()
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .and_then(|view| view["status"].as_str().map(ToString::to_string))
            .unwrap_or_else(|| "completed".to_string())
    }

    fn run_status(&self) -> (String, u64) {
        std::fs::read_to_string(self.dir.path().join("run.json"))
            .ok()
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .map(|view| {
                (
                    view["status"].as_str().unwrap_or("completed").to_string(),
                    view["run_attempt"].as_u64().unwrap_or(1),
                )
            })
            .unwrap_or_else(|| ("completed".to_string(), 1))
    }

    fn set_job(&self, status: &str) {
        let url = format!("{}/actions/jobs/9", self.source());
        let body = json!({"id": 9, "run_id": 123, "status": status, "url": url});
        std::fs::write(self.dir.path().join("job.json"), body.to_string()).unwrap();
    }

    fn set_run(&self, status: &str, attempt: u64) {
        let url = format!("{}/actions/runs/123", self.source());
        let body = json!({"id": 123, "status": status, "run_attempt": attempt, "url": url});
        std::fs::write(self.dir.path().join("run.json"), body.to_string()).unwrap();
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(ToString::to_string)
            .collect()
    }

    fn downloads(&self) -> usize {
        self.calls()
            .iter()
            .filter(|call| call.starts_with("run view"))
            .count()
    }

    fn requests(&self, mut input: Value, cache: Option<&Path>) -> RunLogRequests {
        input["run"] = json!("123");
        let mut requests = RunLogRequests::from_input(&input).unwrap();
        requests.run_log.program = self.program();
        requests.run_view.program = self.program();
        requests.with_log_cache(
            cache.map(|dir| LogCache::new(dir.to_path_buf(), 1 << 20, Duration::from_secs(3600))),
        )
    }
}

fn numbered_log(tag: &str) -> String {
    (1..=40)
        .map(|line| format!("build\tTest\t2026-11-20T02:00:00.0000000Z {tag} line {line}\n"))
        .collect()
}

/// Everything a caller can observe of one read.
fn rendered(requests: &RunLogRequests, max_bytes: usize) -> Value {
    match read_run_log(requests, LogReadBounds::new(max_bytes), None) {
        Ok(read) => json!({
            "text": read.log.text,
            "truncated": read.log.truncated,
            "total_bytes": read.log.total_bytes,
            "returned_bytes": read.log.returned_bytes,
            "source_complete": read.log.source_complete,
            "diagnostic": read.log.diagnostic,
            "failure_regions": read.log.failure_regions,
            "checkout_commits": read.log.checkout_evidence.commits,
            "checkout_evidence": read.log.checkout_evidence.lines,
            "selected_bytes": read.log.selected_bytes,
            "selected_lines": read.log.selected_lines,
            "source": read.source,
            "source_jobs": read.source_jobs,
            "fallback_error": read.fallback_error,
        }),
        Err(error) => json!({"error": error.to_string()}),
    }
}

fn entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, Iterator::count)
}

fn cache_dir(root: &TempDir) -> PathBuf {
    root.path().join("cache")
}

#[test]
fn two_windows_of_a_completed_job_download_its_log_once() {
    let gh = FakeGh::new();
    let root = TempDir::new().unwrap();
    let cache = cache_dir(&root);
    let first = json!({"job": "9", "byte_start": 0, "byte_end": 60});
    let second = json!({"job": "9", "line_start": 5, "line_end": 8});

    let a = rendered(&gh.requests(first, Some(&cache)), 4096);
    let calls_after_first = gh.calls();
    let b = rendered(&gh.requests(second.clone(), Some(&cache)), 4096);

    assert_eq!(
        gh.downloads(),
        1,
        "the log is downloaded once: {:?}",
        gh.calls()
    );
    assert_eq!(
        gh.calls()[calls_after_first.len()..],
        ["api --method GET repos/{owner}/{repo}/actions/jobs/9".to_string()],
        "a cache hit makes only the status request that proves current access"
    );
    assert!(a["text"].as_str().unwrap().contains("first line 1"));
    assert!(b["text"].as_str().unwrap().contains("first line 5"));
    // The warm window renders what an uncached read of it renders.
    assert_eq!(b, rendered(&gh.requests(second, None), 4096));
}

#[test]
fn a_job_that_is_not_completed_is_neither_served_from_nor_written_to_the_cache() {
    let gh = FakeGh::new();
    gh.set_job("in_progress");
    let root = TempDir::new().unwrap();
    let cache = cache_dir(&root);
    let input = json!({"job": "9"});

    rendered(&gh.requests(input.clone(), Some(&cache)), 4096);
    rendered(&gh.requests(input, Some(&cache)), 4096);

    assert_eq!(gh.downloads(), 2);
    assert_eq!(entries(&cache), 0);
}

#[test]
fn a_completed_run_downloads_once_per_attempt_and_asks_its_status_each_read() {
    let gh = FakeGh::new();
    let root = TempDir::new().unwrap();
    let cache = cache_dir(&root);
    let status_reads = |gh: &FakeGh| {
        gh.calls()
            .iter()
            .filter(|call| call.contains("actions/runs/123"))
            .count()
    };

    rendered(
        &gh.requests(json!({"byte_start": 0, "byte_end": 60}), Some(&cache)),
        4096,
    );
    let second = rendered(
        &gh.requests(json!({"line_start": 5, "line_end": 8}), Some(&cache)),
        4096,
    );
    assert_eq!(gh.downloads(), 1);
    assert_eq!(
        status_reads(&gh),
        2,
        "each read asks for status and attempt"
    );
    assert!(second["text"].as_str().unwrap().contains("first line 5"));

    // A rerun: the same run id, now at attempt 2, with a different log.
    gh.set_run("completed", 2);
    gh.set_log(&numbered_log("rerun"));
    let third = rendered(
        &gh.requests(json!({"line_start": 5, "line_end": 8}), Some(&cache)),
        4096,
    );
    assert_eq!(gh.downloads(), 2, "a new attempt downloads its own log");
    assert!(third["text"].as_str().unwrap().contains("rerun line 5"));
    assert_eq!(entries(&cache), 2);
}

#[test]
fn a_run_put_back_in_progress_is_never_served_from_the_cache() {
    let gh = FakeGh::new();
    let root = TempDir::new().unwrap();
    let cache = cache_dir(&root);
    let input = json!({"scope": "all"});
    rendered(&gh.requests(input.clone(), Some(&cache)), 4096);
    assert_eq!(entries(&cache), 1);

    gh.set_run("in_progress", 2);
    gh.set_log(&numbered_log("partial"));
    let read = rendered(&gh.requests(input, Some(&cache)), 4096);

    assert_eq!(gh.downloads(), 2);
    assert!(read["text"].as_str().unwrap().contains("partial line 1"));
    assert_eq!(entries(&cache), 1, "the in-progress log is not written");
}

#[test]
fn every_input_shape_renders_identically_cold_warm_and_uncached() {
    let gh = FakeGh::new();
    let shapes = [
        (json!({}), 4096),
        (json!({"scope": "all"}), 4096),
        (json!({"scope": "all"}), 200),
        (json!({"step": "Test"}), 4096),
        (json!({"job": "9", "step": "Test", "scope": "all"}), 4096),
        (json!({"job": "9", "byte_start": 10, "byte_end": 300}), 4096),
        (json!({"byte_start": 10, "byte_end": 700}), 128),
        (json!({"job": "9", "line_start": 3, "line_end": 9}), 4096),
        (json!({"line_start": 30}), 64),
        (json!({"step": "Absent"}), 4096),
    ];
    for (input, max_bytes) in shapes {
        let root = TempDir::new().unwrap();
        let cache = cache_dir(&root);
        let uncached = rendered(&gh.requests(input.clone(), None), max_bytes);
        let cold = rendered(&gh.requests(input.clone(), Some(&cache)), max_bytes);
        let warm = rendered(&gh.requests(input.clone(), Some(&cache)), max_bytes);
        assert_eq!(cold, uncached, "cold cache: {input} max_bytes={max_bytes}");
        assert_eq!(warm, uncached, "warm cache: {input} max_bytes={max_bytes}");
    }
}

/// ORB-25901: a cached log is served only to a caller whose own status request
/// succeeds today, and only for the repository that request resolved.
#[test]
fn a_cached_log_is_not_served_once_access_is_refused() {
    for input in [json!({"job": "9"}), json!({"scope": "all"})] {
        let gh = FakeGh::new();
        let root = TempDir::new().unwrap();
        let cache = cache_dir(&root);
        let warm = rendered(&gh.requests(input.clone(), Some(&cache)), 4096);
        assert!(warm["text"].as_str().unwrap().contains("first line 1"));
        assert_eq!(entries(&cache), 1, "{input}");

        gh.deny();
        let refused = rendered(&gh.requests(input.clone(), Some(&cache)), 4096);

        assert!(
            refused["error"].is_string(),
            "{input}: a provider refusal stays an error: {refused}"
        );
        assert_eq!(
            gh.downloads(),
            2,
            "{input}: the refused read still asks the provider"
        );
    }
}

#[test]
fn a_cached_log_is_not_served_for_another_resolved_repository() {
    for input in [json!({"job": "9"}), json!({"scope": "all"})] {
        let gh = FakeGh::new();
        let root = TempDir::new().unwrap();
        let cache = cache_dir(&root);
        // The same `{owner}/{repo}` request, before and after the checkout it
        // runs in moves to another repository that reuses the ids.
        let first = rendered(&gh.requests(input.clone(), Some(&cache)), 4096);
        assert!(first["text"].as_str().unwrap().contains("first line 1"));

        gh.set_source("https://api.github.com/repos/other/repo");
        gh.set_log(&numbered_log("other"));
        let second = rendered(&gh.requests(input.clone(), Some(&cache)), 4096);

        assert!(
            second["text"].as_str().unwrap().contains("other line 1"),
            "{input}: another repository's read never returns the first one's log"
        );
        assert_eq!(gh.downloads(), 2, "{input}");
        assert_eq!(entries(&cache), 2, "{input}");
    }
}
