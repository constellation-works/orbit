//! The per-job log fallback, exercised against a scripted `gh` executable.
//!
//! `gh run view --log-failed` exiting 0 with no output is a live GitHub CLI
//! blind spot, not proof that a run passed or that its logs expired. These
//! tests script that exact sequence — empty success, then a job log API read —
//! and pin what the fallback may and may not conclude from it.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::builtin::github::logs::{LogReadBounds, RunLogRead, RunLogRequests, read_run_log};

const RUN_ID: &str = "34060485218";
const FAILED_JOB_ID: u64 = 101560010340;
const PASSING_JOB_ID: u64 = 101560019999;
const CHECKOUT_SHA: &str = "3d9fc7c65934cdc98cec3954a37e10ba6d387e55";

/// One job's log as the log API serves it: timestamped runner lines with no
/// job/step columns, opening on provisioning and closing on the failure.
fn job_log() -> String {
    format!(
        "\u{feff}2026-09-06T21:15:07.8469063Z Current runner version: '2.337.0'\n\
         2026-09-06T21:15:34.9569214Z [command]/usr/bin/git log -1 --format=%H\n\
         2026-09-06T21:15:34.9602377Z {CHECKOUT_SHA}\n\
         2026-09-06T21:28:05.5540516Z  Documenting orbit-web v0.19.0\n\
         2026-09-06T21:28:07.0459354Z error: public documentation for `connect` links to private item `reject_root_override`\n\
         2026-09-06T21:28:07.0460831Z   --> crates/orbit-web/src/connect.rs:92:7\n\
         2026-09-06T21:28:07.4229928Z ##[error]Process completed with exit code 101.\n"
    )
}

/// A `gh run view --json` payload with one failed job and one that passed.
fn run_view(run_url: &str) -> Value {
    json!({
        "databaseId": RUN_ID.parse::<u64>().expect("numeric run id"),
        "number": 12,
        "workflowName": "CI",
        "displayTitle": "CI on agent-main",
        "status": "completed",
        "conclusion": "failure",
        "event": "push",
        "headBranch": "agent-main",
        "headSha": "1111111111111111111111111111111111111111",
        "createdAt": "2026-09-06T21:15:00Z",
        "url": run_url,
        "jobs": [
            {
                "databaseId": PASSING_JOB_ID,
                "name": "test (ubuntu)",
                "status": "completed",
                "conclusion": "success",
                "steps": [{"number": 1, "name": "cargo test", "status": "completed", "conclusion": "success"}],
            },
            {
                "databaseId": FAILED_JOB_ID,
                "name": "docs",
                "status": "completed",
                "conclusion": "failure",
                "steps": [{"number": 5, "name": "cargo doc", "status": "completed", "conclusion": "failure"}],
            },
        ],
    })
}

/// A scripted `gh`: a shell dispatcher over argv, plus the argv log every test
/// reads back to prove which queries actually ran.
struct FakeGh {
    dir: TempDir,
    program: String,
}

impl FakeGh {
    /// `job_logs` maps a job id to what its log endpoint serves; a job absent
    /// from the map answers with a 404 the way `gh api` does.
    fn new(view: &Value, job_logs: &[(u64, &str)]) -> Self {
        let dir = TempDir::new().expect("temp dir");
        let path = |name: &str| dir.path().join(name).display().to_string();
        fs::write(dir.path().join("run_view.json"), view.to_string()).expect("write run view");

        let mut cases = String::new();
        for (job_id, log) in job_logs {
            let file = format!("job_{job_id}.log");
            fs::write(dir.path().join(&file), log).expect("write job log");
            cases.push_str(&format!(
                "  *\"actions/jobs/{job_id}/logs\"*) cat {} ; exit 0 ;;\n",
                path(&file)
            ));
        }

        let script = format!(
            "#!/usr/bin/env bash\n\
             case \"$*\" in --warmup) exit 0 ;; esac\n\
             printf '%s\\n' \"$*\" >> {calls}\n\
             case \"$*\" in\n\
             {cases}  *\"--log-failed\"*|*\" --log\"*) exit 0 ;;\n  \
             *\"--json\"*) cat {view} ; exit 0 ;;\n  \
             *\"actions/jobs/\"*) printf 'gh: Not Found (HTTP 404) token=ghp_{token}\\n' >&2 ; exit 1 ;;\n\
             esac\n\
             printf 'unscripted gh call: %s\\n' \"$*\" >&2\n\
             exit 1\n",
            calls = path("calls.txt"),
            view = path("run_view.json"),
            token = "a".repeat(36),
        );
        let program = dir.path().join("gh");
        fs::write(&program, script).expect("write fake gh");
        set_executable(&program);
        wait_until_executable(&program);

        Self {
            program: program.display().to_string(),
            dir,
        }
    }

