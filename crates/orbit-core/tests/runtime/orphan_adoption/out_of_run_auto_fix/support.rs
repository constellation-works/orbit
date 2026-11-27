//! Harness for the out-of-run auto-fix scenarios: the delivery-leaf fixture
//! and its stored history, the fake forge's blocker answers, and the
//! pass-output and readiness assertions. The scenarios live in the parent
//! module, `out_of_run_auto_fix.rs`.

use orbit_core::application::job::{TaskAutoFixError, TaskAutoFixRefusal};
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, PullRequestBlockers};
use orbit_engine::RunLifecycle;
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{DeliveryRefusal, PR_AUTO_FIX_ADMITTED_EVENT, SOURCE_REPAIR_RESERVED};

use super::super::*;

pub(in super::super) const BOT: &str = "orbit-bot";
pub(in super::super) const PUBLISHED: &str = "published-head";
pub(in super::super) const INCONCLUSIVE: &str = "delivery_check_evidence_inconclusive";
pub(in super::super) const SPENT_1: &str = "repair attempts: 1 of 3 spent, 2 remaining";
pub(in super::super) const SPENT_2: &str = "repair attempts: 2 of 3 spent, 1 remaining";

/// The shipped leaf: the worktree names the batch, `pr_promote` moves the
/// task to review, `pr_complete` watches the pull request.
pub(in super::super) const SHIPPED_STEPS: [(&str, &str); 5] = [
    ("worktree", "pipeline_success_guard"),
    ("push", "git_push"),
    ("pr_open", "pr_open"),
    ("promote_tasks", "pr_promote"),
    ("complete_pr", "pr_complete"),
];

/// The shipped leaf after a reviewed head conflicted: each re-review round
/// rebases the head, and the next round completes the head it republished.
pub(in super::super) const REREVIEWED_STEPS: [(&str, &str); 9] = [
    ("worktree", "pipeline_success_guard"),
    ("push", "git_push"),
    ("pr_open", "pr_open"),
    ("promote_tasks", "pr_promote"),
    ("complete_pr", "pr_complete"),
    ("re_push", "git_push"),
    ("complete_reviewed_pr", "pr_complete"),
    ("re_push_2", "git_push"),
    ("complete_reviewed_pr_2", "pr_complete"),
];

/// How a delivered leaf ended.
#[derive(Clone, Copy)]
pub(in super::super) enum LeafEnd {
    /// Its completion step failed.
    Failed,
    /// It succeeded at review without running completion.
    Review,
    /// Its completion step held the run with this delivery refusal code.
    Held(&'static str),
    /// Its completion step stopped auto-fix on a head that moved.
    HeldAutoFixStop,
    /// An auto-fix stop on a required-checks answer naming an older commit.
    HeldStaleCheckAnswer,
    /// An admitted check repair ran, published nothing, and held the run.
    HeldUnrepairedCheckRepair,
}

/// How a stopped leaf left its task.
#[derive(Clone, Copy)]
pub(in super::super) enum Shelf {
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

pub(in super::super) struct Leaf {
    pub(in super::super) task: String,
    pub(in super::super) run: String,
    pub(in super::super) head: String,
    pub(in super::super) number: u64,
}

pub(in super::super) fn fixture() -> Fixture {
    fixture_with(&SHIPPED_STEPS)
}

pub(in super::super) fn rereviewed_fixture() -> Fixture {
    fixture_with(&REREVIEWED_STEPS)
}

/// A fixture whose `task_pr_pipeline` job runs the deterministic `steps`.
pub(in super::super) fn fixture_with(steps: &[(&str, &str)]) -> Fixture {
    let fx = Fixture::new();
    let steps: Vec<Value> = steps
        .iter()
        .map(|(id, action)| {
            json!({"id": id, "spec": {"type": "deterministic", "action": action, "config": {}}})
        })
        .collect();
    let job = json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pr_pipeline"},
    "spec": {"state": "enabled", "kind": "workflow", "task_delivery": {}, "steps": steps}})
    .to_string();
    let jobs = fx.activities.parent().unwrap().join("jobs");
    std::fs::write(jobs.join("task_pr_pipeline.yaml"), job).unwrap();
    fx
}

