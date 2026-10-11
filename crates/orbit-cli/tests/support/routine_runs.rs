//! Own every routine fixture worker through teardown, including unwinds.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_common::test_env;
use orbit_core::OrbitRuntime;
use orbit_core::application::job::JobRunListParams;
use orbit_types::workflow::JobRun;
use serde_json::Value;
use tempfile::TempDir;

pub(super) struct RoutineRuns {
    temp: Option<TempDir>,
    home: PathBuf,
    repos: Vec<PathBuf>,
    submitted: Vec<String>,
    #[cfg(unix)]
    gate: Option<crate::workflow_gate::WorkflowGate>,
    // Retained outside a caught assertion so the regression can inspect what
    // cleanup actually observed before the shared roots were removed.
    pub(super) settlements: Arc<Mutex<Vec<JobRun>>>,
}

impl RoutineRuns {
    pub(super) fn new(temp: TempDir, home: &Path, repos: &[&Path]) -> Self {
        Self {
            temp: Some(temp),
            home: home.to_owned(),
            repos: repos.iter().map(|repo| repo.to_path_buf()).collect(),
            submitted: Vec::new(),
            #[cfg(unix)]
            gate: None,
            settlements: Arc::default(),
        }
    }

    pub(super) fn retain_sweep(&mut self, sweep: &Value) {
        // Collect before asserting even the reports' shape or dispatch action.
        if let Some(reports) = sweep["reports"].as_array() {
            self.submitted.extend(
                reports
                    .iter()
                    .filter_map(|report| report["run_id"].as_str().map(str::to_owned)),
            );
        }
    }

    #[cfg(unix)]
    pub(super) fn hold_owner(&mut self, owner_repo: &Path) {
        let gate = crate::workflow_gate::WorkflowGate::new();
        // Shipped default names stay bound to the host catalog. Give the
        // owner a custom job retaining the real GC step; the replica continues
        // to execute the unmodified host catalog job.
        let source = self
            .home
            .join(".orbit/resources/jobs/worktree_gc_pipeline.yaml");
        let yaml: serde_yaml::Value =
            serde_yaml::from_slice(&std::fs::read(source).unwrap()).unwrap();
        let mut job = serde_json::to_value(yaml).unwrap();
        let name = "held_owner_worktree_gc";
        job["metadata"]["name"] = name.into();
        let steps = job["spec"]["steps"].as_array_mut().unwrap();
        steps.insert(0, gate.step("owner_rendezvous"));
        let jobs = owner_repo.join(".orbit/resources/jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::write(jobs.join(format!("{name}.yaml")), job.to_string()).unwrap();
        let routine_path = owner_repo.join(".orbit/routines/worktree_gc.yaml");
        let yaml: serde_yaml::Value =
            serde_yaml::from_slice(&std::fs::read(&routine_path).unwrap()).unwrap();
        let mut routine = serde_json::to_value(yaml).unwrap();
        routine["target"] = format!("job:{name}").into();
        std::fs::write(routine_path, routine.to_string()).unwrap();
        self.gate = Some(gate);
    }

    #[cfg(unix)]
    pub(super) fn wait_owner_ready(&self) {
        self.gate.as_ref().expect("owner gate").wait_ready();
    }

    pub(super) fn release_owner(&mut self) {
        #[cfg(unix)]
        if let Some(gate) = self.gate.take() {
            gate.release();
        }
    }

    pub(super) fn settle(&mut self) -> Result<(), String> {
        // Dropping the gate releases an entered waiter without panicking. If
        // readiness failed, dropping its listener makes a late waiter fail
        // rather than wait on an ownerless rendezvous.
        #[cfg(unix)]
        drop(self.gate.take());
        let mut observed = Vec::new();
        let mut errors = Vec::new();
        for repo in &self.repos {
            let result = (|| {
                let runtime =
                    OrbitRuntime::from_roots(&self.home.join(".orbit"), &repo.join(".orbit"))
                        .map_err(|error| error.to_string())?;
                // Discover every admission, including the --wait seed and a
                // sweep which failed before retain_sweep could inspect stdout.
                let runs = runtime
                    .list_job_runs_observed(JobRunListParams::default())
                    .map_err(|error| error.to_string())?;
                for run in runs {
                    match settle_run(&runtime, &run.run_id) {
                        Ok(run) => observed.push(run),
                        Err(error) => errors.push(error),
                    }
                }
                Ok::<_, String>(())
            })();
            if let Err(error) = result {
                errors.push(error);
            }
        }
        for id in &self.submitted {
            if !observed.iter().any(|run| run.run_id == *id) {
                errors.push(format!("submitted run {id} was not settled"));
            }
        }
        *self.settlements.lock().map_err(|error| error.to_string())? = observed;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }
}

fn settle_run(runtime: &OrbitRuntime, id: &str) -> Result<JobRun, String> {
    let deadline = Instant::now() + test_env::FIXTURE_STEP_DEADLINE;
    let mut cancelled = false;
    loop {
        let run = runtime
            .show_job_run(id)
            .map_err(|error| error.to_string())?;
        let exited = run.pid.is_none_or(|pid| {
            probe_process_liveness(pid, run.pid_start_time.as_deref()) == ProcessLiveness::Exited
        });
        if run.state.is_terminal() && exited {
            return Ok(run);
        }
        if Instant::now() >= deadline {
            if cancelled {
                return Err(format!(
                    "worker did not stop after cancellation: {run:?} ({})",
                    test_env::host_load()
                ));
            }
            runtime
                .cancel_job_run(id)
                .map_err(|error| error.to_string())?;
            cancelled = true;
        }
        // After cancellation, allow its confirmed process termination to be
        // observed on the next pass; this is not a second unbounded wait.
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl Drop for RoutineRuns {
    fn drop(&mut self) {
        if let Err(error) = self.settle() {
            // An unconfirmed worker must never lose its roots. Preserve them
            // and fail the case, without a second panic during an unwind.
            let path = self.temp.take().map(TempDir::keep);
            let _ = writeln!(
                std::io::stderr(),
                "fixture cleanup failed; retained {path:?}: {error}"
            );
            if !std::thread::panicking() {
                panic!("fixture workers did not settle: {error}");
            }
        }
    }
}
