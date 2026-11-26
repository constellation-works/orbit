//! An `--auto-fix` owner drain pass hands a conflicting or red Orbit pull
//! request whose task has no live run back to its delivery: it resumes the
//! task's newest stopped delivery leaf at its completion step, with
//! `auto_fix` and deferred footprint locks granted, under the leaf's own
//! `completion`, and restores the task to `review` so that step completes the
//! existing pull request. Operator holds, `no-auto-approve`, leaves started
//! with `--complete` under a drain without `--complete`, pull requests not
//! opened by Orbit and drains without `--auto-fix` are never repaired, and
//! the pull request's auto-fix budget bounds the hand-backs.
//!
//! The pass runs as the drain's `adopt_orphaned_deliveries` step against the
//! fake forge. A resumed leaf starts the fixture's substitute worker. The
//! refresh regression also drives the resumed completion action against its
//! published checkpoints.

#[cfg(unix)]
mod refreshed_head;
mod support;

use super::*;
use orbit_core::application::job::TaskAutoFixError;
use orbit_types::workflow::{DeliveryRefusal, PR_AUTO_FIX_ADMITTED_EVENT};
use support::*;

pub(super) use support::{LeafEnd, Shelf, fixture, repair_skip, started};

/// Records `rounds` as the checkpoints of steps 4.. as production writes them:
/// each is a Success, and a step that failed or never ran has no entry.
fn checkpoint_rounds(fx: &Fixture, leaf: &Leaf, rounds: &[(&str, Value)]) {
    let mut state = fx.state(&leaf.run);
    for (index, (id, output)) in (4..).zip(rounds) {
        state.record_step(index, JobRunState::Success, Some(output.clone()), None);
        state.record_pipeline_output(id, output.clone());
    }
    fx.jobs.write_run_state(&leaf.run, &state).unwrap();
}

/// Checkpoints `rounds` on `leaf`, repairs it at `head`, and asserts that the
/// resumed run reuses steps `0..reused` and reruns from step `reused`. A `pin`
/// names a pipeline round and the head its rerun must carry.
fn rerun_from(
    fx: &Fixture,
    leaf: &Leaf,
    rounds: &[(&str, Value)],
    head: &str,
    reused: u32,
    pin: Option<(&str, &str)>,
) {
    checkpoint_rounds(fx, leaf, rounds);
    let seeded = repaired_state(fx, leaf, head, "done");
    assert_reused(&seeded, 0..reused);
    assert!(
        !seeded.step_states.contains_key(&reused),
        "step {reused} reruns"
    );
    assert_eq!(seeded.next_step_index, reused);
    if let Some((id, sha)) = pin {
        assert_eq!(
            seeded.pipeline[id]["local_sha"], sha,
            "{id} pins the published head"
        );
    }
}

/// The resumed run of `leaf`, repaired once at `head`: its seeded state.
fn repaired_state(fx: &Fixture, leaf: &Leaf, head: &str, completion: &str) -> PipelineState {
    fx.blocked(leaf, head, BOT, true, &[]);
    let drain = fx.drain();
    let pass = fx.pass(&drain, completion, true);
    assert_started(&pass, &leaf.task);
    let retries = fx.retries(&leaf.run);
    assert_eq!(retries.len(), 1, "{pass}");
    fx.state(&retries[0].run_id)
}

fn assert_reused(seeded: &PipelineState, indices: std::ops::Range<u32>) {
    for index in indices {
        assert_eq!(
            seeded.step_states.get(&index),
            Some(&JobRunState::Success),
            "step {index} is reused"
        );
    }
}

fn rebased_round() -> Value {
    json!({"phase": "complete", "re_review_required": true})
}

fn republished(leaf: &Leaf, local: &str, before: &str) -> Value {
    json!({"phase": "push", "branch": leaf.head, "local_sha": local, "remote_sha_before": before})
}

/// Records a failure handoff for `leaf`'s run naming pull request `pr` on
/// `head`, with the run's `pr_open` checkpoint removed as a run that failed
/// before opening its pull request leaves it.
fn failure_handoff(fx: &Fixture, leaf: &Leaf, pr: &str, head: &str) {
    let mut state = fx.state(&leaf.run);
    if let Some(pipeline) = state.pipeline.as_object_mut() {
        pipeline.remove("pr_open");
    }
    state.record_failure_activity(
        "pr_failure_handoff".into(),
        "complete_pr".into(),
        json!({
            "phase": "failure_handoff",
            "decision": "blocked_conflict_pr",
            "task_id": leaf.task,
            "pr_number": pr,
            "branch": leaf.head,
            "head_sha": head,
            "task_spec_digest": "spec-digest",
        }),
    );
    fx.jobs.write_run_state(&leaf.run, &state).unwrap();
}

fn find_task_events_log(root: &std::path::Path, task_id: &str) -> std::path::PathBuf {
    fn walk(dir: &std::path::Path, task_id: &str) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|name| name.to_str()) == Some(task_id) {
                    let candidate = path.join("events.jsonl");
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
                if let Some(found) = walk(&path, task_id) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(root, task_id).unwrap_or_else(|| panic!("events.jsonl for {task_id} under {root:?}"))
}