impl Fixture {
    /// A delivery leaf that opened and promoted pull request `number`, then
    /// failed in completion and left the task on `shelf`.
    pub(in super::super) fn stopped_leaf(
        &self,
        number: u64,
        completion: &str,
        shelf: Shelf,
    ) -> Leaf {
        self.delivered_leaf(number, completion, shelf, LeafEnd::Failed)
    }

    /// A delivery leaf started without `--complete` that succeeded at review.
    pub(in super::super) fn review_leaf(&self, number: u64) -> Leaf {
        self.delivered_leaf(number, "review", Shelf::Review, LeafEnd::Review)
    }

    /// A delivery leaf whose completion held as an unrepaired check repair.
    pub(in super::super) fn unrepaired_leaf(
        &self,
        number: u64,
        completion: &str,
        shelf: Shelf,
    ) -> Leaf {
        self.delivered_leaf(
            number,
            completion,
            shelf,
            LeafEnd::HeldUnrepairedCheckRepair,
        )
    }

    pub(in super::super) fn delivered_leaf(
        &self,
        number: u64,
        completion: &str,
        shelf: Shelf,
        end: LeafEnd,
    ) -> Leaf {
        self.delivered_leaf_after(None, number, completion, shelf, end)
    }

    /// As [`Self::delivered_leaf`], for a task that already records the closed
    /// pull request `earlier` from a discarded candidate, so `number` is its
    /// second recorded `github-pr`.
    pub(in super::super) fn delivered_leaf_after(
        &self,
        earlier: Option<u64>,
        number: u64,
        completion: &str,
        shelf: Shelf,
        end: LeafEnd,
    ) -> Leaf {
        let tags = matches!(shelf, Shelf::OptedOut).then(|| "no-auto-approve".to_string());
        let task = self
            .runtime
            .add_task(TaskAddParams {
                title: "Stopped delivery".into(),
                description: "A delivery whose completion step gave up on its PR.".into(),
                acceptance_criteria: vec!["Its PR lands.".into()],
                plan: "Deliver it.".into(),
                status: Some(TaskStatus::Backlog),
                external_refs: earlier
                    .map(|number| ExternalRef::github_pr(number.to_string()).unwrap())
                    .into_iter()
                    .collect(),
                tags: tags.into_iter().collect(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let head = format!("orbit/{task}-0123abcd");
        let input = json!({"task_ids": [task], "completion": completion, "auto_fix": false});
        let run = self
            .jobs
            .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input.clone()), None)
            .unwrap()
            .run_id;
        let mut state = PipelineState::new(run.clone(), "task_pr_pipeline".into(), input);
        let pr = number.to_string();
        let outputs = [
            (
                "worktree",
                json!({"job_run_id": run, "batch_id": run, "task_ids": [task]}),
            ),
            (
                "push",
                json!({"phase": "push", "branch": head, "local_sha": PUBLISHED}),
            ),
            (
                "pr_open",
                json!({"phase": "pr_open", "decision": "performed",
                    "pr_number": pr, "head": head, "base": "main"}),
            ),
            (
                "promote_tasks",
                json!({"phase": "promote", "performed_task_ids": [task], "pr_number": pr}),
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
        if matches!(end, LeafEnd::Review) {
            // A review delivery skips `complete_pr` and ends `success`.
            state.record_step(4, JobRunState::Skipped, None, None);
        }
        self.jobs.write_run_state(&run, &state).unwrap();
        let stop = |code: DeliveryRefusal, message: String| {
            self.hold(&run, 4, code.error_code(), &message)
        };
        match end {
            // Production checkpoints only a step that succeeded, so the failed
            // `complete_pr` leaves no entry in `step_states`.
            LeafEnd::Failed => {
                self.step(
                    &run,
                    4,
                    "complete_pr",
                    JobRunState::Failed,
                    Some("pr_checks_failed"),
                );
                self.finish(&run, JobRunState::Failed);
            }
            LeafEnd::Review => {
                self.step(&run, 4, "complete_pr", JobRunState::Skipped, None);
                self.finish(&run, JobRunState::Success);
            }
            LeafEnd::Held(code) => self.hold(&run, 4, code, "worker process exited"),
            LeafEnd::HeldAutoFixStop => stop(
                DeliveryRefusal::HeadChanged,
                format!(
                    "deterministic action `complete_pr` refused: auto_fix_stopped: pull \
                     request #{number} cannot be repaired automatically (required checks \
                     name another head); no repair agent runs, the pull request stays open \
                     and the task stays in review"
                ),
            ),
            LeafEnd::HeldStaleCheckAnswer => stop(
                DeliveryRefusal::CheckEvidenceInconclusive,
                format!(
                    "deterministic action `complete_pr` refused: auto_fix_stopped: pull \
                     request #{number} cannot be repaired automatically (its required \
                     checks were read on commit old-head while the pull request still \
                     reports head published-head; no repair is admitted); no repair agent \
                     runs, the pull request stays open and the task stays in review"
                ),
            ),
            LeafEnd::HeldUnrepairedCheckRepair => stop(
                DeliveryRefusal::RepairDispatched,
                format!(
                    "deterministic action `complete_pr` refused: admitted check repair ran \
                     step_failure_recovery and it did not repair the head; the run holds. \
                     The admission remains recorded and spends one repair attempt. Inspect \
                     the recovery attempt; because this held run is terminal, starting a new \
                     authorized delivery run on the same head admits another repair attempt \
                     while per-PR budget remains, or repair and publish a new head, then start \
                     a new delivery attempt. Exhausting the per-PR budget fails the run. \
                     pr_auto_fix_check_repair: pull request #{number} failed required checks \
                     on head published-head; auto-fix repair attempt 1 of 3 runs as crew \
                     'fixer' (drawn from workflow.purpose_crews.auto_fix)."
                ),
            ),
        }
        apply_update(
            &self.runtime,
            &task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                job_run_id: Some(run.clone()),
                external_refs: vec![ExternalRef::github_pr(pr).unwrap()],
                ..Default::default()
            },
        );
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
            apply_update(
                &self.runtime,
                &task,
                TaskAutomationUpdate {
                    status: Some(status),
                    status_event: Some(event.into()),
                    status_note: Some(note),
                    ..Default::default()
                },
            );
        }
        Leaf {
            task,
            run,
            head,
            number,
        }
    }

    /// Marks `run` running if it is still pending.
    pub(in super::super) fn mark_running(&self, run: &str) {
        let current = self.jobs.get_job_run(run).unwrap().unwrap();
        if current.state == JobRunState::Pending {
            self.jobs
                .mark_job_run_running(run, Utc::now(), std::process::id())
                .unwrap();
        }
    }

    /// Ends `run` as `state` now, after marking it running.
    pub(in super::super) fn finish(&self, run: &str, state: JobRunState) {
        self.mark_running(run);
        self.jobs
            .finalize_job_run(run, state, Utc::now(), None)
            .unwrap();
    }

    /// `run`'s completion step at `index` holds it with the delivery refusal
    /// `code`, an hour ago, as a fail-closed completion decision does.
    pub(in super::super) fn hold(&self, run: &str, index: usize, code: &str, message: &str) {
        self.mark_running(run);
        self.step_with_message(
            run,
            index,
            "complete_pr",
            JobRunState::Failed,
            Some(code),
            Some(message),
        );
        self.jobs
            .finalize_job_run(
                run,
                JobRunState::Held,
                Utc::now() - chrono::TimeDelta::hours(1),
                None,
            )
            .unwrap();
    }

    /// A delivery run that ended `success`, as an ordinary delivery does.
    pub(in super::super) fn finish_delivery(&self, input: Value) -> String {
        let run = self
            .jobs
            .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input), None)
            .unwrap()
            .run_id;
        self.finish(&run, JobRunState::Success);
        run
    }

    /// Replaces the forge's blocker answer for `leaf`'s pull request with a
    /// row at `head_sha` by `author`, with no check or conflict reported, then
    /// applies `edit` to it.
    pub(in super::super) fn set_row(
        &self,
        leaf: &Leaf,
        head_sha: &str,
        author: &str,
        edit: impl FnOnce(&mut PullRequestBlockers),
    ) {
        let mut row = PullRequestBlockers {
            pull: ForgePullRequest {
                number: leaf.number,
                head_branch: leaf.head.clone(),
                body: format!("Delivers {}.", leaf.task),
                author: author.to_string(),
            },
            head_sha: head_sha.to_string(),
            conflicting: false,
            failed_required_checks: Vec::new(),
            pending_required_checks: Vec::new(),
            passed_required_checks: Vec::new(),
            unreported_required_checks: Vec::new(),
            requires_status_checks: None,
        };
        edit(&mut row);
        self.forge
            .0
            .lock()
            .unwrap()
            .blockers
            .insert(leaf.number, row);
    }

    /// The forge reports `leaf`'s open pull request at `head_sha`, opened by
    /// `author`, conflicting and with `failed` required checks.
    pub(in super::super) fn blocked(
        &self,
        leaf: &Leaf,
        head_sha: &str,
        author: &str,
        conflict: bool,
        failed: &[&str],
    ) {
        self.set_row(leaf, head_sha, author, |row| {
            row.conflicting = conflict;
            row.failed_required_checks = names(failed);
        });
    }

    /// The forge reports required checks on `leaf`'s open pull request at
    /// `head_sha`: `passed` have finished and `pending` are still running.
    pub(in super::super) fn report_required_checks(
        &self,
        leaf: &Leaf,
        head_sha: &str,
        author: &str,
        passed: &[&str],
        pending: &[&str],
    ) {
        self.set_row(leaf, head_sha, author, |row| {
            row.passed_required_checks = names(passed);
            row.pending_required_checks = names(pending);
            row.requires_status_checks = Some(true);
        });
    }

    /// The forge reports `leaf`'s open pull request at `head_sha`, with
    /// `failed`, `passed` and `unreported` required checks. A name in
    /// `unreported` is configured by the base branch and has not reported on the
    /// head at all.
    pub(in super::super) fn report_partial_required_checks(
        &self,
        leaf: &Leaf,
        head_sha: &str,
        failed: &[&str],
        passed: &[&str],
        unreported: &[&str],
    ) {
        self.set_row(leaf, head_sha, BOT, |row| {
            row.failed_required_checks = names(failed);
            row.passed_required_checks = names(passed);
            row.unreported_required_checks = names(unreported);
            row.requires_status_checks = Some(true);
        });
    }

    /// The forge reports that `leaf`'s base branch configures no required checks.
    pub(in super::super) fn report_no_required_checks(
        &self,
        leaf: &Leaf,
        head_sha: &str,
        author: &str,
    ) {
        self.set_row(leaf, head_sha, author, |row| {
            row.requires_status_checks = Some(false);
        });
    }

    /// Makes the selected PR use the production GraphQL parser and report
    /// GitHub's still-calculating mergeability answer.
    #[cfg(unix)]
    pub(in super::super) fn mergeability_unknown(&self, leaf: &Leaf) {
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
                "author":{"login":BOT},
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

    /// Records `count` admitted repair attempts of `leaf`'s pull request on
    /// `head`, all by `run`.
    pub(in super::super) fn record_attempts(&self, leaf: &Leaf, head: &str, run: &str, count: u32) {
        let entries = (1..=count)
            .map(|attempt| {
                history_entry(
                    PR_AUTO_FIX_ADMITTED_EVENT,
                    json!({"pr_number": leaf.number.to_string(), "head_sha": head,
                        "blocker": "failed_checks", "attempt": attempt, "budget": 3,
                        "run_id": format!("{run}-{attempt}"), "crew": "fixer",
                        "crew_source": "pool"})
                    .to_string(),
                )
            })
            .collect();
        append_history(&self.runtime, &leaf.task, entries);
    }

    /// Records one source repair reservation of `leaf`'s pull request on
    /// `head`, as the source repair admission writes it. It spends the pull
    /// request's shared budget.
    pub(in super::super) fn record_source_repair_reservation(
        &self,
        leaf: &Leaf,
        head: &str,
        attempt: u32,
    ) {
        let (task, pr) = (&leaf.task, leaf.number.to_string());
        let note = json!({
            "request_key": format!("source-repair-{pr}-{head}"),
            "binding": {
                "schema_version": 1,
                "workspace_id": "workspace",
                "task_id": task,
                "source_run_id": "jrun-source",
                "machine_id": "machine",
                "source_digest": "source-digest",
                "task_digest": "task-digest",
                "repository": "owner/repo",
                "remote_url": "https://example.test/owner/repo.git",
                "branch": format!("orbit/{task}-0123abcd"),
                "pull_request": pr,
                "pull_request_url": format!("https://example.test/owner/repo/pull/{pr}"),
                "head_sha": head,
                "base_sha": "base-sha",
                "base_branch": "main",
            },
            "diagnosis": "fix the failed check",
            "authorized_by": "operator",
            "authorizer_provenance": "operator",
            "authorized_at": Utc::now(),
            "attempt": attempt,
            "review": {
                "contract_version": orbit_types::workflow::REVIEW_CONTRACT_VERSION,
                "policy_version": 1,
                "timing": "none",
                "timing_source": "global",
                "crew_source": "global",
                "budget": {"minutes": 30},
                "captured_at": Utc::now(),
            },
        });
        append_history(
            &self.runtime,
            task,
            vec![history_entry(SOURCE_REPAIR_RESERVED, note.to_string())],
        );
    }

    /// Appends an unparseable source repair reservation to `leaf`'s task.
    pub(in super::super) fn corrupt_source_ledger(&self, leaf: &Leaf) {
        append_history(
            &self.runtime,
            &leaf.task,
            vec![history_entry(SOURCE_REPAIR_RESERVED, "{not json".into())],
        );
    }

    /// The workspace's out-of-run auto-fix readiness, with every task's entry.
    pub(in super::super) fn readiness(&self) -> Value {
        self.runtime
            .workspace_auto_readiness(&[], None, 10, &[])
            .unwrap()
    }

    /// The single-task Fix PR readiness of `task`.
    pub(in super::super) fn fix_pr_readiness(&self, task: &str, completion_done: bool) -> Value {
        self.runtime
            .task_pr_auto_fix_readiness(task, completion_done)
            .unwrap()
    }

    /// Starts `task`'s repair from the dashboard, with no drain.
    pub(in super::super) fn dashboard_start(
        &self,
        task: &str,
        completion_done: bool,
    ) -> Result<Value, TaskAutoFixError> {
        self.runtime
            .start_task_pr_auto_fix(task, completion_done, Some("dashboard"))
    }

    /// Asserts that Fix PR readiness and the dashboard start both refuse `leaf`
    /// because its store is unreadable, with `with` in the detail.
    pub(in super::super) fn assert_fix_pr_unreadable(&self, leaf: &Leaf, with: &[&str]) {
        let readiness = self.fix_pr_readiness(&leaf.task, true);
        assert_eq!(readiness["available"], false, "{readiness}");
        assert_unreadable(&readiness, &leaf.number.to_string(), with, &[]);
        let refusal = refusal_of(&leaf.task, self.dashboard_start(&leaf.task, true));
        assert_eq!(refusal.reason(), "store_unreadable");
        assert_detail(&leaf.task, refusal.detail(), with, &[]);
    }

    /// The task's current status.
    pub(in super::super) fn status(&self, task: &str) -> TaskStatus {
        self.runtime.get_task(task).unwrap().status
    }

    /// The stored state of `run`.
    pub(in super::super) fn state(&self, run: &str) -> PipelineState {
        self.jobs.read_run_state(run).unwrap().unwrap()
    }

    /// The blocker reads the forge has answered so far.
    pub(in super::super) fn forge_reads(&self) -> usize {
        self.forge.0.lock().unwrap().blocker_reads
    }

    /// Asserts that `leaf` was resumed exactly `times` times.
    pub(in super::super) fn assert_resumed(&self, leaf: &Leaf, times: usize) {
        assert_eq!(
            self.retries(&leaf.run).len(),
            times,
            "{}: resumed runs",
            leaf.task
        );
    }

    /// Asserts that nothing resumed `leaf`.
    pub(in super::super) fn assert_not_resumed(&self, leaf: &Leaf) {
        assert!(
            self.retries(&leaf.run).is_empty(),
            "{}: resumed runs",
            leaf.task
        );
    }
}

