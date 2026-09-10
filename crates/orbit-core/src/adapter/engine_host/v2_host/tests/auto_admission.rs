//! Conflict-aware admission selection [ORB-11973].
//!
//! The unit tests drive `select_admissions` directly so the selection rule is
//! exercised at the boundary that owns it; the end-to-end tests then confirm
//! the classifier and readiness both reach that rule with the same pool.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::auto_admission::{
    AdmissionHolders, ConflictProvenance, select_admissions,
};
use crate::adapter::engine_host::v2_host::backlog_exclusion::backlog_snapshot;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, seed_list_backlog_task, write_workspace_file,
};
use crate::application::job::crew_pools::CapturedCrewPools;
use crate::application::task::TaskUpdateParams;

fn task_lookup(runtime: &OrbitRuntime) -> BTreeMap<String, Task> {
    runtime
        .list_tasks()
        .expect("list tasks")
        .into_iter()
        .map(|task| (task.id.clone(), task))
        .collect()
}

fn lock_holders(runtime: &OrbitRuntime) -> BTreeMap<String, Vec<String>> {
    backlog_snapshot(runtime, "test", None, &CapturedCrewPools::new())
        .expect("backlog snapshot")
        .lock_holders
}

/// The snapshot's own priority/age ordering of the admissible leaves, which is
/// the order the classifier and readiness both hand to the selection routine.
fn ordered_candidates(runtime: &OrbitRuntime) -> Vec<String> {
    backlog_snapshot(runtime, "test", None, &CapturedCrewPools::new())
        .expect("backlog snapshot")
        .admissible_leaves
        .into_iter()
        .map(|task| task.id)
        .collect()
}

fn backlog_task(
    runtime: &OrbitRuntime,
    repo_root: &Path,
    title: &str,
    priority: TaskPriority,
    context_files: Vec<&str>,
) -> Task {
    for selector in &context_files {
        if let Some(path) = selector.strip_prefix("file:") {
            write_workspace_file(repo_root, path);
        }
    }
    seed_list_backlog_task(
        runtime,
        title,
        TaskStatus::Backlog,
        priority,
        TaskType::Chore,
        None,
        context_files,
    )
}

fn classify_with(runtime: &OrbitRuntime, input: Value) -> Value {
    runtime
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("classify workspace auto tasks")
}

fn readiness_task<'a>(output: &'a Value, task_id: &str) -> &'a Value {
    output["tasks"]
        .as_array()
        .expect("readiness tasks")
        .iter()
        .find(|task| task["task_id"] == task_id)
        .expect("readiness task")
}

fn string_ids(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array of task ids")
        .iter()
        .filter_map(|entry| entry.as_str().map(ToOwned::to_owned))
        .collect()
}

/// The F2026-09-104 shape: a run of overlapping high-priority tasks at the head
/// of the queue used to consume every slot and then serialize at the gate. The
/// wave must take one of them and walk on to the independent work behind them.
#[test]
fn a_ten_slot_wave_walks_past_a_conflicting_cluster_to_independent_work() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let cluster = (0..6)
        .map(|index| {
            backlog_task(
                &runtime,
                &repo_root,
                &format!("Cluster {index}"),
                TaskPriority::High,
                vec!["file:crates/hot/src/lib.rs"],
            )
        })
        .collect::<Vec<_>>();
    let independent = (0..12)
        .map(|index| {
            let selector = format!("file:crates/independent/src/leaf_{index}.rs");
            backlog_task(
                &runtime,
                &repo_root,
                &format!("Independent {index}"),
                TaskPriority::Medium,
                vec![selector.as_str()],
            )
        })
        .collect::<Vec<_>>();

    let selection = select_admissions(
        &ordered_candidates(&runtime),
        &task_lookup(&runtime),
        &repo_root,
        &AdmissionHolders::default(),
        10,
    );

    assert_eq!(selection.selected.len(), 10, "every free slot is filled");
    let mut expected = vec![cluster[0].id.clone()];
    expected.extend(independent[..9].iter().map(|task| task.id.clone()));
    assert_eq!(
        selection.selected, expected,
        "the highest-priority member of the cluster keeps its place and the \
         remaining slots go to the next compatible candidates in order"
    );
    assert_eq!(
        selection
            .deferred
            .iter()
            .map(|deferred| deferred.task_id.clone())
            .collect::<Vec<_>>(),
        cluster[1..]
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>(),
        "only the cluster members that would collide are deferred"
    );
    for deferred in &selection.deferred {
        assert_eq!(deferred.blocking_task_ids(), vec![cluster[0].id.clone()]);
        assert!(
            deferred
                .conflicts
                .iter()
                .all(|conflict| conflict.provenance == ConflictProvenance::SameWave),
            "a task selected earlier this wave holds no lock yet"
        );
    }
}