    /// Requests pointed at the scripted CLI. Everything else — argv, timeouts,
    /// bounds — is what production builds.
    fn requests(&self, input: Value) -> RunLogRequests {
        let mut requests = RunLogRequests::from_input(&input).expect("requests");
        requests.run_log.program.clone_from(&self.program);
        requests.run_view.program.clone_from(&self.program);
        requests
    }

    fn read(&self, input: Value, max_bytes: usize) -> RunLogRead {
        read_run_log(&self.requests(input), LogReadBounds::new(max_bytes)).expect("log read")
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("calls.txt"))
            .unwrap_or_default()
            .lines()
            .map(ToOwned::to_owned)
            .collect()
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod fake gh");
}

/// Run the freshly written script once before any read uses it.
///
/// A parallel test's child can inherit this file's writable descriptor across
/// `fork`, and Linux rejects an exec while that descriptor is open. The
/// descriptor is close-on-exec, so the window is short — but it has to close
/// before the read under test runs, or a transient spawn failure would be
/// mistaken for the fallback finding no evidence.
fn wait_until_executable(program: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match std::process::Command::new(program).arg("--warmup").status() {
            Ok(status) if status.success() => return,
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && std::time::Instant::now() < deadline => {}
            other => panic!("fake gh must be executable: {other:?}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn failed_scope(run: &str) -> Value {
    json!({"run": run})
}

fn run_url(run_id: &str) -> String {
    format!("https://github.com/acme/orbit/actions/runs/{run_id}")
}

fn assert_read_the_failed_job(calls: &[String]) {
    assert!(
        calls
            .iter()
            .any(|call| call.contains(&format!("actions/jobs/{FAILED_JOB_ID}/logs"))),
        "the failed job's log endpoint should have been read: {calls:?}"
    );
}

fn assert_read_no_job_log(calls: &[String]) {
    assert!(
        !calls.iter().any(|call| call.contains("actions/jobs/")),
        "no job log may be read: {calls:?}"
    );
}

#[test]
fn an_empty_failed_step_read_recovers_the_failed_jobs_own_log() {
    let log = job_log();
    let gh = FakeGh::new(
        &run_view(&run_url(RUN_ID)),
        &[(FAILED_JOB_ID, log.as_str())],
    );

    let read = gh.read(failed_scope(RUN_ID), 16_384);

    assert_eq!(read.source, "job_api_log");
    assert_eq!(read.fallback_error, None);
    assert_eq!(
        read.source_jobs,
        vec![json!({
            "job_id": FAILED_JOB_ID,
            "name": "docs",
            "conclusion": "failure",
            "url": format!("{}/job/{FAILED_JOB_ID}", run_url(RUN_ID)),
        })]
    );
    assert!(
        read.log
            .text
            .contains("links to private item `reject_root_override`"),
        "the failing diagnostic must survive into the excerpt: {}",
        read.log.text
    );
    assert!(read.log.text.contains("crates/orbit-web/src/connect.rs:92"));
    // Identity comes from the runner's own output, never from the run's
    // reported head SHA.
    assert_eq!(read.log.checkout_evidence.commits, vec![CHECKOUT_SHA]);
    assert!(read.log.checkout_evidence.complete);
    assert_read_the_failed_job(&gh.calls());
}

#[test]
fn a_successful_job_is_never_read_as_failed_step_evidence() {
    let view = json!({
        "databaseId": RUN_ID.parse::<u64>().expect("numeric run id"),
        "url": run_url(RUN_ID),
        "status": "completed",
        "conclusion": "failure",
        "jobs": [{
            "databaseId": PASSING_JOB_ID,
            "name": "test (ubuntu)",
            "status": "completed",
            "conclusion": "success",
            "steps": [],
        }],
    });
    let gh = FakeGh::new(&view, &[(PASSING_JOB_ID, "passing job output\n")]);

    let read = gh.read(failed_scope(RUN_ID), 16_384);

    assert_eq!(read.source, "run_log");
    assert!(read.log.text.trim().is_empty());
    assert!(read.source_jobs.is_empty());
    let reason = read.fallback_error.expect("fallback reports why");
    assert!(
        reason.contains("reported no failed job"),
        "unexpected reason: {reason}"
    );
    assert_read_no_job_log(&gh.calls());
}

#[test]
fn job_metadata_naming_another_run_is_refused() {
    let mut view = run_view(&run_url(RUN_ID));
    view["databaseId"] = json!(99);
    let gh = FakeGh::new(&view, &[(FAILED_JOB_ID, job_log().as_str())]);

    let read = gh.read(failed_scope(RUN_ID), 16_384);

    let reason = read.fallback_error.expect("fallback reports why");
    assert!(
        reason.contains("reported run 99, not run 34060485218"),
        "unexpected reason: {reason}"
    );
    assert!(read.log.text.trim().is_empty());
    assert!(read.log.checkout_evidence.commits.is_empty());
    assert_read_no_job_log(&gh.calls());
}

#[test]
fn a_job_url_from_another_run_is_refused() {
    // Same run id, but every job URL belongs to another run: metadata this
    // read cannot vouch for, however well-formed it looks.
    let gh = FakeGh::new(
        &run_view(&run_url("77777777")),
        &[(FAILED_JOB_ID, job_log().as_str())],
    );

    let read = gh.read(failed_scope(RUN_ID), 16_384);

    let reason = read.fallback_error.expect("fallback reports why");
    assert!(
        reason.contains("reported no failed job"),
        "unexpected reason: {reason}"
    );
    assert!(read.log.text.trim().is_empty());
    assert_read_no_job_log(&gh.calls());
}

#[test]
fn an_unavailable_job_log_is_reported_rather_than_fabricated() {
    let gh = FakeGh::new(&run_view(&run_url(RUN_ID)), &[]);

    let read = gh.read(failed_scope(RUN_ID), 16_384);

    assert_eq!(read.source, "run_log");
    assert!(read.log.text.trim().is_empty());
    assert!(read.log.checkout_evidence.commits.is_empty());
    let reason = read.fallback_error.expect("fallback reports why");
    assert!(
        reason.contains("Not Found (HTTP 404)") && reason.contains("job 101560010340"),
        "unexpected reason: {reason}"
    );
    assert!(
        !reason.contains("ghp_"),
        "a credential in gh's stderr must not survive: {reason}"
    );
    assert!(reason.contains("[REDACTED_SECRET]"));
}

#[test]
fn an_empty_job_log_falls_through_to_the_next_failed_job() {
    let mut view = run_view(&run_url(RUN_ID));
    view["jobs"][0] = json!({
        "databaseId": PASSING_JOB_ID,
        "name": "build",
        "status": "completed",
        "conclusion": "failure",
        "steps": [],
    });
    let log = job_log();
    let gh = FakeGh::new(
        &view,
        &[(PASSING_JOB_ID, ""), (FAILED_JOB_ID, log.as_str())],
    );

    let read = gh.read(failed_scope(RUN_ID), 16_384);

    assert_eq!(read.source, "job_api_log");
    assert_eq!(read.source_jobs.len(), 1);
    assert_eq!(read.source_jobs[0]["job_id"], json!(FAILED_JOB_ID));
    assert!(read.log.text.contains("reject_root_override"));
}

#[test]
fn an_oversized_job_log_is_bounded_and_keeps_the_failing_tail() {
    let filler = "2026-09-06T21:20:00.0000000Z Compiling something very verbose\n".repeat(20_000);
    let log = format!(
        "\u{feff}2026-09-06T21:15:07.8469063Z Current runner version: '2.337.0'\n{filler}{}",
        "2026-09-06T21:28:07.4229928Z ##[error]Process completed with exit code 101.\n"
    );
    let gh = FakeGh::new(
        &run_view(&run_url(RUN_ID)),
        &[(FAILED_JOB_ID, log.as_str())],
    );

    let read = gh.read(failed_scope(RUN_ID), 8_192);

    assert!(read.log.truncated);
    assert!(
        read.log.returned_bytes <= 8_192 + 128,
        "excerpt must respect the byte budget: {}",
        read.log.returned_bytes
    );
    assert_eq!(read.log.total_bytes, log.len());
    assert!(read.log.text.contains("bytes omitted"));
    // A whole-job log reaches its failure at the very end, so the tail is what
    // the reader came for.
    assert!(
        read.log
            .text
            .contains("Process completed with exit code 101")
    );
    assert!(read.log.text.contains("Current runner version"));
}

#[test]
fn whole_run_scope_recovers_checkout_evidence_from_a_job_log() {
    let log = job_log();
    let gh = FakeGh::new(
        &run_view(&run_url(RUN_ID)),
        &[(FAILED_JOB_ID, log.as_str())],
    );

    let read = gh.read(json!({"run": RUN_ID, "scope": "all"}), 16_384);

    assert_eq!(read.source, "job_api_log");
    assert_eq!(read.log.checkout_evidence.commits, vec![CHECKOUT_SHA]);
    assert!(read.log.checkout_evidence.complete);
    assert_read_the_failed_job(&gh.calls());
}

#[test]
fn a_narrowed_job_must_belong_to_the_run() {
    let gh = FakeGh::new(
        &run_view(&run_url(RUN_ID)),
        &[(FAILED_JOB_ID, "irrelevant")],
    );

    let read = gh.read(json!({"run": RUN_ID, "job": "424242"}), 16_384);

    let reason = read.fallback_error.expect("fallback reports why");
    assert!(
        reason.contains("job 424242 is not a job of run 34060485218"),
        "unexpected reason: {reason}"
    );
    assert_read_no_job_log(&gh.calls());
}

#[test]
fn a_failing_run_scoped_read_stays_an_error_instead_of_falling_back() {
    // Nothing in the script answers a bare `run view … --log-failed` for this
    // run id, so `gh` exits non-zero — a retryable transport failure, not the
    // empty-success ambiguity the fallback exists for.
    let gh = FakeGh::new(
        &run_view(&run_url(RUN_ID)),
        &[(FAILED_JOB_ID, "irrelevant")],
    );
    let mut requests = gh.requests(failed_scope(RUN_ID));
    requests.run_log.args = vec!["explode".to_string()];

    let error = match read_run_log(&requests, LogReadBounds::new(16_384)) {
        Err(error) => error,
        Ok(read) => panic!("a failing read must not fall back: {}", read.log.text),
    };

    assert!(
        error.to_string().contains("gh run view --log"),
        "unexpected error: {error}"
    );
    assert_read_no_job_log(&gh.calls());
}

#[test]
fn the_job_log_endpoint_is_a_read_of_this_repository_only() {
    let gh = FakeGh::new(&run_view(&run_url(RUN_ID)), &[]);

    gh.read(json!({"run": RUN_ID, "repo": "acme/orbit"}), 16_384);

    let call = gh
        .calls()
        .into_iter()
        .find(|call| call.contains("actions/jobs/"))
        .expect("job log endpoint call");
    assert_eq!(
        call,
        format!("api --method GET repos/acme/orbit/actions/jobs/{FAILED_JOB_ID}/logs")
    );
}

#[test]
fn a_repository_that_could_traverse_the_endpoint_is_rejected() {
    let rejected =
        RunLogRequests::from_input(&json!({"run": RUN_ID, "repo": "acme/orbit?per_page=9"}))
            .err()
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no error".to_string());

    assert!(
        rejected.contains("invalid `repo`"),
        "unexpected: {rejected}"
    );
}

/// Only a unix host gates execution on the permission bit.
#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

#[test]
fn source_read_limit_defers_without_retrying_or_returning_a_partial_unit() {
    let log = "bounded line\n".repeat(700_000);
    let gh = FakeGh::new(&run_view(&run_url(RUN_ID)), &[(FAILED_JOB_ID, &log)]);
    let read = gh.read(json!({"run": RUN_ID, "job": FAILED_JOB_ID}), 16_384);
    assert!(read.log.diagnostic.is_none());
    assert!(read.log.text.is_empty());
    assert!(
        read.fallback_error
            .as_deref()
            .is_some_and(|error| error.contains("8 MiB source read limit"))
    );
    assert_eq!(gh.calls().len(), 3);
}

#[test]
#[ignore = "read-only live GitHub verification requires authenticated gh and retained historical logs"]
fn live_long_job_logs_retain_complete_command_and_checkout() {
    for job in [101862218002_u64, 101862218165] {
        let requests = RunLogRequests::from_input(&json!({
            "run": "34160850121", "job": job, "scope": "failed", "repo": "constellation-works/orbit",
        })).expect("requests");
        let read = read_run_log(&requests, LogReadBounds::new(16_384)).expect("live read");
        match read.source {
            "job_api_log" => {
                assert_eq!(read.source_jobs.len(), 1);
                assert_eq!(read.source_jobs[0]["job_id"], job);
            }
            "run_log" => assert!(read.source_jobs.is_empty()),
            other => panic!("unexpected log source: {other}"),
        }
        assert!(read.log.total_bytes > 16_384);
        assert!(read.log.returned_bytes <= 16_384 + 128);
        assert!(read.log.truncated);
        assert!(read.log.source_complete);
        let checkout = if read.log.checkout_evidence.commits.is_empty() {
            let requests = RunLogRequests::from_input(&json!({
                "run": "34160850121", "job": job, "scope": "all", "repo": "constellation-works/orbit",
            })).expect("checkout requests");
            read_run_log(&requests, LogReadBounds::new(16_384))
                .expect("checkout read")
                .log
                .checkout_evidence
        } else {
            read.log.checkout_evidence
        };
        assert!(checkout.complete);
        assert_eq!(checkout.commits.len(), 1);
        assert!(checkout.commits[0].starts_with("a52912235"));
        let unit = read.log.diagnostic.expect("complete unit");
        assert!(unit.len() <= 262_144);
        assert!(unit.contains("owner_machine_id"));
        assert!(unit.contains("error[E0063]"));
        assert!(unit.contains("Process completed with exit code"));
        assert!(!unit.contains("##[group]Run df -h"));
        if read.source == "run_log" {
            let (job_name, step) = if job == 101862218002 {
                ("Check / Clippy / Test", "Run CI guardrails")
            } else {
                ("Coverage (informational)", "Collect workspace coverage")
            };
            assert!(
                unit.lines()
                    .all(|line| line.starts_with(&format!("{job_name}\t{step}\t")))
            );
        }
    }
}