/// Applies `update` to `task` through the runtime's automation path.
pub(in super::super) fn apply_update(
    runtime: &OrbitRuntime,
    task: &str,
    update: TaskAutomationUpdate,
) {
    runtime.apply_task_automation_update(task, update).unwrap();
}

/// A history entry of `event`, recorded now by the system, carrying `note`.
pub(in super::super) fn history_entry(event: &str, note: String) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: Utc::now(),
        by: "system".into(),
        event: event.into(),
        note: Some(note),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
        surface: None,
        caller: None,
    }
}

/// Appends `entries` to `task`'s history.
pub(in super::super) fn append_history(
    runtime: &OrbitRuntime,
    task: &str,
    entries: Vec<TaskHistoryEntry>,
) {
    apply_update(
        runtime,
        task,
        TaskAutomationUpdate {
            append_history: entries,
            ..Default::default()
        },
    );
}

pub(in super::super) fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| name.to_string()).collect()
}

pub(in super::super) fn started<'a>(pass: &'a Value, task: &str) -> Option<&'a Value> {
    pass["auto_fix"]["started"]
        .as_array()?
        .iter()
        .find(|entry| entry["task_id"] == task)
}

/// The start entry of `task` in `pass`, or a panic naming it.
pub(in super::super) fn started_of<'a>(pass: &'a Value, task: &str) -> &'a Value {
    started(pass, task).unwrap_or_else(|| panic!("{task} not started: {pass}"))
}

