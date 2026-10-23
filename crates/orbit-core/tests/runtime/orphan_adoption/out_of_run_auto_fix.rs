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
//! fake forge; a resumed leaf starts the fixture's substitute worker, so the
//! completion step itself is not executed here.

use orbit_core::runtime::task_pr_forge::{ForgePullRequest, PullRequestBlockers};
use orbit_types::workflow::PR_AUTO_FIX_ADMITTED_EVENT;

use super::*;

/// The shipped leaf's shape around completion: the worktree names the batch,
/// `pr_promote` moves the task to review, `pr_complete` watches the pull
/// request.
fn repair_leaf_job() -> String {
    let step = |id: &str, action: &str| json!({"id": id, "spec": {"type": "deterministic", "action": action, "config": {}}});
    json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pr_pipeline"},
    "spec": {"state": "enabled", "kind": "workflow", "task_delivery": {}, "steps": [
        step("worktree", "pipeline_success_guard"),
        step("push", "git_push"),
        step("pr_open", "pr_open"),
        step("promote_tasks", "pr_promote"),
        step("complete_pr", "pr_complete"),
    ]}})
    .to_string()
}

/// How a stopped leaf left its task.
#[derive(Clone, Copy)]
enum Shelf {
    /// In review, as a timed-out completion leaves it.
    Review,
    /// Blocked by the leaf's final-recovery escalation.
    Escalated,
    /// Requeued to the backlog by the leaf's final recovery.
    Requeued,
    /// Blocked by an operator.
    OperatorHold,
    /// Blocked by the leaf's escalation, on a task tagged `no-auto-approve`.
    OptedOut,
}

struct Leaf {
    task: String,
    run: String,
    head: String,
    number: u64,
}

fn fixture() -> Fixture {
    let fx = Fixture::new();
    let jobs = fx.activities.parent().unwrap().join("jobs");
    std::fs::write(jobs.join("task_pr_pipeline.yaml"), repair_leaf_job()).unwrap();
    fx
}