/// Every selected pair must actually be admissible together, not merely
/// distinct: the point of the wave is that no two members reach the gate on the
/// same footprint.
#[test]
fn a_wave_is_pairwise_nonconflicting_across_shared_directories() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/shared/src/lib.rs");
    write_workspace_file(&repo_root, "crates/shared/src/other.rs");
    let directory = backlog_task(
        &runtime,
        &repo_root,
        "Directory scope",
        TaskPriority::High,
        vec!["dir:crates/shared"],
    );
    let inside = backlog_task(
        &runtime,
        &repo_root,
        "File beneath the directory",
        TaskPriority::Medium,
        vec!["file:crates/shared/src/lib.rs"],
    );
    let symbol = seed_list_backlog_task(
        &runtime,
        "Symbol in the same file",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["symbol:crates/shared/src/other.rs#helper:function"],
    );
    let outside = backlog_task(
        &runtime,
        &repo_root,
        "Unrelated crate",
        TaskPriority::Low,
        vec!["file:crates/other/src/lib.rs"],
    );

    let selection = select_admissions(
        &ordered_candidates(&runtime),
        &task_lookup(&runtime),
        &repo_root,
        &AdmissionHolders::default(),
        4,
    );

    assert_eq!(
        selection.selected,
        vec![directory.id.clone(), outside.id.clone()],
        "a `dir:` scope covers both the file and the symbol beneath it"
    );
    let deferred = selection
        .deferred
        .iter()
        .map(|deferred| deferred.task_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(deferred, vec![inside.id.clone(), symbol.id.clone()]);
    let symbol_conflict = selection
        .deferred_for(&symbol.id)
        .expect("symbol candidate is deferred");
    assert_eq!(
        symbol_conflict.conflicts[0].blocking_selector, "dir:crates/shared",
        "the deferral names the selector that actually covers it"
    );
    assert_eq!(
        symbol_conflict.conflicts[0].requested_selector,
        "symbol:crates/shared/src/other.rs#helper:function"
    );
}

/// An epic root reserves the union of its descendants' context files, so a leaf
/// overlapping any descendant must not join the same wave as the root.
#[test]
fn an_epic_roots_descendant_coverage_defers_an_overlapping_leaf() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/epic/src/child.rs");
    let epic = seed_list_backlog_task(
        &runtime,
        "Epic root",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Feature,
        None,
        vec![],
    );
    runtime
        .update_task(
            &epic.id,
            TaskUpdateParams {
                tags: Some(vec!["epic".to_string()]),
                ..Default::default()
            },
        )
        .expect("tag epic root");
    seed_list_backlog_task(
        &runtime,
        "Epic descendant",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        Some(epic.id.clone()),
        vec!["file:crates/epic/src/child.rs"],
    );
    let overlapping_leaf = backlog_task(
        &runtime,
        &repo_root,
        "Leaf touching the descendant's file",
        TaskPriority::Medium,
        vec!["file:crates/epic/src/child.rs"],
    );

    // The epic family is excluded from the leaf population, so the leaf is the
    // only candidate; the root's reservation reaches it through the holder set.
    let lookup = task_lookup(&runtime);
    let holders = AdmissionHolders::new(
        &BTreeMap::new(),
        &BTreeSet::from([epic.id.clone()]),
        &lookup,
        &repo_root,
    );
    let selection = select_admissions(
        std::slice::from_ref(&overlapping_leaf.id),
        &lookup,
        &repo_root,
        &holders,
        5,
    );

    assert!(selection.selected.is_empty());
    let deferred = selection
        .deferred_for(&overlapping_leaf.id)
        .expect("overlapping leaf is deferred");
    assert_eq!(deferred.blocking_task_ids(), vec![epic.id.clone()]);
    assert_eq!(
        deferred.conflicts[0].provenance,
        ConflictProvenance::LiveClaim
    );
}