pub(in super::super) fn assert_started(pass: &Value, task: &str) {
    assert!(started(pass, task).is_some(), "{task}: {pass}");
}

/// Asserts that `task` started in `pass` with `key` equal to `want`.
pub(in super::super) fn assert_started_with(pass: &Value, task: &str, key: &str, want: &str) {
    assert_eq!(started_of(pass, task)[key], want, "{task}: {pass}");
}

pub(in super::super) fn assert_not_started(pass: &Value, task: &str) {
    assert!(started(pass, task).is_none(), "{task}: {pass}");
}

pub(in super::super) fn repair_skip(pass: &Value, task: &str) -> Option<String> {
    pass["auto_fix"]["skipped"]
        .as_array()?
        .iter()
        .find(|entry| entry["task_id"] == task)
        .map(|entry| entry["reason"].as_str().unwrap().to_string())
}

/// The skip entry of `task` in `pass`, or a panic naming it.
pub(in super::super) fn skip_of<'a>(pass: &'a Value, task: &str) -> &'a Value {
    pass["auto_fix"]["skipped"]
        .as_array()
        .unwrap_or_else(|| panic!("{pass}"))
        .iter()
        .find(|entry| entry["task_id"] == task)
        .unwrap_or_else(|| panic!("missing skip for {task}: {pass}"))
}

