//! What an occupied leaf slot is doing, from durable run state [ORB-11973].
//!
//! The fixtures reproduce the F2026-09-104 shape with a temporary store rather
//! than replaying the historical drain: ten wrappers at concurrency ten, five
//! parked in `task_gate_pipeline` before lock acquisition, four with a live
//! implementation run, and one past its implementation.

use std::collections::BTreeSet;

use chrono::Utc;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{ChildDispatch, ChildDispatchPhase, PipelineState};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, seed_list_backlog_task, write_workspace_file,
};
use crate::application::task::TaskUpdateParams;

/// A live wrapper run carrying `task_ids`, as `invoke_detached` leaves one.
fn seed_wrapper_run(runtime: &OrbitRuntime, task_ids: &[&str]) -> String {
    seed_run(
        runtime,
        "task_auto_pipeline",
        json!({ "task_ids": task_ids }),
    )
}

fn seed_run(runtime: &OrbitRuntime, job_name: &str, input: Value) -> String {
    runtime
        .stores()
        .jobs()
        .insert_job_run(job_name, 1, Utc::now(), Some(input), None)
        .expect("insert job run")
        .run_id
}

fn dispatch(child_run_id: &str, job_name: &str, phase: ChildDispatchPhase) -> ChildDispatch {
    let mut dispatch = ChildDispatch::submitted(
        child_run_id.to_string(),
        job_name.to_string(),
        "invoke_and_wait".to_string(),
        true,
        false,
        Utc::now(),
    );
    dispatch.phase = phase;
    if phase == ChildDispatchPhase::Terminal {
        dispatch.child_status = Some("succeeded".to_string());
    }
    dispatch
}

fn write_state(
    runtime: &OrbitRuntime,
    run_id: &str,
    job_id: &str,
    dispatches: Vec<ChildDispatch>,
    waiting_on_locks: Option<Vec<String>>,
) {
    let mut state = PipelineState::new(run_id.to_string(), job_id.to_string(), json!({}));
    state.child_dispatches = dispatches;
    state.waiting_on_locks = waiting_on_locks;
    runtime
        .write_run_state(run_id, &state)
        .expect("write run state");
}

/// One wrapper parked at its gate, with the selectors the gate recorded.
fn seed_lock_waiting_wrapper(
    runtime: &OrbitRuntime,
    task_id: &str,
    waiting_on_locks: &[&str],
) -> (String, String) {
    let gate_run_id = seed_run(
        runtime,
        "task_gate_pipeline",
        json!({ "task_ids": [task_id] }),
    );
    let wrapper_run_id = seed_wrapper_run(runtime, &[task_id]);
    write_state(
        runtime,
        &wrapper_run_id,
        "task_auto_pipeline",
        vec![dispatch(
            &gate_run_id,
            "task_gate_pipeline",
            ChildDispatchPhase::Waiting,
        )],
        None,
    );
    write_state(
        runtime,
        &gate_run_id,
        "task_gate_pipeline",
        Vec::new(),
        Some(
            waiting_on_locks
                .iter()
                .map(|selector| (*selector).to_string())
                .collect(),
        ),
    );
    (wrapper_run_id, gate_run_id)
}

/// One wrapper whose gate has reserved and dispatched. `implementation_phase`
/// separates a live implementation run from one the gate has already joined.
fn seed_dispatched_wrapper(
    runtime: &OrbitRuntime,
    task_id: &str,
    implementation_phase: ChildDispatchPhase,
) -> (String, String, String) {
    let implementation_run_id = seed_run(
        runtime,
        "task_pr_pipeline",
        json!({ "task_ids": [task_id] }),
    );
    let gate_run_id = seed_run(
        runtime,
        "task_gate_pipeline",
        json!({ "task_ids": [task_id] }),
    );
    let wrapper_run_id = seed_wrapper_run(runtime, &[task_id]);
    write_state(
        runtime,
        &wrapper_run_id,
        "task_auto_pipeline",
        vec![dispatch(
            &gate_run_id,
            "task_gate_pipeline",
            ChildDispatchPhase::Waiting,
        )],
        None,
    );
    write_state(
        runtime,
        &gate_run_id,
        "task_gate_pipeline",
        vec![dispatch(
            &implementation_run_id,
            "task_pr_pipeline",
            implementation_phase,
        )],
        None,
    );
    (wrapper_run_id, gate_run_id, implementation_run_id)
}

fn occupancy_run<'a>(readiness: &'a Value, run_id: &str) -> &'a Value {
    readiness["capacity"]["occupancy"]["runs"]
        .as_array()
        .expect("occupancy runs")
        .iter()
        .find(|run| run["run_id"] == run_id)
        .expect("occupancy entry for run")
}

