//! [ORB-15435] The `pipeline.child_wait` audit row a parent records when the
//! child it blocked on settles without success.
//!
//! A held child's wait entry carries no `error`, and neither does a wait that
//! timed out on a running child, so the row once said only that the parent's
//! wait failed. Every non-success row now names the child run, its status and,
//! when the child recorded one, its reason. A held or cancelled child is a
//! settlement the parent handles, not a failed observation, so it no longer
//! reaches the incident list; a failed child stays an unexpected incident.
//!
//! Each case submits a real child through `invoke_and_wait` and executes or
//! cancels it in process while the parent's wait polls it.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_core::{FailureClass, OrbitRuntime};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::{JobRunStoreBackend, classify};
use orbit_tools::ToolContext;
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{BaselineRedHold, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

const PARENT: &str = "child_wait_parent";
const CHILD: &str = "child_wait_child";

struct Fixture {
    root: TempDir,
    global: PathBuf,
    repo: PathBuf,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
    parent: String,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        // A submitted child's substitute worker stays alive until the test
        // has settled the child, so the supervisor never interrupts it.
        crate::worker_fixture::install(root.path().join("ran-{run_id}").as_path(), "created");
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        // The child runs the guard a gate runs over its own child's result,
        // so its outcome follows the result named in its input.
        std::fs::write(
            jobs_dir.join(format!("{CHILD}.yaml")),
            serde_json::to_string(&json!({
                "schemaVersion": 2, "kind": "Job",
                "metadata": {"name": CHILD},
                "spec": {"state": "enabled", "steps": [{
                    "id": "require_child_success",
                    "spec": {
                        "type": "deterministic",
                        "action": "pipeline_success_guard",
                        "config": {},
                    },
                    "default_input": {
                        "context": "fixture leaf",
                        "results": "{{ input.results }}",
                    },
                }]}
            }))
            .unwrap(),
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        let parent = jobs
            .insert_job_run(PARENT, 1, Utc::now(), Some(json!({})), None)
            .unwrap();
        jobs.write_run_state(
            &parent.run_id,
            &PipelineState::new(parent.run_id.clone(), parent.job_id.clone(), json!({})),
        )
        .unwrap();
        Self {
            root,
            global,
            repo,
            runtime,
            jobs,
            parent: parent.run_id,
        }
    }

    /// Submit a child over `results` and wait on it, while `settle` acts on
    /// the submitted child from another thread. Returns the child's
    /// `pipeline.child_wait` row and the activity's output.
    fn observe(
        &self,
        results: Value,
        timeout_seconds: u64,
        settle: impl FnOnce(&OrbitRuntime, &str) + Send,
    ) -> (String, AuditEvent, Value) {
        let before = self.child_runs();
        let (child, output) = std::thread::scope(|scope| {
            let settler = scope.spawn(|| {
                let child = self.await_new_child(&before);
                // A second runtime on the same roots, as a worker opens.
                let runtime =
                    OrbitRuntime::from_roots(&self.global, &self.repo.join(".orbit")).unwrap();
                settle(&runtime, &child);
                child
            });
            let output = crate::scenarios::with_prompt_pipeline_polls(|| {
                self.runtime
                    .run_deterministic(
                        "invoke_and_wait",
                        &json!({}),
                        &json!({
                            "job_name": CHILD,
                            "run_input": {"results": results},
                            "run_id": self.parent,
                            "step_id": "deliver",
                            "timeout_seconds": timeout_seconds,
                            "poll_interval_seconds": 1,
                        }),
                        // The engine dispatches a gate's child with operator authority.
                        ToolContext {
                            session_context: ToolSessionContext {
                                effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                                ..ToolSessionContext::default()
                            },
                            ..ToolContext::default()
                        },
                    )
                    .expect("the wait returns the child's settlement as data")
            });
            (settler.join().unwrap(), output)
        });
        std::fs::write(self.root.path().join(format!("ran-{child}")), "").unwrap();
        let row = self
            .runtime
            .list_audit_events(None, None, None, None, 1000)
            .unwrap()
            .into_iter()
            .find(|event| {
                event.command == "pipeline.child_wait"
                    && event.target_id.as_deref() == Some(child.as_str())
            })
            .unwrap_or_else(|| panic!("no pipeline.child_wait row for {child}"));
        (child, row, output)
    }

    fn child_runs(&self) -> Vec<String> {
        self.jobs
            .list_job_runs(CHILD)
            .unwrap()
            .into_iter()
            .map(|run| run.run_id)
            .collect()
    }

    fn await_new_child(&self, before: &[String]) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(child) = self
                .child_runs()
                .into_iter()
                .find(|run_id| !before.contains(run_id))
            {
                return child;
            }
            assert!(Instant::now() < deadline, "the child was never submitted");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn execute(runtime: &OrbitRuntime, child: &str) {
    // The worker reports an unsuccessful run as an error; the parent's wait
    // reads the stored outcome.
    let _ = runtime.execute_pipeline_run_worker(child);
}

fn payload(row: &AuditEvent) -> Value {
    serde_json::from_str(row.arguments_json.as_deref().unwrap()).unwrap()
}

fn baseline_red_text() -> String {
    BaselineRedHold {
        base_ref: "origin/agent-main".to_string(),
        base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        command: "make ci-fast".to_string(),
        run_id: "jrun-observed".to_string(),
        selection: None,
        failures: Default::default(),
    }
    .text("the required check fails on the base exactly as on the candidate")
}

#[test]
fn a_child_wait_row_names_the_child_and_why_and_only_failures_are_incidents() {
    if !isolated(
        "child_wait_audit::a_child_wait_row_names_the_child_and_why_and_only_failures_are_incidents",
    ) {
        return;
    }
    let fixture = Fixture::new();

    // The fixture guard supplies the leaf's own error without a child run
    // identity; a guard with one would echo that child's typed identity.
    // Held on a red base: the wait entry carries no error, so the reason
    // comes from the child's own hold step.
    let (child, row, _) = fixture.observe(
        json!([{"status": "failed", "error": baseline_red_text()}]),
        60,
        execute,
    );
    let recorded = payload(&row);
    assert_eq!(recorded["status"], "held", "{recorded}");
    assert_eq!(recorded["error"], Value::Null, "{recorded}");
    assert_eq!(
        row.status,
        AuditEventStatus::Success,
        "a held child passes its parent's guard and is not a failed observation"
    );
    assert_eq!(row.error_message, None);
    let message = recorded["message"].as_str().unwrap();
    assert!(
        message.starts_with(&format!("child run {child} status held: baseline_red: ")),
        "{message}"
    );

    // A genuine failure keeps the child's error and stays unexpected.
    let (_, row, _) = fixture.observe(
        json!([{"status": "failed", "error": "cargo test failed"}]),
        60,
        execute,
    );
    assert_eq!(payload(&row)["status"], "failed");
    assert_eq!(row.status, AuditEventStatus::Failure);
    assert!(
        row.error_message
            .as_deref()
            .is_some_and(|message| message.contains("cargo test failed")),
        "{:?}",
        row.error_message
    );
    assert_eq!(
        classify(&row),
        FailureClass::Unexpected,
        "a failed child stays an unexpected incident"
    );

    // An operator cancel ends the parent cancelled, not failed.
    let (child, row, _) = fixture.observe(json!([]), 60, |runtime, child| {
        runtime.cancel_job_run(child).unwrap();
    });
    let recorded = payload(&row);
    assert_eq!(recorded["status"], "cancelled", "{recorded}");
    assert_eq!(row.status, AuditEventStatus::Success);
    assert_eq!(row.error_message, None);
    assert_eq!(
        recorded["message"],
        format!("child run {child} status cancelled: the child recorded no reason"),
        "an operator cancel records no reason, so the row still names the child"
    );

    // The wait's deadline projects a still-pending child as `timeout`. A
    // caller that did not ask for it to be cancelled is told it is still
    // live, so it frees nothing the child uses [ORB-15643].
    let (child, row, output) = fixture.observe(json!([]), 1, |_, _| {});
    assert_eq!(payload(&row)["status"], "timeout");
    assert_eq!(output["child_live"], true, "{output}");
    assert_eq!(row.status, AuditEventStatus::Failure);
    assert_eq!(
        row.error_message.as_deref(),
        Some(
            format!(
                "child run {child} status timeout: no reason recorded; the wait deadline passed \
                 before the child settled"
            )
            .as_str()
        )
    );
    assert_eq!(classify(&row), FailureClass::Unexpected);
}
