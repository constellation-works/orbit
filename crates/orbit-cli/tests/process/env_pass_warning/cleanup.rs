//! A fixture owns all runs in its private registry, including submissions whose
//! CLI output cannot be parsed. Run finalization precedes actual worker exit.

use std::time::{Duration, Instant};

use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_common::test_env;
use orbit_types::workflow::JobRunState;
use rusqlite::{Connection, OpenFlags};

use super::Fixture;

impl Fixture {
    fn finish_workers(&mut self) -> Result<(), String> {
        let connection = Connection::open_with_flags(
            self.home.join(".orbit/orbit.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|error| error.to_string())?;
        connection
            .busy_timeout(test_env::FIXTURE_STEP_DEADLINE)
            .map_err(|error| error.to_string())?;
        let runs = connection
            .prepare("SELECT run_id FROM job_runs")
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|error| error.to_string())?;
        let mut errors = Vec::new();
        for run in runs {
            if let Err(error) = self.finish_worker(&connection, &run) {
                errors.push(format!("{run}: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }

    fn finish_worker(&mut self, connection: &Connection, run: &str) -> Result<(), String> {
        let started = Instant::now();
        let mut cancelled = false;
        loop {
            let (state, pid, identity) = connection
                .query_row(
                    "SELECT state, pid, pid_start_time FROM job_runs WHERE run_id = ?1",
                    [run],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<u32>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .map_err(|error| error.to_string())?;
            let state: JobRunState = state.parse()?;
            let liveness = pid.map(|pid| probe_process_liveness(pid, identity.as_deref()));
            if state.is_terminal() && liveness == Some(ProcessLiveness::Exited) {
                return Ok(());
            }
            if state.is_terminal() && liveness == Some(ProcessLiveness::Alive) {
                // Regression rendezvous: terminal persistence must not release
                // the roots while the worker is still finishing its work.
                if let Some(observed) = self.terminal_worker_observed.take() {
                    let _ = observed.send(());
                }
            }
            if !state.is_terminal() && pid.is_some() && !cancelled {
                // Wait for the worker to claim before cancelling: a pending
                // row has no PID yet and cancelling it cannot prove spawn exit.
                let output = self
                    .command(&[])
                    .args(["run", "cancel", run, "--confirm", "--json"])
                    .timeout(test_env::FIXTURE_STEP_DEADLINE)
                    .output()
                    .map_err(|error| error.to_string())?;
                if !output.status.success() {
                    return Err(format!("cancellation failed: {output:?}"));
                }
                cancelled = true;
                continue;
            }
            if started.elapsed() >= test_env::FIXTURE_STEP_DEADLINE {
                return Err(format!(
                    "worker did not release ownership: state={state}, pid={pid:?}, \
                     liveness={liveness:?} ({})",
                    test_env::host_load()
                ));
            }
            // Poll the event; this interval is not evidence of worker exit.
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.submitted.get() {
            return;
        }
        if let Err(error) = self.finish_workers() {
            // Fail closed even when cleanup itself fails. Do not delete roots
            // still used by a worker, or double-panic during an assertion unwind.
            let root = self._temp.take().map(|temp| temp.keep());
            let message = format!("fixture worker cleanup failed; retained {root:?}: {error}");
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
fn fixture_roots_outlive_held_workers_on_drop_and_unwind() {
    use orbit_common::process::identity::ProcessLiveness;
    use serde_json::json;
    use std::fs;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::mpsc;

    use super::{JOB, SECRET, SET, UNSET, assert_recorded};
    use crate::workflow_gate::WorkflowGate;

    if crate::workflow_gate::wait_if_requested() {
        std::process::exit(0);
    }
    const CHILD: &str =
        "env_pass_warning::cleanup::fixture_roots_outlive_held_workers_on_drop_and_unwind";
    const MARKER: &str = "ORBIT_ENV_PASS_CLEANUP_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let root = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", CHILD, "--nocapture"])
            .env(MARKER, "1");
        let output = test_env::run_child_test(&mut command, CHILD, root.path());
        test_env::assert_child_test_passed(CHILD, output.status, &output.stdout, &output.stderr);
        return;
    }

    for (terminal, unwind) in [(true, false), (true, true), (false, true)] {
        // The scope also joins teardown if a controller assertion unwinds.
        std::thread::scope(|scope| {
            let gate = WorkflowGate::new();
            let mut fixture = Fixture::new();
            super::set_worker_containment(&fixture, false);
            let job = json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": JOB},
            "spec": {"state": "enabled", "kind": "workflow", "steps": [gate.step("held")]}});
            fs::write(
                fixture
                    .home
                    .join(format!(".orbit/resources/jobs/{JOB}.yaml")),
                job.to_string(),
            )
            .unwrap();
            let (submitted, stderr) =
                fixture.start(&[(SET, SECRET)], &["run", "job", JOB, "--json"]);
            assert_eq!(super::warning_lines(&stderr).len(), 1);
            assert_recorded(&fixture, &submitted, &[UNSET]);
            gate.wait_ready();
            let run = submitted["run_id"].as_str().unwrap();
            let connection = Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
            let (pid, identity): (u32, String) = connection
                .query_row(
                    "SELECT pid, pid_start_time FROM job_runs WHERE run_id = ?1",
                    [run],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                probe_process_liveness(pid, Some(&identity)),
                ProcessLiveness::Alive
            );
            let roots = fixture._temp.as_ref().unwrap().path().to_owned();
            let (observed, observation) = mpsc::sync_channel(1);
            if terminal {
                // Deterministically model the interval between terminal persistence
                // and process exit. The real worker remains held at the rendezvous.
                connection
                    .execute(
                        "UPDATE job_runs SET state = 'success' WHERE run_id = ?1",
                        [run],
                    )
                    .unwrap();
                fixture.terminal_worker_observed = Some(observed);
            }
            let (finished, completion) = mpsc::sync_channel(1);
            let teardown = scope.spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(move || {
                    let owned = fixture;
                    assert!(!unwind, "deliberate failure after detached submission");
                    drop(owned);
                }));
                finished.send(result.is_err()).unwrap();
            });
            if terminal {
                observation
                    .recv_timeout(test_env::FIXTURE_STEP_DEADLINE)
                    .expect("teardown observed a terminal but live worker");
                assert!(
                    roots.join("home/.orbit/orbit.db").is_file()
                        && roots.join("repo/.git").is_dir()
                );
                assert!(
                    matches!(completion.try_recv(), Err(mpsc::TryRecvError::Empty)),
                    "teardown must wait for actual worker exit"
                );
                assert_eq!(
                    probe_process_liveness(pid, Some(&identity)),
                    ProcessLiveness::Alive
                );
                gate.release();
            }
            assert_eq!(
                completion
                    .recv_timeout(test_env::FIXTURE_STEP_DEADLINE)
                    .unwrap(),
                unwind
            );
            teardown.join().unwrap();
            assert_eq!(
                probe_process_liveness(pid, Some(&identity)),
                ProcessLiveness::Exited
            );
            assert!(
                !roots.exists(),
                "temporary roots are removed after worker exit"
            );
            if !terminal {
                let state: String = connection
                    .query_row(
                        "SELECT state FROM job_runs WHERE run_id = ?1",
                        [run],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(state, "cancelled");
                gate.release();
            }
        });
    }
}