/// A task a live wrapper is carrying is still `backlog`, so its footprint is
/// invisible to the task-status lock holders. Admitting a leaf that overlaps it
/// only queues a second waiter on the same footprint at the gate.
#[test]
fn a_live_wrapper_claim_defers_an_overlapping_candidate_before_the_gate() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let claimed = backlog_task(
        &runtime,
        &repo_root,
        "Carried by a live wrapper",
        TaskPriority::High,
        vec!["file:crates/contended/src/lib.rs"],
    );
    let overlapping = backlog_task(
        &runtime,
        &repo_root,
        "Overlaps the carried task",
        TaskPriority::Medium,
        vec!["file:crates/contended/src/lib.rs"],
    );
    let independent = backlog_task(
        &runtime,
        &repo_root,
        "Independent",
        TaskPriority::Medium,
        vec!["file:crates/free/src/lib.rs"],
    );

    let lookup = task_lookup(&runtime);
    let holders = AdmissionHolders::new(
        &BTreeMap::new(),
        &BTreeSet::from([claimed.id.clone()]),
        &lookup,
        &repo_root,
    );
    let selection = select_admissions(
        &[overlapping.id.clone(), independent.id.clone()],
        &lookup,
        &repo_root,
        &holders,
        5,
    );

    assert_eq!(selection.selected, vec![independent.id.clone()]);
    let deferred = selection
        .deferred_for(&overlapping.id)
        .expect("overlapping candidate is deferred");
    assert_eq!(deferred.blocking_task_ids(), vec![claimed.id.clone()]);
    assert_eq!(
        deferred.conflicts[0].provenance,
        ConflictProvenance::LiveClaim,
        "a claim is not a held lock, and the remedy differs"
    );
}

/// A claimed task that has reached `in-progress` holds a real lock. The
/// deferral must say so rather than describing the same footprint twice.
#[test]
fn an_in_progress_holder_defers_with_held_lock_provenance() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let holder = backlog_task(
        &runtime,
        &repo_root,
        "Already implementing",
        TaskPriority::High,
        vec!["file:crates/contended/src/lib.rs"],
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
    let overlapping = backlog_task(
        &runtime,
        &repo_root,
        "Overlaps the holder",
        TaskPriority::Medium,
        vec!["file:crates/contended/src/lib.rs"],
    );

    let lookup = task_lookup(&runtime);
    let holders = AdmissionHolders::new(
        &lock_holders(&runtime),
        &BTreeSet::from([holder.id.clone()]),
        &lookup,
        &repo_root,
    );
    let selection = select_admissions(
        std::slice::from_ref(&overlapping.id),
        &lookup,
        &repo_root,
        &holders,
        5,
    );

    let deferred = selection
        .deferred_for(&overlapping.id)
        .expect("overlapping candidate is deferred");
    assert_eq!(deferred.blocking_task_ids(), vec![holder.id.clone()]);
    assert_eq!(
        deferred
            .conflicts
            .iter()
            .map(|conflict| conflict.provenance)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([ConflictProvenance::HeldLock]),
        "an in-progress claim is reported once, as the held lock it is"
    );
}

/// Candidates the wave never reached are saturated, not conflicted. Collapsing
/// the two would tell an operator to clear locks that are not the problem.
#[test]
fn candidates_past_a_full_wave_are_saturated_rather_than_conflicted() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let seeded = (0..5)
        .map(|index| {
            let selector = format!("file:crates/independent/src/leaf_{index}.rs");
            backlog_task(
                &runtime,
                &repo_root,
                &format!("Independent {index}"),
                TaskPriority::Medium,
                vec![selector.as_str()],
            )
        })
        .collect::<Vec<_>>();

    let selection = select_admissions(
        &ordered_candidates(&runtime),
        &task_lookup(&runtime),
        &repo_root,
        &AdmissionHolders::default(),
        2,
    );

    assert_eq!(
        selection.selected,
        vec![seeded[0].id.clone(), seeded[1].id.clone()]
    );
    assert!(selection.deferred.is_empty(), "nothing collided");
    assert_eq!(
        selection.queued_behind_capacity,
        seeded[2..]
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>()
    );
}

/// A zero-slot wave does no lock work at all: everything is behind capacity.
#[test]
fn a_saturated_drain_examines_no_candidate_for_conflicts() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let first = backlog_task(
        &runtime,
        &repo_root,
        "First",
        TaskPriority::High,
        vec!["file:crates/hot/src/lib.rs"],
    );
    let second = backlog_task(
        &runtime,
        &repo_root,
        "Second on the same file",
        TaskPriority::Medium,
        vec!["file:crates/hot/src/lib.rs"],
    );

    let selection = select_admissions(
        &[first.id.clone(), second.id.clone()],
        &task_lookup(&runtime),
        &repo_root,
        &AdmissionHolders::default(),
        0,
    );

    assert!(selection.selected.is_empty());
    assert!(selection.deferred.is_empty());
    assert_eq!(selection.queued_behind_capacity, vec![first.id, second.id]);
}

