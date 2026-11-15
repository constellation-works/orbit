//! An explicit pilot during an approve-proposed drain calls the provider once
//! [ORB-21136]. The shipped `workspace_auto_pipeline` and
//! `task_pilot_pipeline` jobs run in this process the way their workers
//! would, submitted through ordinary admission. Every pilot run, the
//! operator's and any the drain dispatches, executes its real steps against
//! a scripted provider that counts its invocations. Only delivery leaves are
//! settled by the test.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use orbit_core::DrainAdmissionsStopRequest;
use orbit_types::workflow::ExecutorSandboxKind;

use super::step_graph::PILOT_JOB;
use super::*;

/// Scripted codex CLI. Each invocation appends a line to `calls`, then waits
/// for `release` before it assesses the task ids its prompt names.
fn install_provider(workspace: &Workspace, calls: &Path, release: &Path) {
    let provider = workspace._root.path().join("codex");
    std::fs::write(calls, "").unwrap();
    std::fs::write(
        &provider,
        format!(
            r#"#!/usr/bin/env python3
import json, os, re, subprocess, sys, time
raw = sys.stdin.read()
with open({calls:?}, "a", encoding="utf-8") as handle:
    handle.write("call\n")
deadline = time.time() + 120
while not os.path.exists({release:?}):
    if time.time() > deadline:
        sys.stderr.write("provider was never released\n")
        sys.exit(1)
    time.sleep(0.05)
found = []
for blob in re.findall(r'"task_ids"\s*:\s*(\[[^\]]*\])', raw):
    try:
        parsed = json.loads(blob)
    except json.JSONDecodeError:
        continue
    ids = [item for item in parsed if isinstance(item, str) and re.fullmatch(r"ORB-\d+", item)]
    if ids:
        found = ids
indexes = [int(value) for value in re.findall(r'"partition_index"\s*:\s*(\d+)', raw)]
partition = indexes[-1] if indexes else 0
if not found:
    sys.stderr.write("provider saw no task ids\n")
    sys.exit(1)
tasks = [{{
    "task_id": task_id,
    "context_files_after": ["file:README.md"],
    "disposition": "selectors",
    "recommended_crew": "fixture",
    "recommended_complexity": "low",
    "confidence": "high",
    "assessment_rationale": "README.md contains the affected material.",
    "validation_approach": "Inspect the persisted scope.",
    "evidence_gaps": [],
    "reassessment_triggers": [],
    "blocked_by": [],
    "adr_conflicts": [],
    "utility_warnings": [],
    "surface_warnings": [],
    "duplicate_of": None,
    "already_landed": None,
}} for task_id in found]
head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
tree = subprocess.check_output(["git", "rev-parse", "HEAD^{{tree}}"], text=True).strip()
sys.stdout.write(json.dumps({{
    "schemaVersion": 1,
    "status": "success",
    "error": None,
    "result": {{"partition_index": partition, "observed_revision": head, "observed_tree": tree, "task_ids": found, "tasks": tasks}},
}}))
"#,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(provider.display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    workspace
        .runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();
}

fn provider_calls(calls: &Path) -> usize {
    std::fs::read_to_string(calls).unwrap().lines().count()
}

/// The worker a host would start for each pilot child of `drain`: it runs
/// the child's real steps here, once each, until `stop`.
fn run_drain_pilots(
    runtime: OrbitRuntime,
    drain: String,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut ran = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            let children = runtime
                .read_run_state(&drain)
                .unwrap()
                .map(|state| state.child_dispatches)
                .unwrap_or_default();
            for child in children {
                if child.job_name == PILOT_JOB && !ran.contains(&child.child_run_id) {
                    ran.push(child.child_run_id.clone());
                    let _ = runtime.execute_pipeline_run_worker(&child.child_run_id);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        ran
    })
}

fn prepared(workspace: &Workspace, run_id: &str) -> Value {
    workspace
        .runtime
        .read_run_state(run_id)
        .unwrap()
        .unwrap()
        .pipeline["prepare"]
        .clone()
}

#[test]
fn an_explicit_pilot_during_an_approve_proposed_drain_calls_the_provider_once() {
    if !crate::dispatch_admission::isolated(
        "drain_approval::explicit_pilot_reuse::an_explicit_pilot_during_an_approve_proposed_drain_calls_the_provider_once",
    ) {
        return;
    }
    let workspace = Workspace::with_shipped_jobs();
    let workers = workspace._root.path().join("release-workers");
    let mut owned_workers =
        crate::worker_fixture::install_owned_workers(&workers, workspace.repo.clone());
    let calls = workspace._root.path().join("provider-calls");
    let release = workspace._root.path().join("provider-release");
    install_provider(&workspace, &calls, &release);
    let task = workspace.task(
        "never piloted",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );

    // The operator's request, through the same submission `orbit task
    // repilot` uses. Its worker runs here and blocks in the provider.
    let explicit = workspace
        .runtime
        .submit_explicit_task_pilot(&task.id, Some("operator"), JobRunTrigger::cli())
        .unwrap()
        .run_id;
    let explicit_worker = {
        let runtime = workspace.runtime.clone();
        let explicit = explicit.clone();
        std::thread::spawn(move || runtime.execute_pipeline_run_worker(&explicit))
    };
    workspace.wait_for(
        &explicit,
        "the explicit pilot to reach its provider",
        || (provider_calls(&calls) == 1).then_some(()),
    );

    // An approve-proposed drain starts while that pilot is assessing.
    let drain = workspace
        .runtime
        .submit_pipeline_run(
            "workspace_auto_pipeline",
            json!({
                "for_seconds": 3600,
                "max_active_leaf_runs": 1,
                "completion": "review",
                "approve_proposed": true,
                "auto_concurrency": "enforce",
                "poll_sleep_seconds": 0,
                "idle_sleep_seconds": 1,
            }),
            None,
            None,
        )
        .unwrap()
        .run_id;
    let stop = Arc::new(AtomicBool::new(false));
    let drain_pilots = run_drain_pilots(workspace.runtime.clone(), drain.clone(), stop.clone());
    let executor = {
        let runtime = workspace.runtime.clone();
        let drain = drain.clone();
        std::thread::spawn(move || runtime.execute_pipeline_run_worker(&drain))
    };
    let first_pass = workspace.wait_for(&drain, "a drain pass over the piloted task", || {
        workspace
            .runtime
            .read_run_state(&drain)
            .unwrap()?
            .pull_drain
            .drain_approvals
    });

    // The assessment applies once and leaves the task proposed: an operator
    // pilot carries no promotion authority.
    std::fs::write(&release, "").unwrap();
    explicit_worker.join().unwrap().unwrap();
    assert_eq!(workspace.run(&explicit).state, JobRunState::Success);
    assert_eq!(
        prepared(&workspace, &explicit)["task_ids"],
        json!([task.id])
    );

    // The drain promotes that assessment and admits the task.
    let leaf = workspace.wait_for(&drain, "the reused task's leaf", || {
        workspace.leaf_for(&drain, &task)
    });
    let approval = workspace
        .runtime
        .get_task_history(&task.id)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "proposal_approved")
        .expect("the drain approves the explicit assessment");
    assert!(
        approval
            .note
            .as_deref()
            .unwrap_or_default()
            .starts_with(&format!(
                "approved by auto drain {drain} after task-pilot verification"
            )),
        "{approval:?}"
    );
    assert_eq!(
        approval.to_status,
        Some(TaskStatus::Backlog),
        "{approval:?}"
    );

    workspace.settle_leaf(&leaf, &task);
    workspace
        .runtime
        .stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
            actor: "operator",
            source: "fixture",
            reason: None,
            claim_token: None,
            force: false,
        })
        .unwrap();
    executor.join().unwrap().unwrap();
    stop.store(true, Ordering::SeqCst);
    let drain_pilots = drain_pilots.join().unwrap();
    // Reap every substitute (explicit pilot, drain and dispatched children)
    // before the release marker and temporary root are removed.
    let mut expected_workers = vec![explicit.clone(), drain.clone()];
    expected_workers.extend(
        workspace
            .runtime
            .read_run_state(&drain)
            .unwrap()
            .unwrap()
            .child_dispatches
            .iter()
            .map(|child| child.child_run_id.clone()),
    );
    owned_workers.release_and_wait(&expected_workers);
    assert_eq!(workspace.run(&drain).state, JobRunState::Success);

    // Exactly one provider invocation and one applied assessment. A pilot
    // the drain dispatched for the task stood down without the provider.
    assert_eq!(provider_calls(&calls), 1, "drain pilots: {drain_pilots:?}");
    assert_eq!(workspace.pilot_applications(&task), 1);
    for pilot in &drain_pilots {
        let prepare = prepared(&workspace, pilot);
        assert_eq!(prepare["task_ids"], json!([]), "{pilot}: {prepare}");
        let excluded = &prepare["excluded_by_reason"];
        assert!(
            excluded["already_preparing"] == 1 || excluded["assessment_already_applied"] == 1,
            "{pilot}: {prepare}"
        );
    }
    // The pass that saw the operator's pilot held the task instead of
    // dispatching a child that could only stand down.
    let held = first_pass
        .held
        .iter()
        .find(|held| held.task_id == task.id)
        .unwrap_or_else(|| panic!("the first pass held the task: {first_pass:?}"));
    assert_eq!(held.reason.as_deref(), Some("pilot_running"), "{held:?}");
    let report = workspace
        .runtime
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_approvals
        .unwrap();
    assert_eq!(report.approved, vec![task.id.clone()]);
    assert_eq!(report.approved_total, 1);
}