fn descendant_ids(entry: &Value) -> BTreeSet<String> {
    entry["descendant_run_ids"]
        .as_array()
        .expect("descendant run ids")
        .iter()
        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
        .collect()
}

fn readiness(runtime: &OrbitRuntime, concurrency: u32) -> Value {
    runtime
        .workspace_auto_readiness(&[], Some(concurrency), 50, &[])
        .expect("explain readiness")
}

/// The incident shape. Ten occupied slots and no free ones read identically
/// before this change; the phase split is the only thing that says half the
/// drain is queued on locks rather than working.
#[test]
fn a_saturated_drain_reports_lock_waiting_implementing_and_post_implementation_slots() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/held/src/lib.rs");
    let holder = seed_list_backlog_task(
        &runtime,
        "Holds the contended file",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["file:crates/held/src/lib.rs"],
    );
    runtime
        .update_task(
            &holder.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .expect("move holder to in-progress");

    let carried = (0..10)
        .map(|index| {
            seed_list_backlog_task(
                &runtime,
                &format!("Carried leaf {index}"),
                TaskStatus::Backlog,
                TaskPriority::Medium,
                TaskType::Chore,
                None,
                vec![],
            )
            .id
        })
        .collect::<Vec<_>>();

    // Three gates park on a selector a live holder explains; two park on one no
    // current holder does, which must be reported rather than dropped.
    let resolved = (0..3)
        .map(|index| {
            seed_lock_waiting_wrapper(&runtime, &carried[index], &["crates/held/src/lib.rs"])
        })
        .collect::<Vec<_>>();
    let unresolved = (3..5)
        .map(|index| {
            seed_lock_waiting_wrapper(&runtime, &carried[index], &["crates/departed/src/lib.rs"])
        })
        .collect::<Vec<_>>();
    let implementing = (5..9)
        .map(|index| {
            seed_dispatched_wrapper(&runtime, &carried[index], ChildDispatchPhase::Waiting)
        })
        .collect::<Vec<_>>();
    let syncing = seed_dispatched_wrapper(&runtime, &carried[9], ChildDispatchPhase::Terminal);

    let readiness = readiness(&runtime, 10);
    let capacity = &readiness["capacity"];
    let occupancy = &capacity["occupancy"];

    assert_eq!(capacity["active_leaf_runs"], json!(10));
    assert_eq!(capacity["free_slots"], json!(0));
    assert_eq!(occupancy["active_leaf_runs"], json!(10));
    assert_eq!(
        occupancy["free_slots"], capacity["free_slots"],
        "the breakdown cannot disagree with the capacity beside it"
    );
    assert_eq!(
        occupancy["phases"],
        json!({
            "lock_waiting": 5,
            "implementing": 4,
            "post_implementation": 1,
            "unknown": 0,
        }),
        "five slots hold a slot without holding a lock"
    );
    assert_eq!(
        occupancy["runs"].as_array().map(Vec::len),
        Some(10),
        "one entry per occupied slot, and no slot counted twice"
    );
    assert_eq!(
        occupancy["task_ids"],
        json!(carried.iter().cloned().collect::<BTreeSet<_>>()),
        "ten distinct carried tasks, deduplicated"
    );

    let (wrapper, gate) = &resolved[0];
    let entry = occupancy_run(&readiness, wrapper);
    assert_eq!(entry["phase"], "lock_waiting");
    assert_eq!(descendant_ids(entry), BTreeSet::from([gate.clone()]));
    assert_eq!(entry["lock_wait"]["gate_run_id"], json!(gate));
    assert_eq!(
        entry["lock_wait"]["blockers"],
        json!([{
            "selector": "crates/held/src/lib.rs",
            "holder_task_ids": [holder.id],
        }]),
        "an available blocker is named"
    );
    assert_eq!(entry["lock_wait"]["unresolved_selectors"], json!([]));

    let (wrapper, _) = &unresolved[0];
    let entry = occupancy_run(&readiness, wrapper);
    assert_eq!(entry["phase"], "lock_waiting");
    assert_eq!(entry["lock_wait"]["blockers"], json!([]));
    assert_eq!(
        entry["lock_wait"]["unresolved_selectors"],
        json!(["crates/departed/src/lib.rs"]),
        "a wait no current holder explains stays visible as missing evidence"
    );

    let (wrapper, gate, implementation) = &implementing[0];
    let entry = occupancy_run(&readiness, wrapper);
    assert_eq!(entry["phase"], "implementing");
    assert_eq!(
        descendant_ids(entry),
        BTreeSet::from([gate.clone(), implementation.clone()]),
        "the gate and the implementation run it dispatched are both named"
    );
    assert_eq!(entry["lock_wait"], Value::Null);

    let (wrapper, gate, implementation) = &syncing;
    let entry = occupancy_run(&readiness, wrapper);
    assert_eq!(
        entry["phase"], "post_implementation",
        "a joined implementation run leaves the slot doing sync/teardown work"
    );
    assert_eq!(
        descendant_ids(entry),
        BTreeSet::from([gate.clone(), implementation.clone()])
    );
}