/// The dashboard's Fix PR starts one task's repair through the drain pass's
/// own verdict, ledger and start, with no drain, and refuses with the pass's
/// reason everything the pass would leave alone.
fn one_tasks_repair_starts_without_a_drain_and_refusals_are_typed() {
    let fx = fixture();
    // The dashboard's refusal of `task`, checked against its readiness.
    let refusal = |task: &str, completion_done: bool| -> String {
        let readiness = fx.fix_pr_readiness(task, completion_done);
        assert_eq!(readiness["available"], false, "{readiness}");
        let refusal = refusal_of(task, fx.dashboard_start(task, completion_done));
        assert_eq!(readiness["reason"], refusal.reason(), "{readiness}");
        let code = format!("task_auto_fix_{}", refusal.reason());
        assert_eq!(refusal.code(), code);
        refusal.reason().to_string()
    };

    let leaf = fx.stopped_leaf(121, "review", Shelf::Review);
    fx.blocked(&leaf, "fff1", BOT, true, &[]);
    assert_offers(&fx.fix_pr_readiness(&leaf.task, false), "conflict");
    assert!(fx.retries(&leaf.run).is_empty(), "readiness starts nothing");
    let started = fx.dashboard_start(&leaf.task, false).unwrap();
    let retries = fx.retries(&leaf.run);
    assert_eq!(retries.len(), 1, "{started}");
    assert_eq!(started["resumed_run_id"], retries[0].run_id);
    let input = retries[0].input.as_ref().unwrap();
    assert_eq!(input["auto_fix"], true);
    assert_eq!(input["footprint_deferred"], true);
    assert_eq!(
        input["completion"], "review",
        "a dashboard repair never gains merge authority the leaf lacked"
    );
    assert_eq!(input["out_of_run_auto_fix"]["drain_run_id"], Value::Null);
    let history = fx.runtime.get_task_history(&leaf.task).unwrap();
    let recorded: Vec<Value> = history
        .iter()
        .filter(|entry| entry.event == "pr_auto_fix_out_of_run")
        .map(|entry| serde_json::from_str(entry.note.as_deref().unwrap()).unwrap())
        .collect();
    assert_eq!(recorded.len(), 1, "{history:?}");
    assert_eq!(recorded[0]["resumed_run_id"], retries[0].run_id);
    assert!(recorded[0].get("drain_run_id").is_none(), "{}", recorded[0]);
    // The resumed leaf is live now.
    assert_eq!(refusal(&leaf.task, false), "live_run");

    // A dashboard start spends the same ledger a drain pass reads.
    fx.finish(&retries[0].run_id, JobRunState::Failed);
    let drain = fx.drain();
    let pass = fx.pass(&drain, "review", true);
    assert_skip(&pass, &leaf.task, "already_started");

    let held = fx.stopped_leaf(122, "review", Shelf::OperatorHold);
    fx.blocked(&held, "fff2", BOT, true, &[]);
    let opted_out = fx.stopped_leaf(123, "review", Shelf::OptedOut);
    fx.blocked(&opted_out, "fff3", BOT, true, &[]);
    let foreign = fx.stopped_leaf(124, "review", Shelf::Review);
    fx.blocked(&foreign, "fff4", "someone-else", true, &[]);
    let wants_merge = fx.stopped_leaf(125, "done", Shelf::Review);
    fx.blocked(&wants_merge, "fff5", BOT, true, &[]);
    let clean = fx.stopped_leaf(126, "review", Shelf::Review);
    fx.blocked(&clean, "fff6", BOT, false, &[]);
    let closed = fx.stopped_leaf(127, "review", Shelf::Review);
    let undelivered = fx
        .runtime
        .add_task(TaskAddParams {
            title: "Never delivered".into(),
            description: "No delivery run of its own.".into(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap()
        .id;
    for (task, completion_done, reason) in [
        (&held.task, true, "operator_hold"),
        (&opted_out.task, true, "opted_out"),
        (&foreign.task, true, "not_orbit_pull_request"),
        (&wants_merge.task, false, "authority"),
        (&clean.task, true, "no_blocker"),
        (&closed.task, true, "pull_request_not_open"),
        (&undelivered, true, "no_delivery_leaf"),
    ] {
        assert_eq!(refusal(task, completion_done), reason, "{task}");
    }
    for leaf in [&held, &opted_out, &foreign, &wants_merge, &clean, &closed] {
        fx.assert_not_resumed(leaf);
    }
    assert!(matches!(
        fx.dashboard_start("ORB-99999", true),
        Err(TaskAutoFixError::Runtime(OrbitError::NotFound { .. }))
    ));

    // A leaf started with `--complete` resumes under completion authority, and
    // keeps that grant.
    let started = fx.dashboard_start(&wants_merge.task, true).unwrap();
    let resumed = &fx.retries(&wants_merge.run)[0];
    assert_eq!(started["resumed_run_id"], resumed.run_id);
    assert_eq!(resumed.input.as_ref().unwrap()["completion"], "done");
}

/// [ORB-23003] A dashboard or `orbit run ship` delivery without `--complete`
/// succeeds at review; its red pull request is handed back by Fix PR, while a
/// drain pass still repairs only stopped leaves.
fn fix_pr_hands_back_a_red_pull_request_a_review_delivery_left() {
    let fx = fixture();
    let shipped = fx.review_leaf(131);
    fx.blocked(&shipped, "ggg1", BOT, false, &["Test"]);
    // A stopped leaf nominates the drain pass; the shipped task's leaf did not
    // stop, so the pass leaves it alone.
    let stopped = fx.stopped_leaf(132, "review", Shelf::Review);
    fx.blocked(&stopped, "ggg2", BOT, true, &[]);
    let drain = fx.drain();
    let pass = fx.pass(&drain, "review", true);
    assert_started(&pass, &stopped.task);
    assert_not_started(&pass, &shipped.task);
    fx.assert_not_resumed(&shipped);

    assert_offers(&fx.fix_pr_readiness(&shipped.task, false), "failed_checks");
    let started = fx.dashboard_start(&shipped.task, false).unwrap();
    let retries = fx.retries(&shipped.run);
    assert_eq!(retries.len(), 1, "{started}");
    let input = retries[0].input.as_ref().unwrap();
    assert_eq!(input["auto_fix"], true, "{input}");
    assert_eq!(
        input["completion"], "review",
        "the repair never gains merge authority the delivery lacked"
    );
    assert_eq!(
        input["out_of_run_auto_fix"]["failed_checks"],
        json!(["Test"])
    );
}

/// [ORB-28628] A delivery that ended `success` was compacted when it finished:
/// its step outputs sit behind pointers into `pipeline`. Fix PR resumes it with
/// every reused output back in `step_outputs`, which is all the resumed run's
/// `steps.*` seeding reads; before, the first `when:` that read one failed with
/// "no data recorded for step".
fn fix_pr_resumes_a_finished_delivery_with_its_compacted_outputs() {
    let fx = fixture();
    let shipped = fx.review_leaf(133);
    // Production writes the run's last checkpoint as it finishes; a write after
    // `success` is compacted the same way.
    let finished = fx.state(&shipped.run);
    fx.jobs.write_run_state(&shipped.run, &finished).unwrap();
    let compacted = fx.state(&shipped.run);
    for index in 0..4 {
        assert!(
            compacted.step_output_pointers.contains_key(&index)
                && !compacted.step_outputs.contains_key(&index),
            "step {index} is compacted: {:?}",
            compacted.step_output_pointers
        );
    }

    fx.blocked(&shipped, "hhh1", BOT, false, &["Test"]);
    let started = fx.dashboard_start(&shipped.task, false).unwrap();
    let retries = fx.retries(&shipped.run);
    assert_eq!(retries.len(), 1, "{started}");
    let resumed = fx.state(&retries[0].run_id);
    assert!(
        resumed.step_output_pointers.is_empty(),
        "{:?}",
        resumed.step_output_pointers
    );
    for (index, id) in ["worktree", "push", "pr_open", "promote_tasks"]
        .into_iter()
        .enumerate()
    {
        let index = u32::try_from(index).unwrap();
        assert_eq!(
            resumed.step_outputs.get(&index),
            compacted.pipeline.get(id),
            "step {index} ({id}) is a checkpoint again"
        );
        assert!(resumed.step_outputs.contains_key(&index), "{id}");
    }
    assert_eq!(resumed.next_step_index, 4, "completion reruns");
}

fn a_conflicting_pull_request_its_delivery_blocked_is_handed_back_for_an_auto_fix() {
    let fx = fixture();
    let leaf = fx.stopped_leaf(81, "review", Shelf::Escalated);
    fx.blocked(&leaf, "aaa1", BOT, true, &[]);
    let drain = fx.drain();

    // A drain started without `--auto-fix` never repairs.
    let plain = fx.pass(&drain, "review", false);
    assert!(plain.get("auto_fix").is_none(), "{plain}");
    fx.assert_not_resumed(&leaf);

    let pass = fx.pass(&drain, "review", true);
    let entry = started_of(&pass, &leaf.task);
    assert_eq!(entry["blocker"], "conflict", "{pass}");
    assert_eq!(entry["pr_number"], "81", "{pass}");
    let retries = fx.retries(&leaf.run);
    assert_eq!(retries.len(), 1, "the stopped leaf is resumed once");
    let resumed = &retries[0];
    assert_eq!(entry["resumed_run_id"], resumed.run_id, "{pass}");
    let input = resumed.input.as_ref().unwrap();
    assert_eq!(
        input["auto_fix"], true,
        "the repair holds auto-fix authority"
    );
    assert_eq!(
        input["footprint_deferred"], true,
        "the auto-fix crew takes the task's footprint locks before it edits"
    );
    assert_eq!(
        input["completion"], "review",
        "a repair never gains merge authority the leaf lacked"
    );
    assert_eq!(input["out_of_run_auto_fix"]["blocker"], "conflict");
    assert_eq!(input["out_of_run_auto_fix"]["source_run_id"], leaf.run);

    // The resume reruns completion on the published prefix.
    let seeded = fx.state(&resumed.run_id);
    assert_reused(&seeded, 0..4);
    assert!(!seeded.step_states.contains_key(&4), "completion reruns");
    assert_eq!(seeded.next_step_index, 4);

    assert_eq!(
        fx.status(&leaf.task),
        TaskStatus::Review,
        "the task returns to delivery"
    );
    let history = fx.runtime.get_task_history(&leaf.task).unwrap();
    assert!(
        history
            .iter()
            .any(|entry| entry.event == "resume_review_restored"),
        "{history:?}"
    );

    // Readiness names the repair while it runs.
    let readiness = fx.readiness();
    let live = readiness["out_of_run_auto_fix"].as_array().unwrap();
    assert_eq!(live.len(), 1, "{readiness}");
    assert_eq!(live[0]["run_id"], resumed.run_id);
    assert_eq!(live[0]["pr_number"], "81");

    // The resumed leaf is live, so a later pass starts nothing more.
    let again = fx.pass(&drain, "review", true);
    assert_eq!(again["auto_fix"]["started"], json!([]), "{again}");
    fx.assert_resumed(&leaf, 1);
}

/// A requeued task keeps its closed earlier pull request and appends the
/// fresh one: the repair pass observes and hands back the latest recorded
/// pull request, not the closed first one.
fn a_requeued_tasks_repair_observes_its_latest_recorded_pull_request() {
    let fx = fixture();
    let leaf = fx.delivered_leaf_after(Some(79), 82, "review", Shelf::Escalated, LeafEnd::Failed);
    // Only the fresh pull request is open and conflicting.
    fx.blocked(&leaf, "bbb2", BOT, true, &[]);
    let drain = fx.drain();

    let pass = fx.pass(&drain, "review", true);
    assert_eq!(started_of(&pass, &leaf.task)["pr_number"], "82", "{pass}");
    fx.assert_resumed(&leaf, 1);
}

fn skipped_repairs_are_checkpointed_and_visible_in_readiness() {
    let fx = fixture();
    let spent = fx.stopped_leaf(89, "review", Shelf::Review);
    fx.blocked(&spent, "spent-head", BOT, true, &[]);
    // A published leaf that failed before completion cannot be resumed there.
    let mut state = fx.state(&spent.run);
    state.record_step(3, JobRunState::Failed, None, None);
    fx.jobs.write_run_state(&spent.run, &state).unwrap();
    let held = fx.stopped_leaf(90, "review", Shelf::OperatorHold);
    fx.blocked(&held, "held-head", BOT, false, &["Test"]);
    let drain = fx.drain();
    fx.step(&drain, 0, "adopt_orphans", JobRunState::Success, None);

    let pass = fx.pass(&drain, "review", true);
    let stored = fx.runtime.read_run_state(&drain).unwrap().unwrap();
    let run = fx.runtime.show_job_run_observed(&drain).unwrap();
    let steps = orbit_core::application::job::run_display_steps(&run, Vec::new);
    let output = steps.records[0]
        .output(&stored)
        .expect("run show exposes the completed pass immediately");
    assert_eq!(output, &pass);
    for (leaf, reason, blocker) in [
        (&spent, "not_resumable", json!("conflict")),
        (&held, "operator_hold", Value::Null),
    ] {
        let skip = skip_of(output, &leaf.task);
        assert_eq!(skip["reason"], reason, "{output}");
        assert_eq!(skip["pr_number"], leaf.number.to_string());
        assert_eq!(skip["blocker"], blocker);
    }
    let readiness = fx.readiness();
    let skipped = readiness["out_of_run_auto_fix"].as_array().unwrap();
    assert_eq!(skipped.len(), 2, "{readiness}");
    assert!(
        skipped
            .iter()
            .all(|entry| entry["state"] == "skipped" && entry["drain_run_id"] == drain)
    );
    assert!(
        skipped
            .iter()
            .any(|entry| entry["pr_number"] == "89" && entry["reason"] == "not_resumable")
    );
    assert!(
        skipped
            .iter()
            .any(|entry| entry["pr_number"] == "90" && entry["reason"] == "operator_hold")
    );

    // The latest pass replaces the old snapshot, including when auto-fix is off.
    let plain = fx.pass(&drain, "review", false);
    let stored = fx.runtime.read_run_state(&drain).unwrap().unwrap();
    assert_eq!(steps.records[0].output(&stored), Some(&plain));
    let readiness = fx.readiness();
    assert!(
        readiness.get("out_of_run_auto_fix").is_none(),
        "{readiness}"
    );

    let final_pass = fx.pass(&drain, "review", true);
    fx.jobs
        .finalize_job_run(&drain, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let stored = fx.runtime.read_run_state(&drain).unwrap().unwrap();
    assert_eq!(
        steps.records[0].output(&stored),
        Some(&final_pass),
        "terminal compaction retains the last pass"
    );
    let ended = fx.readiness();
    assert!(
        ended["out_of_run_auto_fix"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["drain_live"] == false),
        "{ended}"
    );
}

fn full_capacity_still_reports_the_pull_requests_left_alone() {
    let fx = fixture();
    let leaf = fx.stopped_leaf(80, "review", Shelf::Review);
    fx.blocked(&leaf, "capacity-head", BOT, true, &[]);
    let drain = fx.drain();
    let pass_with_slots = |slots: u64| {
        fx.runtime
            .run_deterministic(
                "adopt_orphaned_deliveries",
                &json!({}),
                &json!({"run_id": drain, "max_active_leaf_runs": slots,
                    "completion": "review", "auto_fix": true}),
                ToolContext::default(),
            )
            .unwrap()
    };
    let pass = pass_with_slots(0);
    assert_skip(&pass, &leaf.task, "no_capacity");
    assert_eq!(
        fx.forge_reads(),
        0,
        "a held pass does not spend forge reads"
    );
    fx.assert_not_resumed(&leaf);

    // The scan continues after its last slot is taken, so the older leaf is
    // still reported as held for capacity.
    let newer = fx.stopped_leaf(79, "review", Shelf::Review);
    fx.blocked(&newer, "newer-head", BOT, false, &["Test"]);
    let pass = pass_with_slots(1);
    assert_started(&pass, &newer.task);
    assert_skip(&pass, &leaf.task, "no_capacity");
    let reads = fx.forge_reads();
    fx.probe.sustain_memory(&fx.runtime, 95.0);
    let pass = fx.pass(&drain, "review", true);
    assert_skip(&pass, &leaf.task, "resource_throttled");
    assert_eq!(
        fx.forge_reads(),
        reads,
        "pressure holds repairs without more forge reads"
    );
}

fn a_repair_reruns_the_completion_round_that_published_the_pull_requests_head() {
    let fx = rereviewed_fixture();
    let leaf = fx.stopped_leaf(85, "done", Shelf::Escalated);
    let rounds = [
        ("complete_pr", rebased_round()),
        ("re_push", republished(&leaf, "ddd2", "ddd1")),
        ("complete_reviewed_pr", rebased_round()),
        ("re_push_2", republished(&leaf, "ddd3", "ddd2")),
    ];
    rerun_from(&fx, &leaf, &rounds, "ddd3", 8, Some(("re_push_2", "ddd3")));
}

/// A round that failed right after a re-review republished the head reruns
/// against that publication, not the one before it.
fn a_repair_reruns_the_second_round_when_it_is_the_one_that_failed() {
    let fx = rereviewed_fixture();
    let leaf = fx.stopped_leaf(86, "done", Shelf::Escalated);
    let rounds = [
        ("complete_pr", rebased_round()),
        ("re_push", republished(&leaf, "eee2", "eee1")),
    ];
    rerun_from(&fx, &leaf, &rounds, "eee2", 6, Some(("re_push", "eee2")));
}

/// Rounds that rebased without a republication, and conditional steps that
/// checkpointed Success with null output, are not publications: the repair
/// reruns the round that follows the original push.
fn a_repair_does_not_count_a_skipped_republication_as_published() {
    let fx = rereviewed_fixture();
    let leaf = fx.stopped_leaf(87, "done", Shelf::Escalated);
    let rounds = [
        ("complete_pr", rebased_round()),
        ("re_push", Value::Null),
        ("complete_reviewed_pr", Value::Null),
        ("re_push_2", Value::Null),
    ];
    rerun_from(&fx, &leaf, &rounds, "fff1", 4, None);
}

fn red_checks_and_review_or_requeued_tasks_are_repaired_too() {
    let fx = fixture();
    let red = fx.stopped_leaf(82, "review", Shelf::Escalated);
    fx.blocked(&red, "bbb1", BOT, false, &["Test"]);
    let in_review = fx.stopped_leaf(83, "done", Shelf::Review);
    fx.blocked(&in_review, "bbb2", BOT, false, &["Clippy"]);
    let requeued = fx.stopped_leaf(84, "review", Shelf::Requeued);
    fx.blocked(&requeued, "bbb3", BOT, true, &[]);
    let drain = fx.drain();

    let pass = fx.pass(&drain, "done", true);
    for (leaf, blocker) in [
        (&red, "failed_checks"),
        (&in_review, "failed_checks"),
        (&requeued, "conflict"),
    ] {
        assert_started_with(&pass, &leaf.task, "blocker", blocker);
        fx.assert_resumed(leaf, 1);
        assert_eq!(fx.status(&leaf.task), TaskStatus::Review, "{}", leaf.task);
    }
    let resumed = &fx.retries(&red.run)[0];
    assert_eq!(
        resumed.input.as_ref().unwrap()["out_of_run_auto_fix"]["failed_checks"],
        json!(["Test"])
    );
}

fn a_failure_handoff_pull_request_without_pr_open_names_the_requeue_action() {
    let fx = fixture();
    let leaf = fx.stopped_leaf(97, "review", Shelf::Escalated);
    failure_handoff(&fx, &leaf, "97", "handoff-head");
    fx.blocked(&leaf, "handoff-head", BOT, true, &[]);
    // A handoff naming another pull request is not this task's publication.
    let other = fx.stopped_leaf(98, "review", Shelf::Escalated);
    failure_handoff(&fx, &other, "999", "other-head");
    let drain = fx.drain();

    let pass = fx.pass(&drain, "review", true);
    assert_eq!(pass["auto_fix"]["started"], json!([]), "{pass}");
    assert_skip(&pass, &other.task, "no_pull_request");
    let skip = skip_of(&pass, &leaf.task);
    assert_eq!(skip["reason"], "failure_handoff", "{pass}");
    assert_ne!(skip["reason"], "no_pull_request", "{pass}");
    assert_eq!(skip["pr_number"], leaf.number.to_string(), "{pass}");
    let detail = skip["detail"].as_str().unwrap_or("");
    let number = leaf.number.to_string();
    // The detail names the requeue action with placeholders, not this task's id
    // or pull request number.
    assert_detail(
        &leaf.task,
        detail,
        &[
            "orbit task requeue <task-id>",
            "--reason '<why this candidate can continue>'",
        ],
        &[leaf.task.as_str(), number.as_str()],
    );
    fx.assert_not_resumed(&leaf);
    fx.assert_not_resumed(&other);

    let readiness = fx.readiness();
    let skipped = readiness["out_of_run_auto_fix"].as_array().unwrap();
    assert!(
        skipped.iter().any(|entry| entry["pr_number"] == "97"
            && entry["state"] == "skipped"
            && entry["reason"] == "failure_handoff"),
        "{readiness}"
    );
    assert!(
        skipped
            .iter()
            .filter(|entry| entry["pr_number"] == "97")
            .all(|entry| entry["reason"] != "no_pull_request"),
        "{readiness}"
    );
}

fn a_genuine_forge_failure_still_defers_later_blocker_reads() {
    let fx = fixture();
    let older = fx.stopped_leaf(87, "review", Shelf::Escalated);
    fx.blocked(&older, "eee1", BOT, true, &[]);
    let newest = fx.stopped_leaf(88, "review", Shelf::Escalated);
    fx.blocked(&newest, "eee2", BOT, true, &[]);
    fx.forge.0.lock().unwrap().unreachable = true;
    let drain = fx.drain();

    let pass = fx.pass(&drain, "review", true);

    // A real forge failure defers every candidate of this pass.
    for leaf in [&newest, &older] {
        assert_skip(&pass, &leaf.task, "forge_unreadable");
        fx.assert_not_resumed(leaf);
    }
    assert_eq!(fx.forge_reads(), 1);
}

fn holds_opt_outs_missing_grants_and_foreign_pull_requests_are_never_repaired() {
    let fx = fixture();
    let held = fx.stopped_leaf(91, "review", Shelf::OperatorHold);
    fx.blocked(&held, "ccc1", BOT, true, &[]);
    let opted_out = fx.stopped_leaf(92, "review", Shelf::OptedOut);
    fx.blocked(&opted_out, "ccc2", BOT, true, &[]);
    let foreign = fx.stopped_leaf(93, "review", Shelf::Review);
    fx.blocked(&foreign, "ccc3", "someone-else", true, &[]);
    let wants_merge = fx.stopped_leaf(94, "done", Shelf::Escalated);
    fx.blocked(&wants_merge, "ccc4", BOT, true, &[]);
    let claimed = fx.stopped_leaf(96, "review", Shelf::Review);
    fx.blocked(&claimed, "claimed-head", BOT, true, &[]);
    let mut claimed_run = fx.jobs.get_job_run(&claimed.run).unwrap().unwrap();
    claimed_run.input.as_mut().unwrap()["claimed"] = json!(true);
    fx.runtime
        .sqlite_store()
        .unwrap()
        .write(|tx| {
            tx.job_run()
                .put(&fx.runtime.workspace_id().unwrap(), &claimed_run, None)
        })
        .unwrap();
    let clean = fx.stopped_leaf(95, "review", Shelf::Review);
    fx.blocked(&clean, "ccc5", BOT, false, &[]);
    let drain = fx.drain();

    let pass = fx.pass(&drain, "review", true);
    assert_eq!(pass["auto_fix"]["started"], json!([]), "{pass}");
    assert_eq!(repair_skip(&pass, &claimed.task), None, "{pass}");
    fx.assert_not_resumed(&claimed);
    for (leaf, reason) in [
        (&held, "operator_hold"),
        (&opted_out, "opted_out"),
        (&foreign, "not_orbit_pull_request"),
        (&wants_merge, "authority"),
        (&clean, "no_blocker"),
    ] {
        assert_skip(&pass, &leaf.task, reason);
        fx.assert_not_resumed(leaf);
    }
    assert_eq!(
        fx.status(&held.task),
        TaskStatus::Blocked,
        "an operator's block stays"
    );
}

/// The pull request's auto-fix budget bounds out-of-run repairs: spent budgets
/// are skipped, a hand-back is spent once per head, and a new head is repaired.
fn the_pull_requests_auto_fix_budget_bounds_out_of_run_repairs() {
    let fx = fixture();
    // A pull request whose in-run auto-fix already spent its three repairs.
    let spent = fx.stopped_leaf(101, "review", Shelf::Escalated);
    fx.blocked(&spent, "ddd9", BOT, true, &[]);
    let attempts = (1..=3)
        .map(|attempt| {
            history_entry(
                PR_AUTO_FIX_ADMITTED_EVENT,
                json!({"pr_number": "101", "head_sha": format!("ddd{attempt}"),
                    "blocker": "failed_checks", "attempt": attempt, "budget": 3,
                    "run_id": spent.run, "crew": "fixer", "crew_source": "pool"})
                .to_string(),
            )
        })
        .collect();
    append_history(&fx.runtime, &spent.task, attempts);
    // A pull request handed back once whose resumed leaf failed again on the
    // same head and blocker.
    let looped = fx.stopped_leaf(102, "review", Shelf::Escalated);
    fx.blocked(&looped, "eee1", BOT, true, &[]);
    let drain = fx.drain();
    let first = fx.pass(&drain, "review", true);
    assert_started(&first, &looped.task);
    assert_skip(&first, &spent.task, "budget_exhausted");
    let resumed = fx.retries(&looped.run)[0].run_id.clone();
    fx.finish(&resumed, JobRunState::Failed);
    apply_update(
        &fx.runtime,
        &looped.task,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            status_event: Some("final_recovery_escalated".into()),
            status_note: Some(format!(
                "final recovery (run_id={resumed}) escalated: the conflict stayed"
            )),
            ..Default::default()
        },
    );

    let again = fx.pass(&drain, "review", true);
    assert_skip(&again, &looped.task, "already_started");
    assert!(fx.retries(&resumed).is_empty(), "one hand-back per head");

    // A new head is a new repair.
    fx.blocked(&looped, "eee2", BOT, true, &[]);
    let next = fx.pass(&drain, "review", true);
    assert_started(&next, &looped.task);
    assert_eq!(fx.retries(&resumed).len(), 1);
}

fn a_repair_that_published_nothing_leaves_the_heads_unspent_attempts_usable() {
    let fx = fixture();
    // One admitted repair published no head: the head is still red and two of
    // three attempts are unspent.
    let stranded = fx.stopped_leaf(131, "review", Shelf::Escalated);
    fx.blocked(&stranded, "ggg1", BOT, false, &["Check"]);
    fx.record_attempts(&stranded, "ggg1", "jrun-earlier", 1);
    // Another pull request spent all three attempts, the last on its head.
    let spent = fx.stopped_leaf(132, "review", Shelf::Escalated);
    fx.blocked(&spent, "hhh1", BOT, false, &["Check"]);
    fx.record_attempts(&spent, "hhh1", "jrun-spent", 3);
    let drain = fx.drain();

    // The unchanged red head is admitted a second attempt.
    let pass = fx.pass(&drain, "review", true);
    assert_started(&pass, &stranded.task);
    assert_skip_with(
        &pass,
        &spent.task,
        "budget_exhausted",
        &["repair budget spent: 3 of 3 attempts used"],
    );
}

fn a_skipped_head_lists_the_attempts_spent_and_remaining() {
    let fx = fixture();
    let leaf = fx.stopped_leaf(141, "review", Shelf::OptedOut);
    fx.blocked(&leaf, "iii1", BOT, false, &["Check"]);
    fx.record_attempts(&leaf, "iii1", "jrun-earlier", 1);
    let drain = fx.drain();

    let pass = fx.pass(&drain, "review", true);
    assert_detail(&leaf.task, &detail_of(&pass, &leaf.task), &[SPENT_1], &[]);
}

/// ORB-27262: an admitted check repair whose `step_failure_recovery` failed
/// without publishing holds as `delivery_repair_dispatched` carrying
/// `pr_auto_fix_check_repair:`. The drain observes it without ignoring it: it
/// retries while budget remains, reports spent/remaining attempts when
/// exhausted or held, hands back if checks turn green, honors operator/opt-out
/// holds, and keeps ignoring other `delivery_repair_dispatched` holds.
fn an_unrepaired_check_repair_hold_retries_in_budget_reports_exhausted_and_honors_holds() {
    let fx = fixture();

    // 1. One failed check recovery (1 of 3 spent, 0 pushes, unchanged red head):
    //    both the drain pass and Fix PR offer/start the next authorized attempt.
    let retryable = fx.unrepaired_leaf(142, "done", Shelf::Review);
    fx.blocked(&retryable, PUBLISHED, BOT, false, &["Check"]);
    fx.record_attempts(&retryable, PUBLISHED, &retryable.run, 1);
    assert_offers(&fx.fix_pr_readiness(&retryable.task, true), "failed_checks");

    // 2. Spent budget (3 of 3 spent) on an unrepaired check-repair hold:
    //    reported as budget_exhausted with remaining budget in readiness.
    let exhausted = fx.unrepaired_leaf(143, "done", Shelf::Review);
    fx.blocked(&exhausted, PUBLISHED, BOT, false, &["Check"]);
    fx.record_attempts(&exhausted, PUBLISHED, &exhausted.run, 3);

    // 3. Unrelated operator hold and no-auto-approve opt-out stay honored and
    //    report the remaining two attempts in readiness.
    let op_held = fx.unrepaired_leaf(144, "done", Shelf::OperatorHold);
    fx.blocked(&op_held, PUBLISHED, BOT, false, &["Check"]);
    fx.record_attempts(&op_held, PUBLISHED, &op_held.run, 1);
    let opted_out = fx.unrepaired_leaf(145, "done", Shelf::OptedOut);
    fx.blocked(&opted_out, PUBLISHED, BOT, false, &["Check"]);
    fx.record_attempts(&opted_out, PUBLISHED, &opted_out.run, 1);

    // 4. Other `delivery_repair_dispatched` holds (a spent/failed no-edit rerun
    //    without `pr_auto_fix_check_repair:`, or a post-recovery hold wrapping
    //    `; original error before recovery:`) keep their own next step and are
    //    ignored by the drain pass.
    let held_dispatched = LeafEnd::Held("delivery_repair_dispatched");
    let other_hold = fx.delivered_leaf(146, "done", Shelf::Review, held_dispatched);
    fx.blocked(&other_hold, PUBLISHED, BOT, false, &["Check"]);
    let post_recovery_hold = fx.delivered_leaf(147, "done", Shelf::Review, held_dispatched);
    fx.step_with_message(
        &post_recovery_hold.run,
        4,
        "complete_pr",
        JobRunState::Failed,
        Some(DeliveryRefusal::RepairDispatched.error_code()),
        Some(
            "no-edit rerun failed again; original error before recovery: \
             pr_auto_fix_check_repair: pull request #147 failed required checks",
        ),
    );
    fx.blocked(&post_recovery_hold, PUBLISHED, BOT, false, &["Check"]);

    let drain = fx.drain();
    fx.step(&drain, 0, "adopt_orphans", JobRunState::Success, None);
    let pass = fx.pass(&drain, "done", true);

    // Attempt 2 starts for the in-budget unrepaired check repair.
    assert_started_with(&pass, &retryable.task, "blocker", "failed_checks");
    assert_started_with(&pass, &retryable.task, "pr_number", "142");
    let retries = fx.retries(&retryable.run);
    assert_eq!(retries.len(), 1, "{pass}");
    assert_eq!(
        fx.state(&retries[0].run_id).next_step_index,
        4,
        "completion reruns: {pass}"
    );
    assert_eq!(fx.status(&retryable.task), TaskStatus::Review);

    // Exhausted and held tasks are reported in both pass output and readiness.
    assert_skip_with(
        &pass,
        &exhausted.task,
        "budget_exhausted",
        &["repair budget spent: 3 of 3 attempts used, none remain"],
    );
    assert_skip_with(&pass, &op_held.task, "operator_hold", &[SPENT_1]);
    assert_skip_with(&pass, &opted_out.task, "opted_out", &[SPENT_1]);
    for leaf in [&exhausted, &op_held, &opted_out] {
        fx.assert_not_resumed(leaf);
    }
    assert_eq!(fx.status(&op_held.task), TaskStatus::Blocked);

    // Non-matching `delivery_repair_dispatched` holds stay ignored.
    for ignored in [&other_hold, &post_recovery_hold] {
        assert_not_started(&pass, &ignored.task);
        assert_eq!(repair_skip(&pass, &ignored.task), None, "{pass}");
        fx.assert_not_resumed(ignored);
    }

    let readiness = fx.readiness();
    let entries = readiness["out_of_run_auto_fix"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|item| item["run_id"] == retries[0].run_id && item["pr_number"] == "142"),
        "{readiness}"
    );
    assert!(
        entries.iter().any(|item| item["pr_number"] == "143"
            && item["state"] == "skipped"
            && item["reason"] == "budget_exhausted"
            && item["hold_code"] == "delivery_repair_dispatched"
            && item["detail"].as_str().is_some_and(
                |d| d.contains("repair budget spent: 3 of 3 attempts used, none remain")
            )),
        "{readiness}"
    );
    assert!(
        entries.iter().any(|item| item["pr_number"] == "144"
            && item["state"] == "skipped"
            && item["reason"] == "operator_hold"
            && item["hold_code"] == "delivery_repair_dispatched"
            && item["detail"].as_str().is_some_and(|d| d.contains(SPENT_1))),
        "{readiness}"
    );

    // If the resumed attempt 2 also holds unrepaired, attempt 3 still starts.
    let resumed_2 = retries[0].run_id.clone();
    fx.hold(
        &resumed_2,
        4,
        DeliveryRefusal::RepairDispatched.error_code(),
        "deterministic action `complete_pr` refused: admitted check repair ran \
         step_failure_recovery and it did not repair the head; \
         pr_auto_fix_check_repair: pull request #142 failed required checks on head \
         published-head; auto-fix repair attempt 2 of 3 runs as crew 'fixer'.",
    );
    append_history(
        &fx.runtime,
        &retryable.task,
        vec![history_entry(
            PR_AUTO_FIX_ADMITTED_EVENT,
            json!({"pr_number": "142", "head_sha": PUBLISHED, "blocker": "failed_checks",
                "attempt": 2, "budget": 3, "run_id": resumed_2, "crew": "fixer",
                "crew_source": "pool"})
            .to_string(),
        )],
    );
    let pass_3 = fx.pass(&drain, "done", true);
    assert_started(&pass_3, &retryable.task);
    assert_eq!(fx.retries(&resumed_2).len(), 1, "{pass_3}");

    // If checks turn green on the same published head after an unrepaired
    // check-repair hold (with pending checks waiting first), completion is
    // handed back without spending another repair attempt.
    let cleared = fx.unrepaired_leaf(148, "done", Shelf::Review);
    fx.report_required_checks(&cleared, PUBLISHED, BOT, &[], &["Check"]);
    fx.record_attempts(&cleared, PUBLISHED, &cleared.run, 1);
    let waiting = fx.pass(&drain, "done", true);
    assert_skip_with(
        &waiting,
        &cleared.task,
        "delivery_repair_dispatched",
        &[SPENT_1],
    );
    fx.report_required_checks(&cleared, PUBLISHED, BOT, &["Check"], &[]);
    let green = fx.pass(&drain, "done", true);
    assert_started_with(&green, &cleared.task, "reason", "checks_green_after_hold");
    fx.assert_resumed(&cleared, 1);
}

/// A configured required check that has not reported on the held head is not
/// green, even while its siblings passed. An auto-fix stop and an unrepaired
/// check-recovery hold each wait on that partial rollup: no completion retry
/// and no hand-back start is recorded. Both hand back only once every
/// configured required check passes on the same published head.
fn a_partially_reported_rollup_hands_back_only_once_every_required_check_passes() {
    let fx = fixture();
    let drain = fx.drain();
    let unrepaired = fx.unrepaired_leaf(149, "done", Shelf::Review);
    let stopped = fx.delivered_leaf(150, "done", Shelf::Review, LeafEnd::HeldAutoFixStop);
    for leaf in [&unrepaired, &stopped] {
        fx.report_partial_required_checks(leaf, PUBLISHED, &[], &["Build"], &["Test"]);
    }
    fx.record_attempts(&unrepaired, PUBLISHED, &unrepaired.run, 1);

    // A passing classic check with an unreadable layered policy is not a
    // complete green observation for either held route.
    for leaf in [&unrepaired, &stopped] {
        fx.report_required_checks(leaf, PUBLISHED, BOT, &["Build"], &[]);
        fx.forge
            .0
            .lock()
            .unwrap()
            .blockers
            .get_mut(&leaf.number)
            .unwrap()
            .requires_status_checks = None;
    }
    let unreadable = fx.pass(&drain, "done", true);
    for leaf in [&unrepaired, &stopped] {
        assert_not_started(&unreadable, &leaf.task);
        fx.assert_not_resumed(leaf);
        fx.report_partial_required_checks(leaf, PUBLISHED, &[], &["Build"], &["Test"]);
    }

    let waiting = fx.pass(&drain, "done", true);
    assert_skip_with(
        &waiting,
        &unrepaired.task,
        "delivery_repair_dispatched",
        &[SPENT_1],
    );
    assert_skip(&waiting, &stopped.task, "auto_fix_stopped");
    assert_not_started(&waiting, &unrepaired.task);
    assert_not_started(&waiting, &stopped.task);
    fx.assert_not_resumed(&unrepaired);
    fx.assert_not_resumed(&stopped);

    for leaf in [&unrepaired, &stopped] {
        fx.report_partial_required_checks(leaf, PUBLISHED, &[], &["Build", "Test"], &[]);
    }
    let green = fx.pass(&drain, "done", true);
    for leaf in [&unrepaired, &stopped] {
        assert_started_with(&green, &leaf.task, "reason", "checks_green_after_hold");
        fx.assert_resumed(leaf, 1);
    }
}

/// A confirmed zero-required-check base still hands back an auto-fix stop, and
/// a failed required check on an unrepaired check-recovery hold still takes
/// the repair ledger path while another configured check has not reported.
fn a_zero_check_base_and_a_failed_check_keep_their_repair_paths() {
    let fx = fixture();
    let drain = fx.drain();
    let zero = fx.delivered_leaf(151, "done", Shelf::Review, LeafEnd::HeldAutoFixStop);
    fx.report_no_required_checks(&zero, PUBLISHED, BOT);
    let failed = fx.unrepaired_leaf(152, "done", Shelf::Review);
    fx.report_partial_required_checks(&failed, PUBLISHED, &["Check"], &["Build"], &["Test"]);
    fx.record_attempts(&failed, PUBLISHED, &failed.run, 1);

    let pass = fx.pass(&drain, "done", true);
    assert_started_with(&pass, &zero.task, "reason", "checks_green_after_hold");
    fx.assert_resumed(&zero, 1);
    assert_started_with(&pass, &failed.task, "blocker", "failed_checks");
    fx.assert_resumed(&failed, 1);
}

/// An unrepaired check-repair hold is judged on the head its newest completion
/// round published. After re-review republished the pull request through
/// `re_push`, green checks on that head hand completion back through both Fix
/// PR and the drain. A red or conflicting replacement head on the same branch
/// is an identity mismatch: nothing resumes and no repair attempt is recorded.
fn an_unrepaired_check_repair_hold_is_judged_on_its_newest_publication() {
    let fx = rereviewed_fixture();
    let rereviewed = |number: u64| {
        let leaf = fx.unrepaired_leaf(number, "done", Shelf::Review);
        checkpoint_rounds(
            &fx,
            &leaf,
            &[
                ("complete_pr", rebased_round()),
                ("re_push", republished(&leaf, "reviewed-head", PUBLISHED)),
            ],
        );
        fx.record_attempts(&leaf, "reviewed-head", &leaf.run, 1);
        leaf
    };
    let admissions = |task: &str| {
        fx.runtime
            .get_task_history(task)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == PR_AUTO_FIX_ADMITTED_EVENT)
            .count()
    };

    // Fix PR hands back a green re-review publication.
    let fix_pr = rereviewed(160);
    fx.report_required_checks(&fix_pr, "reviewed-head", BOT, &["Check"], &[]);
    let readiness = fx.fix_pr_readiness(&fix_pr.task, true);
    assert_eq!(readiness["available"], true, "{readiness}");
    fx.dashboard_start(&fix_pr.task, true).unwrap();
    fx.assert_resumed(&fix_pr, 1);

    // The drain hands back a green re-review publication.
    let drained = rereviewed(161);
    fx.report_required_checks(&drained, "reviewed-head", BOT, &["Check"], &[]);

    // Red and conflicting replacements of the published head, on the
    // first-round and on the re-reviewed publication alike.
    let red = fx.unrepaired_leaf(162, "done", Shelf::Review);
    fx.blocked(&red, "replacement-head", BOT, false, &["Check"]);
    fx.record_attempts(&red, PUBLISHED, &red.run, 1);
    let conflicting = rereviewed(163);
    fx.blocked(&conflicting, "replacement-head", BOT, true, &[]);
    // The first-round head is no longer the re-reviewed run's publication.
    let superseded = rereviewed(164);
    fx.blocked(&superseded, PUBLISHED, BOT, false, &["Check"]);

    let drain = fx.drain();
    let pass = fx.pass(&drain, "done", true);
    let handed = started_of(&pass, &drained.task);
    assert_eq!(handed["reason"], "checks_green_after_hold", "{pass}");
    assert_eq!(handed["head_sha"], "reviewed-head", "{pass}");
    fx.assert_resumed(&drained, 1);
    for leaf in [&red, &conflicting, &superseded] {
        assert_skip(&pass, &leaf.task, "publication_identity_changed");
        assert_not_started(&pass, &leaf.task);
        fx.assert_not_resumed(leaf);
        assert_eq!(admissions(&leaf.task), 1, "no repair start is recorded");
        let readiness = fx.fix_pr_readiness(&leaf.task, true);
        assert_eq!(readiness["available"], false, "{readiness}");
        assert_eq!(
            readiness["reason"], "publication_identity_changed",
            "{readiness}"
        );
        let refusal = refusal_of(&leaf.task, fx.dashboard_start(&leaf.task, true));
        assert_eq!(refusal.reason(), "publication_identity_changed");
        fx.assert_not_resumed(leaf);
        assert_eq!(admissions(&leaf.task), 1);
    }
}

fn a_source_repair_and_a_watcher_attempt_share_the_pull_requests_budget() {
    let fx = fixture();
    let leaf = fx.stopped_leaf(151, "review", Shelf::OptedOut);
    fx.blocked(&leaf, "jjj1", BOT, false, &["Check"]);
    fx.record_source_repair_reservation(&leaf, "jjj0", 1);
    fx.record_attempts(&leaf, "jjj1", "jrun-earlier", 1);
    let drain = fx.drain();

    let pass = fx.pass(&drain, "review", true);
    assert_detail(&leaf.task, &detail_of(&pass, &leaf.task), &[SPENT_2], &[]);
    // Readiness names the same shared count.
    let readiness = fx.readiness();
    let entry = readiness["out_of_run_auto_fix"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["pr_number"] == "151")
        .unwrap_or_else(|| panic!("{readiness}"));
    assert_detail(
        &leaf.task,
        entry["detail"].as_str().unwrap_or_default(),
        &[SPENT_2],
        &[],
    );

    // A second source repair makes the third spend, so the budget is spent.
    fx.record_source_repair_reservation(&leaf, "jjj2", 2);
    let pass = fx.pass(&drain, "review", true);
    assert_detail(
        &leaf.task,
        &detail_of(&pass, &leaf.task),
        &["repair budget spent: 3 of 3 attempts used, none remain"],
        &[],
    );
}

/// Source repairs alone can spend the whole budget: three reserved repairs and
/// no watcher attempt are three spent, not zero of zero, in both the refusal
/// and its skip note.
fn source_repairs_alone_spend_the_shared_budget_in_the_refusal() {
    let fx = fixture();
    let spent = fx.stopped_leaf(152, "review", Shelf::Escalated);
    fx.blocked(&spent, "kkk9", BOT, false, &["Check"]);
    for attempt in 1..=3 {
        fx.record_source_repair_reservation(&spent, &format!("kkk{attempt}"), attempt);
    }
    let drain = fx.drain();

    let pass = fx.pass(&drain, "review", true);
    assert_skip_with(
        &pass,
        &spent.task,
        "budget_exhausted",
        &[
            "#152 spent its 3 auto-fix repairs",
            "repair budget spent: 3 of 3 attempts used, none remain",
        ],
    );
}

fn a_stuck_pull_request_is_repaired_however_many_deliveries_ran_since() {
    let fx = fixture();
    let leaf = fx.stopped_leaf(110, "review", Shelf::Escalated);
    fx.blocked(&leaf, "old-head", BOT, true, &[]);
    // More finished deliveries of other tasks than any fixed window of recent
    // leaves could hold.
    for index in 0..201 {
        fx.finish_delivery(json!({"task_ids": [format!("ORB-9{index}")], "completion": "review"}));
    }
    let drain = fx.drain();
    let pass = fx.pass(&drain, "review", true);
    assert!(
        started(&pass, &leaf.task).is_some() || repair_skip(&pass, &leaf.task).is_some(),
        "the task's stopped leaf is neither repaired nor reported: {pass}"
    );
    fx.assert_resumed(&leaf, 1);
}

fn a_newer_finished_delivery_of_the_task_keeps_its_older_stopped_leaf_from_resuming() {
    let fx = fixture();
    let leaf = fx.stopped_leaf(111, "review", Shelf::Review);
    fx.blocked(&leaf, "stale-head", BOT, true, &[]);
    fx.finish_delivery(json!({"task_ids": [leaf.task], "completion": "review"}));
    for index in 0..201 {
        fx.finish_delivery(json!({"task_ids": [format!("ORB-8{index}")], "completion": "review"}));
    }
    let drain = fx.drain();
    let pass = fx.pass(&drain, "review", true);
    assert_not_started(&pass, &leaf.task);
    assert_eq!(repair_skip(&pass, &leaf.task), None, "{pass}");
    fx.assert_not_resumed(&leaf);
}

/// ORB-27266: early job-store failures (reading a task's delivery leaves or a
/// held leaf's steps) for a known task and PR preserve the original error and
/// report spent/remaining attempts when the task ledger is readable, or mark
/// ledger evidence unavailable without inventing counts when it is not.
fn early_job_store_failures_preserve_errors_and_report_available_or_unavailable_repair_counts() {
    const LEAVES: &str = "read the task's delivery leaves:";
    const STEPS: &str = "read the run's steps:";
    const UNAVAILABLE: &str = "repair attempts unavailable: task ledger unreadable:";
    const UNCOUNTED: &[&str] = &["of 3 spent", "remaining"];
    const BAD_TIMESTAMP: &str =
        "UPDATE job_runs SET scheduled_at = 'not-a-timestamp' WHERE run_id = ?1";
    const BAD_STEP: &str =
        "UPDATE job_run_steps SET agent_response_json = '{not json' WHERE run_id = ?1";

    let fx = fixture();
    let store = fx.runtime.sqlite_store().unwrap();
    // Runs one corrupting statement against the job store for `run`.
    let corrupt = |sql: &str, run: &str| {
        store
            .connection()
            .lock()
            .unwrap()
            .execute(sql, rusqlite::params![run])
            .unwrap();
    };

    // 1. Newest delivery leaf lookup fails (`pass.rs`), task ledger readable.
    let leaf_fail = fx.stopped_leaf(171, "review", Shelf::Escalated);
    fx.record_attempts(&leaf_fail, "head-171", "jrun-earlier", 1);
    corrupt(BAD_TIMESTAMP, &leaf_fail.run);

    // 2. Held run step lookup fails (`verdict.rs`), task ledger readable.
    let held_fail = fx.delivered_leaf(172, "done", Shelf::Review, LeafEnd::Held(INCONCLUSIVE));
    fx.record_attempts(&held_fail, "head-172", "jrun-earlier", 2);
    corrupt(BAD_STEP, &held_fail.run);

    // 3. Newest delivery leaf lookup fails, source repair ledger entry corrupt.
    let corrupt_ledger = fx.stopped_leaf(173, "review", Shelf::Escalated);
    fx.corrupt_source_ledger(&corrupt_ledger);
    corrupt(BAD_TIMESTAMP, &corrupt_ledger.run);

    // 4. Held run step lookup fails, source repair ledger entry corrupt.
    let held_corrupt_ledger =
        fx.delivered_leaf(174, "done", Shelf::Review, LeafEnd::Held(INCONCLUSIVE));
    fx.corrupt_source_ledger(&held_corrupt_ledger);
    corrupt(BAD_STEP, &held_corrupt_ledger.run);

    // Single-task Fix PR readiness and start reflect each early failure too.
    fx.assert_fix_pr_unreadable(&held_fail, &[STEPS, SPENT_2]);
    fx.assert_fix_pr_unreadable(&leaf_fail, &[LEAVES, SPENT_1]);
    let readiness = fx.fix_pr_readiness(&held_corrupt_ledger.task, true);
    assert_unreadable(&readiness, "174", &[STEPS, UNAVAILABLE], UNCOUNTED);

    // The drain pass reports all four early store failures with PR numbers and
    // either spent/remaining counts or unavailable-ledger diagnostics.
    let drain = fx.drain();
    let pass = fx.pass(&drain, "review", true);
    let skipped = |leaf: &Leaf, with: &[&str], without: &[&str]| {
        assert_unreadable(
            skip_of(&pass, &leaf.task),
            &leaf.number.to_string(),
            with,
            without,
        );
    };
    skipped(&leaf_fail, &[LEAVES, SPENT_1], &[]);
    skipped(&held_fail, &[STEPS, SPENT_2], &[]);
    skipped(&corrupt_ledger, &[LEAVES, UNAVAILABLE], UNCOUNTED);
    skipped(&held_corrupt_ledger, &[STEPS, UNAVAILABLE], UNCOUNTED);

    let readiness = fx.readiness();
    let entries = readiness["out_of_run_auto_fix"].as_array().unwrap();
    assert!(
        entries.iter().any(|item| {
            item["pr_number"] == "171"
                && item["reason"] == "store_unreadable"
                && item["detail"].as_str().is_some_and(|d| d.contains(SPENT_1))
        }),
        "{readiness}"
    );
    assert!(
        entries.iter().any(|item| {
            item["pr_number"] == "173"
                && item["reason"] == "store_unreadable"
                && item["detail"]
                    .as_str()
                    .is_some_and(|d| d.contains(UNAVAILABLE))
        }),
        "{readiness}"
    );

    // When the task's events.jsonl itself is unreadable while metadata is
    // readable, the drain pass reports unavailable ledger evidence too.
    let events_path = find_task_events_log(fx._root.path(), &leaf_fail.task);
    let mut damaged = b"{not valid jsonl\n".to_vec();
    damaged.extend_from_slice(&std::fs::read(&events_path).unwrap());
    std::fs::write(&events_path, damaged).unwrap();
    let pass_unreadable_events = fx.pass(&drain, "review", true);
    assert_unreadable(
        skip_of(&pass_unreadable_events, &leaf_fail.task),
        "171",
        &[LEAVES, UNAVAILABLE],
        UNCOUNTED,
    );
}

crate::scenario_table!(
    out_of_run_auto_fix_scenarios,
    one_tasks_repair_starts_without_a_drain_and_refusals_are_typed,
    fix_pr_hands_back_a_red_pull_request_a_review_delivery_left,
    fix_pr_resumes_a_finished_delivery_with_its_compacted_outputs,
    a_conflicting_pull_request_its_delivery_blocked_is_handed_back_for_an_auto_fix,
    a_requeued_tasks_repair_observes_its_latest_recorded_pull_request,
    a_repair_reruns_the_completion_round_that_published_the_pull_requests_head,
    a_repair_reruns_the_second_round_when_it_is_the_one_that_failed,
    a_repair_does_not_count_a_skipped_republication_as_published,
    skipped_repairs_are_checkpointed_and_visible_in_readiness,
    full_capacity_still_reports_the_pull_requests_left_alone,
    red_checks_and_review_or_requeued_tasks_are_repaired_too,
    a_failure_handoff_pull_request_without_pr_open_names_the_requeue_action,
    a_genuine_forge_failure_still_defers_later_blocker_reads,
    holds_opt_outs_missing_grants_and_foreign_pull_requests_are_never_repaired,
    the_pull_requests_auto_fix_budget_bounds_out_of_run_repairs,
    a_repair_that_published_nothing_leaves_the_heads_unspent_attempts_usable,
    a_skipped_head_lists_the_attempts_spent_and_remaining,
    an_unrepaired_check_repair_hold_retries_in_budget_reports_exhausted_and_honors_holds,
    an_unrepaired_check_repair_hold_is_judged_on_its_newest_publication,
    a_partially_reported_rollup_hands_back_only_once_every_required_check_passes,
    a_zero_check_base_and_a_failed_check_keep_their_repair_paths,
    a_source_repair_and_a_watcher_attempt_share_the_pull_requests_budget,
    source_repairs_alone_spend_the_shared_budget_in_the_refusal,
    a_stuck_pull_request_is_repaired_however_many_deliveries_ran_since,
    a_newer_finished_delivery_of_the_task_keeps_its_older_stopped_leaf_from_resuming,
    early_job_store_failures_preserve_errors_and_report_available_or_unavailable_repair_counts,
);

#[cfg(unix)]
crate::scenario_table!(
    unknown_mergeability_scenarios,
    unknown_mergeability_on_the_newest_pull_request_does_not_defer_older_repairs,
);

#[cfg(unix)]
fn unknown_mergeability_on_the_newest_pull_request_does_not_defer_older_repairs() {
    let fx = fixture();
    let older_conflicting = fx.stopped_leaf(85, "review", Shelf::Escalated);
    fx.blocked(&older_conflicting, "ddd1", BOT, true, &[]);
    let newest_unknown = fx.stopped_leaf(86, "review", Shelf::Escalated);
    fx.mergeability_unknown(&newest_unknown);
    let drain = fx.drain();

    let pass = fx.pass(&drain, "review", true);

    // The unknown answer is a per-pull-request skip, not a pass-wide deferral.
    assert_skip(&pass, &newest_unknown.task, "mergeability_unknown");
    assert_started_with(&pass, &older_conflicting.task, "blocker", "conflict");
    fx.assert_resumed(&older_conflicting, 1);
    fx.assert_not_resumed(&newest_unknown);
}

use std::sync::atomic::{AtomicUsize, Ordering};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

struct EnvelopeSelections(Arc<AtomicUsize>);
impl<S: tracing::Subscriber> Layer<S> for EnvelopeSelections {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _: &tracing::span::Id,
        _: Context<'_, S>,
    ) {
        if matches!(
            attrs.metadata().name(),
            "task_envelope_selection" | "task_metadata_read"
        ) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn idle_pass_reads_only_stopped_leaf_tasks_and_sees_new_candidates() {
    let fx = fixture();
    for index in 0..64 {
        fx.runtime
            .add_task(TaskAddParams {
                title: format!("Closed history {index}"),
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .unwrap();
    }
    let leaf = fx.stopped_leaf(112, "review", Shelf::Review);
    fx.blocked(&leaf, "newly-eligible-head", BOT, true, &[]);
    let other = OrbitRuntime::from_roots(
        &fx._root.path().join("home/.orbit"),
        &fx._root.path().join("repo/.orbit"),
    )
    .unwrap();
    apply_update(
        &other,
        &leaf.task,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Done),
            ..Default::default()
        },
    );
    let malformed = fx
        .jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids": ["not-a-task", "ORB-999999"], "task_id": "../foreign"})),
            None,
        )
        .unwrap();
    fx.finish(&malformed.run_id, JobRunState::Failed);
    let drain = fx.drain();
    // Warm the normal freshness proofs, outside the observed candidate query.
    fx.runtime.list_task_metadata().unwrap();
    for _ in 0..2 {
        let selections = Arc::new(AtomicUsize::new(0));
        let subscriber =
            tracing_subscriber::registry().with(EnvelopeSelections(Arc::clone(&selections)));
        let pass =
            tracing::subscriber::with_default(subscriber, || fx.pass(&drain, "review", true));
        assert_eq!(
            pass["auto_fix"],
            json!({"started":[],"skipped":[]}),
            "{pass}"
        );
        assert_eq!(
            selections.load(Ordering::Relaxed),
            0,
            "idle adoption requests metadata only for stopped-leaf tasks now in \
             review/blocked/backlog, not unrelated closed history"
        );
    }
    // A second runtime publishes the status change: no process-local cache may
    // hide it, even immediately after the previous idle pass.
    apply_update(
        &other,
        &leaf.task,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Review),
            ..Default::default()
        },
    );
    let pass = fx.pass(&drain, "review", true);
    assert_started(&pass, &leaf.task);
    fx.assert_resumed(&leaf, 1);
    let new = fx.stopped_leaf(113, "review", Shelf::Requeued);
    fx.blocked(&new, "new-task-head", BOT, false, &["Test"]);
    let pass = fx.pass(&drain, "review", true);
    assert_started(&pass, &new.task);
    fx.assert_resumed(&new, 1);
}

crate::scenario_table!(
    candidate_population_scenarios,
    idle_pass_reads_only_stopped_leaf_tasks_and_sees_new_candidates
);
