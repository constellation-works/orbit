//! Window admission preserves single-pass controls and owns queued worker teardown.

use super::*;

#[test]
fn malformed_duration_errors_show_accepted_format_without_submitting_a_run() {
    if !isolated_run_observation(
        "run_observation::drain_window::malformed_duration_errors_show_accepted_format_without_submitting_a_run",
    ) {
        return;
    }
    let fixture = Fixture::terminal();
    let before = fixture.snapshot();
    for duration in ["bogus", "12", ""] {
        let args = ["run", "auto", "--for", duration, "--json"];
        let output = fixture.orbit().args(args).output().unwrap();
        assert!(
            !output.status.success(),
            "{args:?} accepted malformed input"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("30m"), "{args:?}: {stderr}");
        assert_eq!(fixture.snapshot(), before, "{args:?} must submit no run");
    }
}

#[test]
fn oversized_windows_create_no_run_and_single_pass_still_submits() {
    if !isolated_run_observation(
        "run_observation::drain_window::oversized_windows_create_no_run_and_single_pass_still_submits",
    ) {
        return;
    }
    let fixture = DrainWindowFixture::new();
    // Keep accepted submissions queued. They still launch workers, which the
    // guard cancels and observes stopped before dropping the fixture roots.
    let now = chrono::Utc::now().to_rfc3339();
    fixture.db().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,created_at,pid) VALUES ('jrun-window-incumbent',?1,'workspace_auto_pipeline',1,'running',?2,?2,?2,?3)",
        params![fixture.workspace_id(), now, std::process::id()],
    ).unwrap();
    let before = fixture.snapshot();
    for pull in [false, true] {
        for duration in ["2d", "86401s"] {
            let mut args = vec!["run", "auto", "--for", duration, "--json"];
            if pull {
                args.extend(["--pull", "hm_owner/ws_fixture"]);
            }
            let output = fixture.orbit().args(&args).output().unwrap();
            assert!(
                !output.status.success(),
                "{args:?} submitted a run: {output:?}"
            );
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert!(
                error["error"]
                    .as_str()
                    .unwrap()
                    .contains("for_seconds must be a whole number from 1 to 86400 (24 hours)"),
                "{args:?}: {error}"
            );
            assert_eq!(fixture.snapshot(), before, "refused input creates no run");
        }
    }
    for (duration, seconds) in [(None, 0), (Some("0s"), 0), (Some("24h"), 86_400)] {
        let mut args = vec!["run", "auto", "--json"];
        if let Some(duration) = duration {
            args.extend(["--for", duration]);
        }
        let receipt = fixture.json(&args);
        assert_eq!(receipt["state"], "queued", "{receipt}");
        assert_eq!(receipt["job_id"], "workspace_auto_pipeline");
        assert!(receipt.get("submitted_at").is_none());
        assert!(receipt.get("review").is_none());
        let input: String = fixture
            .db()
            .query_row(
                "SELECT input_json FROM job_runs WHERE run_id = ?1",
                [receipt["run_id"].as_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        let input: Value = serde_json::from_str(&input).unwrap();
        assert_eq!(input["for_seconds"], seconds);
    }
}

// A queued receipt still launches a detached worker. Own all real submissions
// in this private store, even when receipt parsing or a later assertion fails.
// Synthetic incumbent rows belong to the test process and must not be signalled.
const INCUMBENT: &str = "jrun-window-incumbent";

struct DrainWindowFixture(Fixture);

impl std::ops::Deref for DrainWindowFixture {
    type Target = Fixture;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DrainWindowFixture {
    fn new() -> Self {
        Self(Fixture::terminal())
    }

    fn submitted_drains(&self) -> Result<Vec<String>, String> {
        self.db()
            .prepare("SELECT run_id FROM job_runs WHERE run_id NOT IN (?1, ?2) ORDER BY created_at")
            .and_then(|mut statement| {
                statement
                    .query_map([INCUMBENT, FAILED], |row| row.get(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|error| error.to_string())
    }

    fn worker_owner(&self, run: &str) -> Result<(String, Option<u32>, Option<String>), String> {
        self.db()
            .query_row(
                "SELECT state, pid, pid_start_time FROM job_runs WHERE run_id = ?1",
                [run],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|error| error.to_string())
    }

    fn finish_submitted_drains(&self) -> Result<(), String> {
        let mut errors = Vec::new();
        for run in self.submitted_drains()? {
            if let Err(error) = self.finish_drain_worker(&run) {
                errors.push(format!("{run}: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }

    fn finish_drain_worker(&self, run: &str) -> Result<(), String> {
        use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
        use orbit_types::workflow::JobRunState;
        use std::time::{Duration, Instant};

        let deadline = Instant::now() + test_env::FIXTURE_STEP_DEADLINE;
        let mut cancelled = false;
        loop {
            let (state, pid, identity) = self.worker_owner(run)?;
            let state: JobRunState = state.parse()?;
            let liveness = pid.map(|pid| probe_process_liveness(pid, identity.as_deref()));
            if state.is_terminal() && liveness == Some(ProcessLiveness::Exited) {
                let _ = writeln!(
                    std::io::stderr(),
                    "test-note: drain-worker-exit run={run} pid={} state={state} owner=exited",
                    pid.unwrap()
                );
                return Ok(());
            }
            if !state.is_terminal() && pid.is_some() && !cancelled {
                // Pending is not process-free. Wait for the worker to publish
                // its identity before cancellation, then observe actual exit.
                let output = self
                    .orbit()
                    .args(["run", "cancel", run, "--force", "--confirm", "--json"])
                    .timeout(test_env::FIXTURE_STEP_DEADLINE)
                    .output()
                    .map_err(|error| error.to_string())?;
                if !output.status.success() {
                    return Err(format!("cancellation failed: {output:?}"));
                }
                cancelled = true;
                continue;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "worker did not exit: state={state}, pid={pid:?}, liveness={liveness:?} ({})",
                    test_env::host_load()
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for DrainWindowFixture {
    fn drop(&mut self) {
        if let Err(error) = self.finish_submitted_drains() {
            let root = self.0._temp.take().map(|temp| temp.keep());
            let message = format!("drain fixture cleanup failed; retained {root:?}: {error}");
            if std::thread::panicking() {
                tracing::error!("{message}");
            } else {
                panic!("{message}");
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn submitted_drains_are_stopped_on_drop_and_unwind_before_roots_are_removed() {
    if !isolated_run_observation(
        "run_observation::drain_window::submitted_drains_are_stopped_on_drop_and_unwind_before_roots_are_removed",
    ) {
        return;
    }
    use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
    use std::panic::{AssertUnwindSafe, catch_unwind};

    for (start, unwind) in [(false, false), (true, true)] {
        let mut fixture = DrainWindowFixture::new();
        let now = chrono::Utc::now().to_rfc3339();
        fixture.db().execute(
            "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'workspace_auto_pipeline',1,'running',?3,?3,?3,?4)",
            params![INCUMBENT, fixture.workspace_id(), now, std::process::id()],
        ).unwrap();
        let receipt = fixture.json(&["run", "auto", "--for", "24h", "--json"]);
        assert_eq!(receipt["state"], "queued", "{receipt}");
        let run = receipt["run_id"].as_str().unwrap();
        test_env::wait_until("queued drain worker ownership", || {
            fixture.worker_owner(run).unwrap().1.is_some()
        });
        let (_, pid, identity) = fixture.worker_owner(run).unwrap();
        let pid = pid.unwrap();
        assert_ne!(pid, std::process::id());
        assert_eq!(
            probe_process_liveness(pid, identity.as_deref()),
            ProcessLiveness::Alive
        );
        if start {
            // Release only the synthetic incumbent. The real queued worker
            // now starts its accepted 24h window, without load or provider work.
            fixture
                .db()
                .execute(
                    "UPDATE job_runs SET state='interrupted',finished_at=?1 WHERE run_id=?2",
                    params![now, INCUMBENT],
                )
                .unwrap();
            test_env::wait_until("24h drain starts after incumbent exits", || {
                fixture.run_state(run) == "running"
            });
        }
        let run = run.to_owned();
        let db = fixture.db();
        // Retain the real roots outside the guard so the cwd-deletion watchdog
        // cannot make a missing teardown pass this regression.
        let roots = fixture.0._temp.take().unwrap();
        let dropped = catch_unwind(AssertUnwindSafe(move || {
            let _fixture = fixture;
            assert!(!unwind, "exercise cleanup during an assertion unwind");
        }));
        assert_eq!(dropped.is_err(), unwind);
        assert!(roots.path().exists());
        let state: String = db
            .query_row(
                "SELECT state FROM job_runs WHERE run_id=?1",
                [&run],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "cancelled",
            "queued or started drain must be cancelled before root deletion"
        );
        assert_eq!(
            probe_process_liveness(pid, identity.as_deref()),
            ProcessLiveness::Exited,
            "worker must exit before fixture teardown"
        );
        writeln!(std::io::stderr(), "test-note: drain cleanup start={start} unwind={unwind} run={run} pid={pid} state={state} owner=exited").unwrap();
    }
}