/// A phase the stores cannot support is reported as unknown with the reason.
/// Guessing here would put a made-up number in the diagnostic an operator uses
/// to decide whether to intervene.
#[test]
fn a_slot_without_durable_evidence_reports_unknown_and_says_why() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let stateless = seed_wrapper_run(&runtime, &["ORB-STATELESS"]);
    let no_gate = seed_wrapper_run(&runtime, &["ORB-NO-GATE"]);
    write_state(&runtime, &no_gate, "task_auto_pipeline", Vec::new(), None);
    let silent_gate_run = seed_run(&runtime, "task_gate_pipeline", json!({}));
    let silent_gate = seed_wrapper_run(&runtime, &["ORB-SILENT-GATE"]);
    write_state(
        &runtime,
        &silent_gate,
        "task_auto_pipeline",
        vec![dispatch(
            &silent_gate_run,
            "task_gate_pipeline",
            ChildDispatchPhase::Waiting,
        )],
        None,
    );
    write_state(
        &runtime,
        &silent_gate_run,
        "task_gate_pipeline",
        Vec::new(),
        None,
    );

    let readiness = readiness(&runtime, 5);
    assert_eq!(
        readiness["capacity"]["occupancy"]["phases"]["unknown"],
        json!(3)
    );
    assert_eq!(
        occupancy_run(&readiness, &stateless)["unknown_reason"],
        "no readable pipeline state for this leaf run"
    );
    assert_eq!(
        occupancy_run(&readiness, &no_gate)["unknown_reason"],
        "leaf run has not recorded a gate dispatch yet"
    );
    assert_eq!(
        occupancy_run(&readiness, &silent_gate)["unknown_reason"],
        "gate run recorded neither a lock wait nor a dispatch"
    );
}

/// Slots and tasks are counted on different axes: two wrappers carrying one
/// task occupy two slots, and the task roll-up still names it once.
#[test]
fn two_wrappers_carrying_one_task_occupy_two_slots_and_name_it_once() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let first = seed_wrapper_run(&runtime, &["ORB-SHARED"]);
    let second = seed_wrapper_run(&runtime, &["ORB-SHARED"]);

    let readiness = readiness(&runtime, 5);
    let occupancy = &readiness["capacity"]["occupancy"];
    assert_eq!(occupancy["active_leaf_runs"], json!(2));
    assert_eq!(occupancy["free_slots"], json!(3));
    assert_eq!(occupancy["task_ids"], json!(["ORB-SHARED"]));
    assert_ne!(first, second);
    assert_eq!(occupancy["runs"].as_array().map(Vec::len), Some(2));
}

/// Readiness reads; it must not reconcile, reserve, or move anything. The
/// occupancy walk added store reads, so re-assert the whole snapshot is inert.
#[test]
fn reading_occupancy_mutates_no_task_run_or_lock() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/held/src/lib.rs");
    let backlog = seed_list_backlog_task(
        &runtime,
        "Waiting leaf",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["file:crates/held/src/lib.rs"],
    );
    let (wrapper, gate) =
        seed_lock_waiting_wrapper(&runtime, "ORB-CARRIED", &["crates/held/src/lib.rs"]);

    let tasks_before = runtime.list_tasks().expect("list tasks");
    let runs_before = runtime
        .stores()
        .jobs()
        .list_pending_or_running_job_runs("task_auto_pipeline")
        .expect("list wrapper runs");
    let gate_state_before = runtime.read_run_state(&gate).expect("read gate state");
    let locks_before = crate::runtime::task::locks::list(&runtime).expect("list task locks");

    let _ = readiness(&runtime, 5);

    assert_eq!(runtime.list_tasks().expect("list tasks"), tasks_before);
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .list_pending_or_running_job_runs("task_auto_pipeline")
            .expect("list wrapper runs")
            .len(),
        runs_before.len()
    );
    assert_eq!(
        runtime.read_run_state(&gate).expect("read gate state"),
        gate_state_before
    );
    assert_eq!(
        crate::runtime::task::locks::list(&runtime).expect("list task locks"),
        locks_before
    );
    assert_eq!(
        runtime.get_task(&backlog.id).expect("read task").status,
        TaskStatus::Backlog
    );
    assert!(!wrapper.is_empty());
}