#[test]
fn certified_explicit_assessment_with_done_foreign_prerequisite_is_reused() {
    if !crate::dispatch_admission::isolated(
        "drain_approval::explicit_pilot_reuse::certified_explicit_assessment_with_done_foreign_prerequisite_is_reused",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let done_foreign = workspace.foreign_task("done foreign prerequisite", TaskStatus::Done);
    let unfinished_foreign =
        workspace.foreign_task("unfinished foreign prerequisite", TaskStatus::Backlog);
    // The foreign tasks are registered, not local to this workspace.
    assert!(workspace.runtime.get_task(&done_foreign.id).is_err());
    assert!(workspace.runtime.get_task(&unfinished_foreign.id).is_err());

    let done_task = workspace.task(
        "done foreign dependent",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let unfinished_task = workspace.task(
        "unfinished foreign dependent",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let missing_task = workspace.task(
        "missing foreign dependent",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    // No stored dependency edges: the prerequisites exist only in pilot prose.
    for task in [&done_task, &unfinished_task, &missing_task] {
        assert!(
            workspace
                .runtime
                .get_task(&task.id)
                .unwrap()
                .dependencies()
                .is_empty(),
            "{}",
            task.id
        );
    }

    let drain = workspace.running(
        "workspace_auto_pipeline",
        json!({"approve_proposed": true, "for_seconds": 3600}),
    );

    // A certified explicit pilot for each task, naming its prerequisite in
    // `blocked_by` without a stored edge.
    for (task, prerequisite) in [
        (&done_task, done_foreign.id.as_str()),
        (&unfinished_task, unfinished_foreign.id.as_str()),
        (&missing_task, "ORB-999999999"),
    ] {
        let explicit = workspace.running("task_pilot_pipeline", json!({"task_ids": [task.id]}));
        workspace.set_trigger(&explicit, JobRunTrigger::cli());
        let prepared = workspace.prepare_named(&explicit, std::slice::from_ref(&task.id));
        assert_eq!(prepared["task_count"], 1, "{prepared}");
        let mut assessed = assessment(task);
        assessed["blocked_by"] = json!([format!("Wait for {prerequisite} to land")]);
        let applied = workspace.apply_unpromoted_assessment(&explicit, &prepared, assessed);
        assert_eq!(applied["status"], "succeeded", "{applied}");
        assert_eq!(applied["applied_count"], 1, "{applied}");
        workspace.finish(&explicit, JobRunState::Success);
        assert_eq!(workspace.status(task), TaskStatus::Proposed);
        assert_eq!(workspace.pilot_applications(task), 1);
    }

    // Recording through the public boundary names only the done task as
    // awaiting reuse; the others stay held as warnings.
    let recorded = workspace.record(&drain);
    assert_eq!(
        recorded["awaiting_reuse"],
        json!([done_task.id]),
        "{recorded}"
    );

    // Selection promotes the done task's certified assessment without a
    // second pilot; missing and unfinished prerequisites still hold, with
    // their findings shown (not an empty findings array).
    let selection = workspace.select(&drain);
    assert_eq!(selection["reused"], json!([done_task.id]), "{selection}");
    assert_eq!(workspace.status(&done_task), TaskStatus::Backlog);
    assert!(held_reason(&selection, &done_task).is_none(), "{selection}");
    for task in [&unfinished_task, &missing_task] {
        assert_eq!(workspace.status(task), TaskStatus::Proposed);
        assert_eq!(
            held_reason(&selection, task),
            Some("warnings"),
            "{selection}"
        );
        let held = selection["held"]
            .as_array()
            .unwrap()
            .iter()
            .find(|held| held["task_id"] == task.id.as_str())
            .unwrap();
        assert_eq!(held["findings"].as_array().unwrap().len(), 1, "{held}");
    }
    assert_eq!(workspace.pilot_applications(&done_task), 1);
}