impl Fixture {
    /// A task whose delivery leaf, started with `completion`, opened and
    /// promoted pull request `number` on its Orbit branch, then failed in
    /// completion and left the task on `shelf`.
    fn stopped_leaf(&self, number: u64, completion: &str, shelf: Shelf) -> Leaf {
        let task = self
            .runtime
            .add_task(TaskAddParams {
                title: "Stopped delivery".into(),
                description: "A delivery whose completion step gave up on its PR.".into(),
                acceptance_criteria: vec!["Its PR lands.".into()],
                plan: "Deliver it.".into(),
                status: Some(TaskStatus::Backlog),
                tags: matches!(shelf, Shelf::OptedOut)
                    .then(|| "no-auto-approve".to_string())
                    .into_iter()
                    .collect(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let head = format!("orbit/{task}-leaf");
        let input = json!({"task_ids": [task], "completion": completion, "auto_fix": false});
        let run = self
            .jobs
            .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input.clone()), None)
            .unwrap()
            .run_id;
        let mut state = PipelineState::new(run.clone(), "task_pr_pipeline".into(), input);
        let outputs = [
            (
                "worktree",
                json!({"job_run_id": run, "batch_id": run, "task_ids": [task]}),
            ),
            ("push", json!({"phase": "push", "branch": head})),
            (
                "pr_open",
                json!({"phase": "pr_open", "decision": "performed",
                    "pr_number": number.to_string(), "head": head, "base": "main"}),
            ),
            (
                "promote_tasks",
                json!({"phase": "promote", "performed_task_ids": [task],
                    "pr_number": number.to_string()}),
            ),
        ];
        for (index, (id, output)) in outputs.iter().enumerate() {
            state.record_step(
                u32::try_from(index).unwrap(),
                JobRunState::Success,
                Some(output.clone()),
                None,
            );
            state.record_pipeline_output(id, output.clone());
            self.step(&run, index, id, JobRunState::Success, None);
        }
        state.record_step(4, JobRunState::Failed, None, None);
        self.jobs.write_run_state(&run, &state).unwrap();
        self.step(
            &run,
            4,
            "complete_pr",
            JobRunState::Failed,
            Some("pr_checks_failed"),
        );
        self.fail(&run);
        self.runtime
            .apply_task_automation_update(
                &task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Review),
                    job_run_id: Some(run.clone()),
                    external_refs: vec![ExternalRef::github_pr(number.to_string()).unwrap()],
                    ..Default::default()
                },
            )
            .unwrap();
        let shelved = match shelf {
            Shelf::Review => None,
            Shelf::Escalated | Shelf::OptedOut => Some((
                TaskStatus::Blocked,
                "final_recovery_escalated",
                format!("final recovery (run_id={run}) escalated: the PR stayed red"),
            )),
            Shelf::Requeued => Some((
                TaskStatus::Backlog,
                "final_recovery_requeued",
                format!("final recovery (run_id={run}): the PR conflicts with its base"),
            )),
            Shelf::OperatorHold => Some((
                TaskStatus::Blocked,
                "task_blocked",
                "waiting on the vendor's release".to_string(),
            )),
        };
        if let Some((status, event, note)) = shelved {
            self.runtime
                .apply_task_automation_update(
                    &task,
                    TaskAutomationUpdate {
                        status: Some(status),
                        status_event: Some(event.into()),
                        status_note: Some(note),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        Leaf {
            task,
            run,
            head,
            number,
        }
    }

    /// `run` ends `failed`, as a completion that gave up does.
    fn fail(&self, run: &str) {
        let current = self.jobs.get_job_run(run).unwrap().unwrap();
        if current.state == JobRunState::Pending {
            self.jobs
                .mark_job_run_running(run, Utc::now(), std::process::id())
                .unwrap();
        }
        self.jobs
            .finalize_job_run(run, JobRunState::Failed, Utc::now(), None)
            .unwrap();
    }

    /// A delivery run of `input` that ended `success`, as an ordinary
    /// delivery does.
    fn finish_delivery(&self, input: Value) -> String {
        let run = self
            .jobs
            .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input), None)
            .unwrap()
            .run_id;
        self.jobs
            .mark_job_run_running(&run, Utc::now(), std::process::id())
            .unwrap();
        self.jobs
            .finalize_job_run(&run, JobRunState::Success, Utc::now(), None)
            .unwrap();
        run
    }

    /// The forge reports `leaf`'s pull request open at `head_sha`, opened by
    /// `author`, conflicting and with `failed` required checks.
    fn blocked(&self, leaf: &Leaf, head_sha: &str, author: &str, conflict: bool, failed: &[&str]) {
        self.forge.0.lock().unwrap().blockers.insert(
            leaf.number,
            PullRequestBlockers {
                pull: ForgePullRequest {
                    number: leaf.number,
                    head_branch: leaf.head.clone(),
                    body: format!("Delivers {}.", leaf.task),
                    author: author.to_string(),
                },
                head_sha: head_sha.to_string(),
                conflicting: conflict,
                failed_required_checks: failed.iter().map(|name| name.to_string()).collect(),
            },
        );
    }

