//! Shared helpers for the retained API security and interleaving tests.
use axum::body::to_bytes;
use axum::response::Response;
use chrono::Utc;
use orbit_core::{JobRun, JobRunState, OrbitRuntime};
use serde_json::Value;

const ISOLATED_TEST_ENV: &str = "ORBIT_TEST_WEB_FIXTURE_CHILD";

/// Run the calling test's body in a child of this test binary.
///
/// An explicit-root fixture (`OrbitRuntime::from_roots`, or a global
/// `DashboardState` that opens one lazily) reads ambient authority from the
/// process environment. Inherited from a managed run, that authority can route
/// writes to the live workspace or refuse runtime construction before the
/// behavior under test runs; a temporary root is not process isolation. The
/// child starts with that authority cleared and a disposable `HOME`,
/// `USERPROFILE` and working directory; the parent's environment is untouched.
///
/// Returns `true` inside the child, where the caller runs its body, and
/// `false` in the parent once the child ran exactly that test and passed.
pub(super) fn enter_isolated_child(module: &str, test: &str) -> bool {
    run_isolated_child(module, test).is_none()
}

fn run_isolated_child(module: &str, test: &str) -> Option<String> {
    let module = module
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module);
    let exact_test = format!("{module}::{test}");
    if std::env::var_os(ISOLATED_TEST_ENV).is_some_and(|name| name == exact_test.as_str()) {
        return None;
    }

    let home = tempfile::tempdir().expect("isolated fixture home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command.args(["--exact", &exact_test, "--nocapture", "--test-threads=1"]);
    let output = command
        .env(ISOLATED_TEST_ENV, &exact_test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .expect("run isolated fixture");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "isolated `{exact_test}` failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "the isolated child must run `{exact_test}` itself, not filter it out:\n{stdout}"
    );
    Some(stdout)
}

/// Refuse an explicit-root fixture outside [`enter_isolated_child`], so a new
/// test cannot silently build one with the launching process's authority.
pub(super) fn assert_isolated_child() {
    assert!(
        std::env::var_os(ISOLATED_TEST_ENV).is_some(),
        "explicit-root web fixtures must run through `enter_isolated_child`"
    );
}

pub(super) async fn body_json(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    serde_json::from_slice(&bytes).expect("json response")
}

pub(super) fn seed_run(
    runtime: &OrbitRuntime,
    run_id: &str,
    job_id: &str,
    state: JobRunState,
) -> JobRun {
    let now = Utc::now();
    let run = JobRun {
        executed_on: None,
        run_id: run_id.to_string(),
        job_id: job_id.to_string(),
        attempt: 1,
        state,
        scheduled_at: now,
        started_at: matches!(
            state,
            JobRunState::Running
                | JobRunState::Success
                | JobRunState::Failed
                | JobRunState::Timeout
                | JobRunState::Cancelled
        )
        .then_some(now),
        finished_at: state.is_terminal().then_some(now),
        duration_ms: state.is_terminal().then_some(0),
        created_at: now,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    write_seeded_run(runtime, &run);
    run
}

fn write_seeded_run(runtime: &OrbitRuntime, run: &JobRun) {
    let workspace_id = runtime.workspace_id().expect("workspace id");
    runtime
        .sqlite_store()
        .expect("sqlite store")
        .upsert_job_run_for_workspace(&workspace_id, run, None)
        .expect("insert job run");
}

pub(super) fn write_lines(path: &std::path::Path, lines: &[String]) {
    let mut content = String::new();
    for line in lines {
        content.push_str(line);
        content.push('\n');
    }
    std::fs::write(path, content).expect("write fixture");
}