/// The classifier and readiness must not be able to disagree about which task
/// takes a slot, because an operator uses one to predict the other.
#[test]
fn the_classifier_and_readiness_select_the_same_tasks_for_one_snapshot() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    for index in 0..4 {
        backlog_task(
            &runtime,
            &repo_root,
            &format!("Cluster {index}"),
            TaskPriority::High,
            vec!["file:crates/hot/src/lib.rs"],
        );
    }
    for index in 0..4 {
        let selector = format!("file:crates/independent/src/leaf_{index}.rs");
        backlog_task(
            &runtime,
            &repo_root,
            &format!("Independent {index}"),
            TaskPriority::Medium,
            vec![selector.as_str()],
        );
    }

    let classified = classify_with(&runtime, json!({ "max_active_leaf_runs": 3 }));
    let readiness = runtime
        .workspace_auto_readiness(&[], Some(3), 50, &[])
        .expect("explain readiness");
    let eligible = readiness["tasks"]
        .as_array()
        .expect("readiness tasks")
        .iter()
        .filter(|task| task["eligible"] == json!(true))
        .map(|task| task["task_id"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();

    assert_eq!(string_ids(&classified["loose_task_ids"]).len(), 3);
    assert_eq!(
        string_ids(&classified["loose_task_ids"])
            .into_iter()
            .collect::<BTreeSet<_>>(),
        eligible.into_iter().collect::<BTreeSet<_>>(),
        "one selection routine, one answer"
    );
    assert_eq!(
        classified["deferred_conflicts"], readiness["capacity"]["deferred_conflicts"],
        "and one explanation for the tasks it passed over"
    );
}

/// Readiness names the blocker and the overlapping selectors so an operator can
/// tell a lock problem from a capacity problem without reading the code.
#[test]
fn readiness_explains_a_deferred_conflict_and_clears_it_when_the_blocker_goes() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let first = backlog_task(
        &runtime,
        &repo_root,
        "First on the hot file",
        TaskPriority::High,
        vec!["file:crates/hot/src/lib.rs"],
    );
    let second = backlog_task(
        &runtime,
        &repo_root,
        "Second on the hot file",
        TaskPriority::Medium,
        vec!["file:crates/hot/src/lib.rs"],
    );

    let blocked = runtime
        .workspace_auto_readiness(&[], Some(5), 50, &[])
        .expect("explain readiness");
    let entry = readiness_task(&blocked, &second.id);
    assert_eq!(entry["eligible"], json!(false));
    assert_eq!(entry["reason"], "conflict_deferred");
    assert_eq!(entry["blocking_task_ids"], json!([first.id]));
    assert_eq!(
        entry["conflicts"][0]["requested_selector"],
        "file:crates/hot/src/lib.rs"
    );
    assert_eq!(entry["conflicts"][0]["provenance"], "same_wave");
    assert_eq!(
        readiness_task(&blocked, &first.id)["reason"],
        "ready",
        "the blocker itself still starts"
    );

    // The deferral is not a permanent exclusion: once the blocker leaves the
    // population the candidate becomes eligible on the very next snapshot.
    runtime
        .update_task(
            &first.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .expect("finish the blocker");
    let cleared = runtime
        .workspace_auto_readiness(&[], Some(5), 50, &[])
        .expect("explain readiness");
    assert_eq!(readiness_task(&cleared, &second.id)["reason"], "ready");
    assert_eq!(
        readiness_task(&cleared, &second.id)["eligible"],
        json!(true)
    );
    assert_eq!(cleared["capacity"]["deferred_conflicts"], json!([]));
}

/// `max_tasks` bounds how much backlog one iteration expands footprints for. A
/// prefix of conflicting candidates must not make that bound look like an empty
/// backlog: the short wave says the pool was truncated.
#[test]
fn a_truncated_candidate_pool_is_reported_when_conflicts_exhaust_it() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    for index in 0..4 {
        backlog_task(
            &runtime,
            &repo_root,
            &format!("Cluster {index}"),
            TaskPriority::High,
            vec!["file:crates/hot/src/lib.rs"],
        );
    }
    let independent = backlog_task(
        &runtime,
        &repo_root,
        "Independent behind the cluster",
        TaskPriority::Medium,
        vec!["file:crates/independent/src/lib.rs"],
    );

    let truncated = classify_with(
        &runtime,
        json!({ "max_active_leaf_runs": 5, "max_tasks": 3 }),
    );
    assert_eq!(truncated["candidate_pool_size"], json!(3));
    assert_eq!(truncated["candidate_pool_truncated"], json!(true));
    assert_eq!(string_ids(&truncated["loose_task_ids"]).len(), 1);

    // With the whole pool examined the independent task behind the cluster is
    // reached, and the truncation flag goes back down.
    let whole = classify_with(&runtime, json!({ "max_active_leaf_runs": 5 }));
    assert_eq!(
        string_ids(&whole["loose_task_ids"]).len(),
        2,
        "the cluster costs one slot, not the whole wave"
    );
    assert!(string_ids(&whole["loose_task_ids"]).contains(&independent.id));
    assert_eq!(whole["candidate_pool_truncated"], json!(false));
}