/// The `detail` of `task`'s skip in `pass`.
pub(in super::super) fn detail_of(pass: &Value, task: &str) -> String {
    skip_of(pass, task)["detail"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Asserts that `task` is skipped in `pass` with `reason`.
pub(in super::super) fn assert_skip(pass: &Value, task: &str, reason: &str) {
    assert_eq!(
        repair_skip(pass, task).as_deref(),
        Some(reason),
        "{task}: {pass}"
    );
}

/// Asserts that `task` is skipped in `pass` with `reason`, and that the skip's
/// detail holds every `with` fragment.
pub(in super::super) fn assert_skip_with(pass: &Value, task: &str, reason: &str, with: &[&str]) {
    assert_skip(pass, task, reason);
    assert_detail(task, &detail_of(pass, task), with, &[]);
}

/// Asserts that `case`'s `detail` holds every `with` fragment and no `without` one.
pub(in super::super) fn assert_detail(case: &str, detail: &str, with: &[&str], without: &[&str]) {
    for &fragment in with {
        assert!(
            detail.contains(fragment),
            "{case}: missing {fragment:?} in {detail:?}"
        );
    }
    for &fragment in without {
        assert!(
            !detail.contains(fragment),
            "{case}: unexpected {fragment:?} in {detail:?}"
        );
    }
}

/// Asserts that `entry` (a skip, or a readiness answer) is a `store_unreadable`
/// refusal of pull request `pr` whose detail holds `with` and not `without`.
pub(in super::super) fn assert_unreadable(
    entry: &Value,
    pr: &str,
    with: &[&str],
    without: &[&str],
) {
    assert_eq!(entry["reason"], "store_unreadable", "{entry}");
    assert_eq!(entry["pr_number"], pr, "{entry}");
    assert_detail(
        &format!("pull request #{pr}"),
        entry["detail"].as_str().unwrap_or_default(),
        with,
        without,
    );
}

/// The refusal a single-task start answered with, or a panic naming `task`.
pub(in super::super) fn refusal_of(
    task: &str,
    result: Result<Value, TaskAutoFixError>,
) -> TaskAutoFixRefusal {
    match result {
        Err(TaskAutoFixError::Refused(refusal)) => refusal,
        other => panic!("{task}: expected a refusal, got {other:?}"),
    }
}

/// Asserts that Fix PR readiness offers a repair for `blocker`.
pub(in super::super) fn assert_offers(readiness: &Value, blocker: &str) {
    assert_eq!(readiness["available"], true, "{readiness}");
    assert_eq!(readiness["blocker"], blocker, "{readiness}");
}