    /// Make the selected PR use the production GraphQL parser and report
    /// GitHub's still-calculating mergeability answer.
    #[cfg(unix)]
    fn mergeability_unknown(&self, leaf: &Leaf) {
        self.forge.use_gh_for_blockers(leaf.number);
        let home = std::env::var_os("HOME").expect("scenario HOME");
        let bin = std::path::PathBuf::from(home).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let answer = bin.join("unknown-mergeability.json");
        std::fs::write(
            &answer,
            json!({"data":{"repository":{"pullRequest":{
                "state":"OPEN",
                "body":format!("Delivers {}.", leaf.task),
                "author":{"login":"orbit-bot"},
                "headRefName":&leaf.head,
                "headRefOid":"unknown-head",
                "mergeable":"UNKNOWN",
                "commits":{"nodes":[{"commit":{"oid":"unknown-head",
                    "statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},"nodes":[]}}
                }}]}
            }}}})
            .to_string(),
        )
        .unwrap();
        let quoted = answer.to_string_lossy().replace('\'', "'\\''");
        let gh = bin.join("gh");
        std::fs::write(
            &gh,
            format!("#!/bin/sh\n[ \"$1 $2\" = \"api graphql\" ] || exit 99\ncat '{quoted}'\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn started<'a>(pass: &'a Value, task: &str) -> Option<&'a Value> {
    pass["auto_fix"]["started"]
        .as_array()?
        .iter()
        .find(|entry| entry["task_id"] == task)
}

fn repair_skip(pass: &Value, task: &str) -> Option<String> {
    pass["auto_fix"]["skipped"]
        .as_array()?
        .iter()
        .find(|entry| entry["task_id"] == task)
        .map(|entry| entry["reason"].as_str().unwrap().to_string())
}

fn a_conflicting_pull_request_its_delivery_blocked_is_handed_back_for_an_auto_fix() {
    let fx = fixture();
    let leaf = fx.stopped_leaf(81, "review", Shelf::Escalated);
    fx.blocked(&leaf, "aaa1", "orbit-bot", true, &[]);
    let drain = fx.drain();

    // A drain started without `--auto-fix` never repairs.
    let plain = fx.pass(&drain, "review", false);
    assert!(plain.get("auto_fix").is_none(), "{plain}");
    assert!(fx.retries(&leaf.run).is_empty());

    let pass = fx.pass(&drain, "review", true);
    let entry = started(&pass, &leaf.task).unwrap_or_else(|| panic!("{pass}"));
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
    let seeded = fx.jobs.read_run_state(&resumed.run_id).unwrap().unwrap();
    for index in 0..4 {
        assert_eq!(
            seeded.step_states.get(&index),
            Some(&JobRunState::Success),
            "step {index} is reused"
        );
    }
    assert!(!seeded.step_states.contains_key(&4), "completion reruns");
    assert_eq!(seeded.next_step_index, 4);

    let task = fx.runtime.get_task(&leaf.task).unwrap();
    assert_eq!(
        task.status,
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
    let readiness = fx
        .runtime
        .workspace_auto_readiness(&[], None, 10, &[])
        .unwrap();
    let live = readiness["out_of_run_auto_fix"].as_array().unwrap();
    assert_eq!(live.len(), 1, "{readiness}");
    assert_eq!(live[0]["run_id"], resumed.run_id);
    assert_eq!(live[0]["pr_number"], "81");

    // The resumed leaf is live, so a later pass starts nothing more.
    let again = fx.pass(&drain, "review", true);
    assert_eq!(again["auto_fix"]["started"], json!([]), "{again}");
    assert_eq!(fx.retries(&leaf.run).len(), 1);
}

fn skipped_repairs_are_checkpointed_and_visible_in_readiness() {
    let fx = fixture();
    let spent = fx.stopped_leaf(89, "review", Shelf::Review);
    fx.blocked(&spent, "spent-head", "orbit-bot", true, &[]);
    // A published leaf that failed before completion cannot be resumed there.
    let mut state = fx.jobs.read_run_state(&spent.run).unwrap().unwrap();
    state.record_step(3, JobRunState::Failed, None, None);
    fx.jobs.write_run_state(&spent.run, &state).unwrap();
    let held = fx.stopped_leaf(90, "review", Shelf::OperatorHold);
    fx.blocked(&held, "held-head", "orbit-bot", false, &["Test"]);
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
        let skip = output["auto_fix"]["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["task_id"] == leaf.task)
            .unwrap();
        assert_eq!(skip["reason"], reason, "{output}");
        assert_eq!(skip["pr_number"], leaf.number.to_string());
        assert_eq!(skip["blocker"], blocker);
    }
    let readiness = fx
        .runtime
        .workspace_auto_readiness(&[], None, 10, &[])
        .unwrap();
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
    let readiness = fx
        .runtime
        .workspace_auto_readiness(&[], None, 10, &[])
        .unwrap();
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
    let ended = fx
        .runtime
        .workspace_auto_readiness(&[], None, 10, &[])
        .unwrap();
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
    fx.blocked(&leaf, "capacity-head", "orbit-bot", true, &[]);
    let drain = fx.drain();
    let pass = fx.runtime.run_deterministic(
        "adopt_orphaned_deliveries", &json!({}),
        &json!({"run_id": drain, "max_active_leaf_runs": 0, "completion": "review", "auto_fix": true}),
        ToolContext::default(),
    ).unwrap();
    assert_eq!(
        repair_skip(&pass, &leaf.task).as_deref(),
        Some("no_capacity"),
        "{pass}"
    );
    assert_eq!(
        fx.forge.0.lock().unwrap().blocker_reads,
        0,
        "a held pass does not spend forge reads"
    );
    assert!(fx.retries(&leaf.run).is_empty());

    let newer = fx.stopped_leaf(79, "review", Shelf::Review);
    fx.blocked(&newer, "newer-head", "orbit-bot", false, &["Test"]);
    let pass = fx.runtime.run_deterministic(
        "adopt_orphaned_deliveries", &json!({}),
        &json!({"run_id": drain, "max_active_leaf_runs": 1, "completion": "review", "auto_fix": true}),
        ToolContext::default(),
    ).unwrap();
    assert!(started(&pass, &newer.task).is_some(), "{pass}");
    assert_eq!(
        repair_skip(&pass, &leaf.task).as_deref(),
        Some("no_capacity"),
        "the scan continues after its last slot is taken: {pass}"
    );
    let reads = fx.forge.0.lock().unwrap().blocker_reads;
    fx.probe.sustain_memory(&fx.runtime, 95.0);
    let pass = fx.pass(&drain, "review", true);
    assert_eq!(
        repair_skip(&pass, &leaf.task).as_deref(),
        Some("resource_throttled"),
        "{pass}"
    );
    assert_eq!(
        fx.forge.0.lock().unwrap().blocker_reads,
        reads,
        "pressure holds repairs without more forge reads"
    );
}

fn red_checks_and_review_or_requeued_tasks_are_repaired_too() {
    let fx = fixture();
    let red = fx.stopped_leaf(82, "review", Shelf::Escalated);
    fx.blocked(&red, "bbb1", "orbit-bot", false, &["Test"]);
    let in_review = fx.stopped_leaf(83, "done", Shelf::Review);
    fx.blocked(&in_review, "bbb2", "orbit-bot", false, &["Clippy"]);
    let requeued = fx.stopped_leaf(84, "review", Shelf::Requeued);
    fx.blocked(&requeued, "bbb3", "orbit-bot", true, &[]);
    let drain = fx.drain();

    let pass = fx.pass(&drain, "done", true);
    for (leaf, blocker) in [
        (&red, "failed_checks"),
        (&in_review, "failed_checks"),
        (&requeued, "conflict"),
    ] {
        let entry = started(&pass, &leaf.task).unwrap_or_else(|| panic!("{pass}"));
        assert_eq!(entry["blocker"], blocker, "{pass}");
        assert_eq!(fx.retries(&leaf.run).len(), 1, "{}", leaf.task);
        assert_eq!(
            fx.runtime.get_task(&leaf.task).unwrap().status,
            TaskStatus::Review,
            "{}",
            leaf.task
        );
    }
    let resumed = &fx.retries(&red.run)[0];
    assert_eq!(
        resumed.input.as_ref().unwrap()["out_of_run_auto_fix"]["failed_checks"],
        json!(["Test"])
    );
}

#[cfg(unix)]
fn unknown_mergeability_on_the_newest_pull_request_does_not_defer_older_repairs() {
    let fx = fixture();
    let older_conflicting = fx.stopped_leaf(85, "review", Shelf::Escalated);
    fx.blocked(&older_conflicting, "ddd1", "orbit-bot", true, &[]);
    let newest_unknown = fx.stopped_leaf(86, "review", Shelf::Escalated);
    fx.mergeability_unknown(&newest_unknown);
    let drain = fx.drain();

    let pass = fx.pass(&drain, "review", true);

    assert_eq!(
        repair_skip(&pass, &newest_unknown.task).as_deref(),
        Some("mergeability_unknown"),
        "the unknown answer is a per-pull-request skip: {pass}"
    );
    let repaired = started(&pass, &older_conflicting.task).unwrap_or_else(|| panic!("{pass}"));
    assert_eq!(repaired["blocker"], "conflict", "{pass}");
    assert_eq!(fx.retries(&older_conflicting.run).len(), 1);
    assert!(fx.retries(&newest_unknown.run).is_empty());
}

fn a_genuine_forge_failure_still_defers_later_blocker_reads() {
    let fx = fixture();
    let older = fx.stopped_leaf(87, "review", Shelf::Escalated);
    fx.blocked(&older, "eee1", "orbit-bot", true, &[]);
    let newest = fx.stopped_leaf(88, "review", Shelf::Escalated);
    fx.blocked(&newest, "eee2", "orbit-bot", true, &[]);
    fx.forge.0.lock().unwrap().unreachable = true;
    let drain = fx.drain();

    let pass = fx.pass(&drain, "review", true);

    for leaf in [&newest, &older] {
        assert_eq!(
            repair_skip(&pass, &leaf.task).as_deref(),
            Some("forge_unreadable"),
            "a real forge failure defers this pass: {pass}"
        );
        assert!(fx.retries(&leaf.run).is_empty());
    }
    assert_eq!(fx.forge.0.lock().unwrap().blocker_reads, 1);
}

fn holds_opt_outs_missing_grants_and_foreign_pull_requests_are_never_repaired() {
    let fx = fixture();
    let held = fx.stopped_leaf(91, "review", Shelf::OperatorHold);
    fx.blocked(&held, "ccc1", "orbit-bot", true, &[]);
    let opted_out = fx.stopped_leaf(92, "review", Shelf::OptedOut);
    fx.blocked(&opted_out, "ccc2", "orbit-bot", true, &[]);
    let foreign = fx.stopped_leaf(93, "review", Shelf::Review);
    fx.blocked(&foreign, "ccc3", "someone-else", true, &[]);
    let wants_merge = fx.stopped_leaf(94, "done", Shelf::Escalated);
    fx.blocked(&wants_merge, "ccc4", "orbit-bot", true, &[]);
    let clean = fx.stopped_leaf(95, "review", Shelf::Review);
    fx.blocked(&clean, "ccc5", "orbit-bot", false, &[]);
    let drain = fx.drain();

    let pass = fx.pass(&drain, "review", true);
    assert_eq!(pass["auto_fix"]["started"], json!([]), "{pass}");
    for (leaf, reason) in [
        (&held, "operator_hold"),
        (&opted_out, "opted_out"),
        (&foreign, "not_orbit_pull_request"),
        (&wants_merge, "authority"),
        (&clean, "no_blocker"),
    ] {
        assert_eq!(
            repair_skip(&pass, &leaf.task).as_deref(),
            Some(reason),
            "{pass}"
        );
        assert!(fx.retries(&leaf.run).is_empty(), "{}", leaf.task);
    }
    assert_eq!(
        fx.runtime.get_task(&held.task).unwrap().status,
        TaskStatus::Blocked,
        "an operator's block stays"
    );
}

fn the_pull_requests_auto_fix_budget_bounds_out_of_run_repairs() {
    let fx = fixture();
    // A pull request whose in-run auto-fix already spent its three repairs.
    let spent = fx.stopped_leaf(101, "review", Shelf::Escalated);
    fx.blocked(&spent, "ddd9", "orbit-bot", true, &[]);
    let attempts = (1..=3)
        .map(|attempt| orbit_types::task::TaskHistoryEntry {
            at: Utc::now(),
            by: "system".into(),
            event: PR_AUTO_FIX_ADMITTED_EVENT.into(),
            note: Some(
                json!({"pr_number": "101", "head_sha": format!("ddd{attempt}"),
                    "blocker": "failed_checks", "attempt": attempt, "budget": 3,
                    "run_id": spent.run, "crew": "fixer", "crew_source": "pool"})
                .to_string(),
            ),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        })
        .collect();
    fx.runtime
        .apply_task_automation_update(
            &spent.task,
            TaskAutomationUpdate {
                append_history: attempts,
                ..Default::default()
            },
        )
        .unwrap();
    // A pull request handed back once whose resumed leaf failed again on the
    // same head and blocker.
    let looped = fx.stopped_leaf(102, "review", Shelf::Escalated);
    fx.blocked(&looped, "eee1", "orbit-bot", true, &[]);
    let drain = fx.drain();
    let first = fx.pass(&drain, "review", true);
    assert!(started(&first, &looped.task).is_some(), "{first}");
    assert_eq!(
        repair_skip(&first, &spent.task).as_deref(),
        Some("budget_exhausted"),
        "{first}"
    );
    let resumed = fx.retries(&looped.run)[0].run_id.clone();
    fx.fail(&resumed);
    fx.runtime
        .apply_task_automation_update(
            &looped.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Blocked),
                status_event: Some("final_recovery_escalated".into()),
                status_note: Some(format!(
                    "final recovery (run_id={resumed}) escalated: the conflict stayed"
                )),
                ..Default::default()
            },
        )
        .unwrap();

    let again = fx.pass(&drain, "review", true);
    assert_eq!(
        repair_skip(&again, &looped.task).as_deref(),
        Some("already_started"),
        "{again}"
    );
    assert!(fx.retries(&resumed).is_empty(), "one hand-back per head");

    // A new head is a new repair.
    fx.blocked(&looped, "eee2", "orbit-bot", true, &[]);
    let next = fx.pass(&drain, "review", true);
    assert!(started(&next, &looped.task).is_some(), "{next}");
    assert_eq!(fx.retries(&resumed).len(), 1);
}

fn a_stuck_pull_request_is_repaired_however_many_deliveries_ran_since() {
    let fx = fixture();
    let leaf = fx.stopped_leaf(110, "review", Shelf::Escalated);
    fx.blocked(&leaf, "old-head", "orbit-bot", true, &[]);
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
    assert_eq!(fx.retries(&leaf.run).len(), 1, "{pass}");
}

fn a_newer_finished_delivery_of_the_task_keeps_its_older_stopped_leaf_from_resuming() {
    let fx = fixture();
    let leaf = fx.stopped_leaf(111, "review", Shelf::Review);
    fx.blocked(&leaf, "stale-head", "orbit-bot", true, &[]);
    fx.finish_delivery(json!({"task_ids": [leaf.task], "completion": "review"}));
    for index in 0..201 {
        fx.finish_delivery(json!({"task_ids": [format!("ORB-8{index}")], "completion": "review"}));
    }
    let drain = fx.drain();
    let pass = fx.pass(&drain, "review", true);
    assert!(started(&pass, &leaf.task).is_none(), "{pass}");
    assert_eq!(repair_skip(&pass, &leaf.task), None, "{pass}");
    assert!(fx.retries(&leaf.run).is_empty());
}

crate::scenario_table!(
    out_of_run_auto_fix_scenarios,
    a_conflicting_pull_request_its_delivery_blocked_is_handed_back_for_an_auto_fix,
    skipped_repairs_are_checkpointed_and_visible_in_readiness,
    full_capacity_still_reports_the_pull_requests_left_alone,
    red_checks_and_review_or_requeued_tasks_are_repaired_too,
    a_genuine_forge_failure_still_defers_later_blocker_reads,
    holds_opt_outs_missing_grants_and_foreign_pull_requests_are_never_repaired,
    the_pull_requests_auto_fix_budget_bounds_out_of_run_repairs,
    a_stuck_pull_request_is_repaired_however_many_deliveries_ran_since,
    a_newer_finished_delivery_of_the_task_keeps_its_older_stopped_leaf_from_resuming,
);

#[cfg(unix)]
crate::scenario_table!(
    unknown_mergeability_scenarios,
    unknown_mergeability_on_the_newest_pull_request_does_not_defer_older_repairs,
);
