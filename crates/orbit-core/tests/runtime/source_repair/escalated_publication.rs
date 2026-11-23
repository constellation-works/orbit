//! A run that published its own pull request, failed completion and was
//! escalated leaves its task blocked with that PR open and unmerged. Once
//! every required check has passed on the recorded head, an operator requeue
//! resumes the PR and the next run adopts it without re-implementing. Source
//! repair can authorize source changes separately. A moved head,
//! unfinished, failed, missing or stale checks, a closed or merged PR, a
//! foreign owner, a changed specification and a terminal task stay refused
//! with nothing written. A plain resume whose worktree is gone is refused
//! before the task moves.
use super::failed_publication::escalated_completion_fixture;
use super::handoff_requeue::{assert_next_run_resumes_the_published_candidate, refused, requeue};
use super::*;
use orbit_core::runtime::task_pr_forge::{
    ForgePullRequest, GhTaskPrForge, PullRequestBlockers, TaskPrForge,
};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// A forge reporting the fixture's PR 17 with the required checks a case
/// sets.
#[derive(Clone)]
pub(super) struct ChecksForge(Arc<Mutex<PullRequestBlockers>>);

impl TaskPrForge for ChecksForge {
    fn open_pull_requests(
        &self,
        _: &Path,
    ) -> Result<Vec<ForgePullRequest>, orbit_core::OrbitError> {
        Ok(vec![self.0.lock().unwrap().pull.clone()])
    }
    fn pull_request_blockers(
        &self,
        _: &Path,
        number: u64,
    ) -> Result<Option<PullRequestBlockers>, orbit_core::OrbitError> {
        let blockers = self.0.lock().unwrap().clone();
        Ok((blockers.pull.number == number).then_some(blockers))
    }
    fn authenticated_login(&self, _: &Path) -> Result<String, orbit_core::OrbitError> {
        Ok("orbit-bot".into())
    }
    fn close_pull_request(&self, _: &Path, _: u64, _: &str) -> Result<(), orbit_core::OrbitError> {
        Ok(())
    }
}

impl ChecksForge {
    pub(super) fn set(&self, head_sha: &str, failed: &[&str], pending: &[&str], passed: &[&str]) {
        let names = |names: &[&str]| names.iter().map(|name| name.to_string()).collect();
        let mut blockers = self.0.lock().unwrap();
        blockers.head_sha = head_sha.into();
        blockers.failed_required_checks = names(failed);
        blockers.pending_required_checks = names(pending);
        blockers.passed_required_checks = names(passed);
    }
}

/// A required-checks case: its name, the head the checks describe, the
/// failed, pending and passed checks, and what the refusal must say.
type ChecksCase<'a> = (
    &'a str,
    &'a str,
    &'a [&'a str],
    &'a [&'a str],
    &'a [&'a str],
    &'a str,
);

/// A provider case: its name and how it rewrites the PR's answer.
type AnswerCase<'a> = (&'a str, &'a dyn Fn(&mut Value));

/// Install a forge whose required checks all passed on the recorded head.
pub(super) fn green_forge(f: &mut Fixture) -> ChecksForge {
    let forge = ChecksForge(Arc::new(Mutex::new(PullRequestBlockers {
        pull: ForgePullRequest {
            number: 17,
            head_branch: f.branch.clone(),
            body: format!("Delivers {}.", f.task),
            author: "orbit-bot".into(),
        },
        head_sha: f.head.clone(),
        conflicting: false,
        failed_required_checks: Vec::new(),
        pending_required_checks: Vec::new(),
        passed_required_checks: vec!["Check".into(), "Test".into()],
        unreported_required_checks: Vec::new(),
        requires_status_checks: Some(true),
    })));
    f.runtime = f
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(forge.clone()));
    forge
}

/// Rewrite the provider's answer for PR 17.
fn edit_answer(f: &Fixture, edit: impl FnOnce(&mut Value)) -> Value {
    let original: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    let mut answer = original.clone();
    edit(&mut answer);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    original
}

/// Record `origin` as where the stopped run executed. Ordinary upserts
/// preserve the trusted origin, so hostile persisted evidence is written
/// directly in this isolated fixture.
fn set_origin(f: &Fixture, origin: &orbit_types::task::ExecutionLocation) {
    f.runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                    [
                        serde_json::to_string(origin).unwrap(),
                        f.source.run_id.clone(),
                    ],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
}

/// The incident: the escalated task's PR went green at its recorded head,
/// requeue pointed at source repair and source repair pointed back. Requeue
/// still carries a green candidate without granting source-repair authority.
fn an_escalated_publication_with_green_checks_is_requeued_onto_its_pull_request() {
    let (mut f, _, _) = escalated_completion_fixture();
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Blocked
    );
    let repair = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap();
    assert_eq!(repair["binding"]["head_sha"], f.head);
    assert!(repair["policy_digest"].is_string());
    green_forge(&mut f);
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog", "{answer}");
    assert_eq!(answer["source_run_id"], f.source.run_id);
    assert_eq!(answer["resumes"]["source"], "pipeline");
    assert_eq!(answer["resumes"]["pull_request"], "17");
    assert_eq!(answer["resumes"]["branch"], f.branch);
    assert_eq!(answer["resumes"]["head_sha"], f.head);
    let history = f.runtime.get_task_history(&f.task).unwrap();
    let preserved = history
        .iter()
        .filter(|entry| entry.event == "published_owner_requeued")
        .collect::<Vec<_>>();
    assert_eq!(preserved.len(), 1, "one preservation record");
    assert_eq!(
        preserved[0].by, "system",
        "written by Orbit, not the operator"
    );
    assert_next_run_resumes_the_published_candidate(&f);
}

/// A successful required check cannot make an unreadable neighbouring check
/// optional. Exercise the real forge reader as well as the operator requeue.
fn escalated_publication_requeue_refuses_unreadable_required_flags() {
    let (mut f, checkout, _) = escalated_completion_fixture();
    let rollup = f._root.path().join("rollup.json");
    std::fs::write(
        f._root.path().join("bin/gh"),
        format!(
            "#!/bin/sh\ncase \"$1 $2 $3\" in\n'pr view 17') cat '{}' ;;\n'api graphql '*) cat '{}' ;;\n'api user '*) echo orbit-bot ;;\n'api repos/'*) echo '[]' ;;\n*) exit 99 ;;\nesac\n",
            f.answer.display(),
            rollup.display()
        ),
    )
    .unwrap();
    f.runtime = f
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(GhTaskPrForge));
    let answer = json!({"data":{"repository":{"pullRequest":{
        "number":17, "state":"OPEN", "headRefName":f.branch, "headRefOid":f.head,
        "baseRefName":"main", "baseRef":{"branchProtectionRule":null},
        "author":{"login":"orbit-bot"}, "body":format!("Delivers {}.", f.task),
        "mergeable":"MERGEABLE", "commits":{"nodes":[{"commit":{
            "oid":f.head, "statusCheckRollup":{"contexts":{
                "pageInfo":{"hasNextPage":false}, "nodes":[
                    {"__typename":"CheckRun", "name":"Check", "status":"COMPLETED",
                        "conclusion":"SUCCESS", "isRequired":true},
                    {"__typename":"CheckRun", "name":"Test", "status":"COMPLETED",
                        "conclusion":"FAILURE"}
                ]
            }}
        }}]}
    }}}});
    let node =
        "/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup/contexts/nodes/1";
    let refs = git(&f.repo, &["ls-remote", "origin"]);
    for status in ["COMPLETED", "IN_PROGRESS"] {
        for required in [None, Some(Value::Null), Some(json!("true"))] {
            let case = format!("{status}, isRequired={required:?}");
            let mut malformed = answer.clone();
            let check = malformed.pointer_mut(node).unwrap();
            check["status"] = json!(status);
            if let Some(required) = required {
                check["isRequired"] = required;
            }
            std::fs::write(&rollup, malformed.to_string()).unwrap();
            assert!(
                GhTaskPrForge.pull_request_blockers(&f.repo, 17).is_err(),
                "{case}: unreadable rollup was classified as readable"
            );
            let error = refused(&f, false, &case);
            assert!(
                error.contains("required checks unreadable"),
                "{case}: {error}"
            );
            assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), f.head, "{case}");
            assert_eq!(git(&f.repo, &["ls-remote", "origin"]), refs, "{case}");
        }
    }
    // A boolean false proves the failed neighbour optional; boolean true
    // with a successful conclusion proves both required checks green.
    for required in [false, true] {
        let mut valid = answer.clone();
        let check = valid.pointer_mut(node).unwrap();
        check["isRequired"] = json!(required);
        if required {
            check["conclusion"] = json!("SUCCESS");
        }
        std::fs::write(&rollup, valid.to_string()).unwrap();
        let blockers = GhTaskPrForge
            .pull_request_blockers(&f.repo, 17)
            .unwrap()
            .unwrap();
        assert!(blockers.failed_required_checks.is_empty());
        assert!(blockers.pending_required_checks.is_empty());
        assert_eq!(
            blockers.passed_required_checks,
            if required {
                vec!["Check", "Test"]
            } else {
                vec!["Check"]
            }
        );
    }
    let resumed = requeue(&f, false).unwrap();
    assert_eq!(resumed["status"], "backlog", "{resumed}");
    assert_eq!(resumed["resumes"]["head_sha"], f.head, "{resumed}");
}

/// Each neighbouring state is refused with nothing written, and none of the
/// refusals sends the operator to source repair. Once the publication is
/// restored the same requeue succeeds, so each refusal was its case's.
fn escalated_publication_requeue_keeps_its_neighbouring_refusals() {
    let (mut f, _, _) = escalated_completion_fixture();
    let forge = green_forge(&mut f);
    let head = f.head.clone();
    let base = f.base.clone();
    let checks: [ChecksCase; 5] = [
        ("failed checks", &head, &["Check"], &[], &["Test"], "failed"),
        (
            "pending checks",
            &head,
            &[],
            &["Test"],
            &["Check"],
            "not finished",
        ),
        ("missing checks", &head, &[], &[], &[], "no required check"),
        (
            "stale checks",
            &base,
            &[],
            &[],
            &["Check", "Test"],
            "describe head",
        ),
        (
            "stale failure",
            &base,
            &["Check"],
            &[],
            &[],
            "describe head",
        ),
    ];
    for (case, at, failed, pending, passed, reason) in checks {
        forge.set(at, failed, pending, passed);
        let error = refused(&f, false, case);
        assert!(error.contains(reason), "{case}: {error}");
        assert!(!error.contains("source_repair"), "{case}: {error}");
        assert!(error.contains("--discard-candidate"), "{case}: {error}");
    }
    forge.set(&head, &[], &[], &["Check", "Test"]);
    let provider: [AnswerCase; 3] = [
        ("head moved", &|answer| answer["headRefOid"] = json!(base)),
        ("closed", &|answer| answer["state"] = json!("CLOSED")),
        ("merged", &|answer| {
            answer["state"] = json!("MERGED");
            answer["mergedAt"] = json!("2026-10-24T18:20:00Z");
            answer["mergeCommit"] = json!({"oid": base});
        }),
    ];
    for (case, edit) in provider {
        let original = edit_answer(&f, edit);
        let error = refused(&f, false, case);
        assert!(error.contains("closed/merged"), "{case}: {error}");
        assert!(error.contains("settle-merged"), "{case}: {error}");
        assert!(!error.contains("source_repair"), "{case}: {error}");
        std::fs::write(&f.answer, original.to_string()).unwrap();
    }
    let local = f
        .runtime
        .show_job_run_observed(&f.source.run_id)
        .unwrap()
        .executed_on
        .unwrap();
    let mut foreign = local.clone();
    foreign.machine_id = "hm_other".into();
    set_origin(&f, &foreign);
    let error = refused(&f, false, "foreign owner");
    assert!(error.contains("owner-local"), "foreign owner: {error}");
    set_origin(&f, &local);
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Blocked
    );
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["resumes"]["source"], "pipeline", "{answer}");
}

/// A specification edited since the run published is refused with the
/// discard exit, and so is every terminal task.
fn escalated_publication_requeue_refuses_a_changed_specification_and_terminal_tasks() {
    let (mut f, _, _) = escalated_completion_fixture();
    green_forge(&mut f);
    f.runtime
        .update_task_as_human(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                description: Some("A different mandate than the one published".into()),
                ..Default::default()
            },
            "human".into(),
        )
        .unwrap();
    let error = refused(&f, false, "changed specification");
    assert!(error.contains("specification changed"), "{error}");
    assert!(error.contains("--discard-candidate"), "{error}");
    // Fixtures are built one at a time; a second built while this one is
    // alive does not finish setting up.
    drop(f);

    for terminal in [TaskStatus::Done, TaskStatus::Rejected, TaskStatus::Archived] {
        let (mut f, _, _) = escalated_completion_fixture();
        green_forge(&mut f);
        f.runtime
            .apply_task_automation_update(
                &f.task,
                TaskAutomationUpdate {
                    status: Some(terminal),
                    ..Default::default()
                },
            )
            .unwrap();
        let error = refused(&f, false, &terminal.to_string());
        assert!(
            error.contains("only a blocked task can be requeued"),
            "{terminal}: {error}"
        );
    }
}

/// The incident's resume: the kept worktree was gone, yet the resume moved
/// the task `blocked -> review` and then failed. It is now refused before
/// anything is written, and the requeue still resumes the published branch.
fn a_resume_whose_worktree_is_gone_is_refused_before_the_task_moves() {
    let (mut f, checkout, _) = escalated_completion_fixture();
    git(
        &f.repo,
        &["worktree", "remove", "--force", checkout.to_str().unwrap()],
    );
    let task = serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap();
    let history = serde_json::to_value(f.runtime.get_task_history(&f.task).unwrap()).unwrap();
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", json!({"id": f.source.run_id}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("no longer exists"), "{error}");
    assert!(error.contains("orbit task requeue"), "{error}");
    assert_eq!(
        serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap(),
        task,
        "the task did not move"
    );
    assert_eq!(
        serde_json::to_value(f.runtime.get_task_history(&f.task).unwrap()).unwrap(),
        history,
        "nothing was recorded"
    );
    green_forge(&mut f);
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog", "{answer}");
    assert_eq!(answer["resumes"]["head_sha"], f.head);
}

fn record_pr_auto_fix_attempt(f: &Fixture) {
    use orbit_types::task::TaskHistoryEntry;
    use orbit_types::workflow::{
        PR_AUTO_FIX_ADMITTED_EVENT, PR_AUTO_FIX_BUDGET, PrAutoFixAttempt, PrAutoFixBlocker,
    };

    let note = serde_json::to_string(&PrAutoFixAttempt {
        pr_number: "17".into(),
        head_sha: f.head.clone(),
        blocker: PrAutoFixBlocker::FailedChecks,
        attempt: 1,
        budget: PR_AUTO_FIX_BUDGET,
        run_id: f.source.run_id.clone(),
        crew: "repair".into(),
        crew_source: "default".into(),
    })
    .unwrap();
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: PR_AUTO_FIX_ADMITTED_EVENT.into(),
                    note: Some(note),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
}

/// A cancelled run that pushed and opened its PR has no failure-handoff
/// receipt; requeuing without discarding preserves the PR and repair budget,
/// while a subsequent specification edit refuses without saying checks can pass
/// and updates `blocked_next_step` to name the discard command.
fn cancelled_publication_with_green_checks_is_requeued_preserving_repair_budget() {
    let (mut f, _, _) = escalated_completion_fixture();
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.failure_activity_checkpoint = None;
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.source.state = JobRunState::Cancelled;
    published_owner::save_run(&f, &f.source);
    record_pr_auto_fix_attempt(&f);
    green_forge(&mut f);

    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog", "{answer}");
    assert_eq!(answer["resumes"]["source"], "pipeline", "{answer}");
    assert_eq!(answer["resumes"]["pull_request"], "17", "{answer}");
    let history = f.runtime.get_task_history(&f.task).unwrap();
    assert_eq!(
        orbit_types::workflow::pr_auto_fix_attempts(&history, "17").len(),
        1,
        "candidate requeue must not reset PR repair attempts"
    );
    assert_next_run_resumes_the_published_candidate(&f);
    drop(f);

    let (mut f, _, _) = escalated_completion_fixture();
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.failure_activity_checkpoint = None;
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.source.state = JobRunState::Cancelled;
    published_owner::save_run(&f, &f.source);
    green_forge(&mut f);
    f.runtime
        .update_task_as_human(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                description: Some("A new specification after cancellation".into()),
                ..Default::default()
            },
            "human".into(),
        )
        .unwrap();
    let error = refused(&f, false, "cancelled with changed specification");
    assert!(error.contains("specification changed"), "{error}");
    assert!(error.contains("--discard-candidate"), "{error}");
    assert!(
        !error.contains("requeue again when they pass"),
        "spec change cannot pass by waiting for checks: {error}"
    );
    let task = f.runtime.get_task(&f.task).unwrap();
    let history = f.runtime.get_task_history(&f.task).unwrap();
    let next = f
        .runtime
        .blocked_next_step(&task, &history)
        .expect("blocked next step");
    assert_eq!(
        next.requeue_command,
        format!(
            "orbit task update {} --status backlog --discard-candidate",
            f.task
        )
    );
}

/// While a run is still live, `blocked_next_step` never points at a failing
/// plain backlog move and instead names `orbit run cancel`.
fn live_run_refusal_and_next_step_name_cancel() {
    let (mut f, _, _) = escalated_completion_fixture();
    f.source.state = JobRunState::Running;
    f.source.finished_at = None;
    published_owner::save_run(&f, &f.source);

    let error = refused(&f, false, "live run");
    assert!(!error.is_empty());
    let task = f.runtime.get_task(&f.task).unwrap();
    let history = f.runtime.get_task_history(&f.task).unwrap();
    let next = f
        .runtime
        .blocked_next_step(&task, &history)
        .expect("blocked next step");
    assert_eq!(next.resume_command, None);
    assert_eq!(
        next.requeue_command,
        format!("orbit run cancel {} --confirm", f.source.run_id)
    );
}

/// A run that succeeded into `review` with an open PR can be returned to backlog
/// or requeued without discarding, keeps its PR repair attempts, and any
/// refusal on `review` names `orbit task update ... --discard-candidate` without
/// naming `orbit task requeue`.
fn succeeded_review_publication_is_requeued_preserving_repair_budget() {
    let (mut f, _, _) = escalated_completion_fixture();
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.failure_activity_checkpoint = None;
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.source.state = JobRunState::Success;
    published_owner::save_run(&f, &f.source);
    record_pr_auto_fix_attempt(&f);
    let forge = green_forge(&mut f);

    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();

    forge.set(&f.head, &["Check"], &[], &["Test"]);
    let review_err = super::handoff_requeue::plain_backlog(&f, false)
        .unwrap_err()
        .to_string();
    assert!(
        review_err.contains(&format!(
            "orbit task update {} --status backlog --discard-candidate",
            f.task
        )),
        "{review_err}"
    );
    assert!(
        !review_err.contains("orbit task requeue"),
        "review refusal must not suggest blocked-only requeue: {review_err}"
    );

    forge.set(&f.head, &[], &[], &["Check", "Test"]);
    super::handoff_requeue::plain_backlog(&f, false).unwrap();
    let history = f.runtime.get_task_history(&f.task).unwrap();
    assert_eq!(
        orbit_types::workflow::pr_auto_fix_attempts(&history, "17").len(),
        1,
        "review backlog return must preserve PR repair attempts"
    );
    assert_next_run_resumes_the_published_candidate(&f);
    drop(f);

    let (mut f, _, _) = escalated_completion_fixture();
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.failure_activity_checkpoint = None;
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.source.state = JobRunState::Success;
    published_owner::save_run(&f, &f.source);
    green_forge(&mut f);
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog", "{answer}");
    assert_eq!(answer["resumes"]["source"], "pipeline", "{answer}");
}

/// A stopped successful owner delivery in `review` has no failure-handoff
/// receipt, so only its persisted history shows a specification change. A
/// backlog move that carries the change in the same public update must be
/// refused, with neither the move nor the edit applied and no preservation
/// record written, while an unchanged specification still resumes the PR and
/// an explicit discard still allows a fresh implementation.
fn combined_specification_edit_and_backlog_move_refuses_a_no_receipt_publication() {
    let succeeded_review = || {
        let (mut f, _, _) = escalated_completion_fixture();
        let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
        state.failure_activity_checkpoint = None;
        f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
        f.source.state = JobRunState::Success;
        published_owner::save_run(&f, &f.source);
        green_forge(&mut f);
        f.runtime
            .apply_task_automation_update(
                &f.task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Review),
                    ..Default::default()
                },
            )
            .unwrap();
        f
    };
    let preserved = |f: &Fixture| {
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == "published_owner_requeued")
            .count()
    };

    for edit in [
        json!({"description": "A different specification"}),
        json!({"acceptance_criteria": ["A different criterion"]}),
    ] {
        let f = succeeded_review();
        let before = serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap();
        let history = serde_json::to_value(f.runtime.get_task_history(&f.task).unwrap()).unwrap();
        let mut input = json!({
            "id": f.task, "workspace": f.repo, "status": "backlog", "model": "claude"
        });
        input
            .as_object_mut()
            .unwrap()
            .extend(edit.as_object().unwrap().clone());
        let error = f
            .runtime
            .run_tool("orbit.task.update", input)
            .expect_err("combined edit and backlog move was accepted")
            .to_string();
        assert!(error.contains("specification changed"), "{edit}: {error}");
        assert!(error.contains("--discard-candidate"), "{edit}: {error}");
        assert_eq!(
            serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap(),
            before,
            "{edit}: the refused update changed the task"
        );
        assert_eq!(
            serde_json::to_value(f.runtime.get_task_history(&f.task).unwrap()).unwrap(),
            history,
            "{edit}: the refused update wrote history"
        );
        assert_eq!(preserved(&f), 0, "{edit}: preservation record written");
        assert_eq!(
            f.runtime.get_task(&f.task).unwrap().status,
            TaskStatus::Review
        );

        // An explicit discard still starts fresh work with the new text.
        f.runtime
            .update_task_as_human(
                &f.task,
                orbit_core::application::task::TaskUpdateParams {
                    status: Some(TaskStatus::Backlog),
                    discard_candidate: true,
                    description: Some("A different specification".into()),
                    ..Default::default()
                },
                "human".into(),
            )
            .unwrap();
        assert_eq!(
            f.runtime.get_task(&f.task).unwrap().status,
            TaskStatus::Backlog
        );
        assert_eq!(preserved(&f), 0, "{edit}: a discard preserves nothing");
    }

    // An unchanged specification still resumes the green publication, also
    // when the update restates the stored text.
    let f = succeeded_review();
    let stored = f.runtime.get_task(&f.task).unwrap();
    f.runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": f.task, "workspace": f.repo, "status": "backlog", "model": "claude",
                "description": stored.description,
                "acceptance_criteria": stored.acceptance_criteria,
            }),
        )
        .unwrap();
    assert_eq!(preserved(&f), 1);
    assert_next_run_resumes_the_published_candidate(&f);
}

/// Repositories with neither classic required checks nor required ruleset
/// checks distinguish themselves from repositories that require checks missing
/// from the head's rollup.
fn repository_with_no_required_checks_allows_requeue_and_distinguishes_absent_checks() {
    let (mut f, _, _) = escalated_completion_fixture();
    let graphql = f._root.path().join("graphql.json");
    let rules = f._root.path().join("rules.json");
    std::fs::write(
        f._root.path().join("bin/gh"),
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n'pr view') cat '{}' ;;\n'api graphql') cat '{}' ;;\n'api repos/'*) cat '{}' ;;\n'api user') echo orbit-bot ;;\n*) exit 99 ;;\nesac\n",
            f.answer.display(),
            graphql.display(),
            rules.display(),
        ),
    )
    .unwrap();
    f.runtime = f
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(GhTaskPrForge));

    let rollup_graphql = |rule: Value, nodes: Value| {
        json!({"data":{"repository":{"pullRequest":{
            "number":17, "state":"OPEN", "headRefName":f.branch, "headRefOid":f.head,
            "baseRefName":"main", "isDraft":false,
            "baseRef":{"branchProtectionRule":rule},
            "author":{"login":"orbit-bot"}, "body":format!("Delivers {}.", f.task),
            "mergeable":"MERGEABLE", "commits":{"nodes":[{"commit":{
                "oid":f.head, "statusCheckRollup":{"contexts":{
                    "pageInfo":{"hasNextPage":false}, "nodes":nodes
                }}
            }}]}
        }}}})
    };

    let base_graphql = |rule: Value| rollup_graphql(rule, json!([]));

    // Classic required check missing from rollup -> refused.
    std::fs::write(
        &graphql,
        base_graphql(json!({"requiredStatusChecks":[{"context":"CI"}]})).to_string(),
    )
    .unwrap();
    std::fs::write(&rules, "[]").unwrap();
    assert_eq!(
        GhTaskPrForge
            .pull_request_blockers(&f.repo, 17)
            .unwrap()
            .unwrap()
            .requires_status_checks,
        Some(true)
    );
    let classic_err = refused(&f, false, "classic branch protection");
    assert!(classic_err.contains("no required check"), "{classic_err}");

    // Ruleset required check missing from rollup -> refused.
    std::fs::write(&graphql, base_graphql(Value::Null).to_string()).unwrap();
    std::fs::write(
        &rules,
        json!([{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"Rule CI"}]}}]).to_string(),
    )
    .unwrap();
    assert_eq!(
        GhTaskPrForge
            .pull_request_blockers(&f.repo, 17)
            .unwrap()
            .unwrap()
            .requires_status_checks,
        Some(true)
    );
    let ruleset_err = refused(&f, false, "ruleset required checks");
    assert!(ruleset_err.contains("no required check"), "{ruleset_err}");

    // Neither classic branch protection nor rulesets require checks -> allowed.
    std::fs::write(&rules, "[]").unwrap();
    let blockers = GhTaskPrForge
        .pull_request_blockers(&f.repo, 17)
        .unwrap()
        .unwrap();
    assert_eq!(blockers.requires_status_checks, Some(false));
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog", "{answer}");
    assert_eq!(answer["resumes"]["pull_request"], "17", "{answer}");
}

/// A head that reports only some of the configured required checks is not
/// green: the one passing result must not stand for the missing one.
fn partially_reported_required_checks_refuse_requeue_until_every_check_passes() {
    let (mut f, _, _) = escalated_completion_fixture();
    let graphql = f._root.path().join("graphql.json");
    let rules = f._root.path().join("rules.json");
    std::fs::write(
        f._root.path().join("bin/gh"),
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n'pr view') cat '{}' ;;\n'api graphql') cat '{}' ;;\n'api repos/'*) cat '{}' ;;\n'api user') echo orbit-bot ;;\n*) exit 99 ;;\nesac\n",
            f.answer.display(),
            graphql.display(),
            rules.display(),
        ),
    )
    .unwrap();
    f.runtime = f
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(GhTaskPrForge));
    let node = |name: &str| {
        json!({"__typename":"CheckRun", "name":name, "status":"COMPLETED",
            "conclusion":"SUCCESS", "isRequired":true})
    };
    let graphql_with = |rule: Value, nodes: Vec<Value>| {
        json!({"data":{"repository":{"pullRequest":{
            "number":17, "state":"OPEN", "headRefName":f.branch, "headRefOid":f.head,
            "baseRefName":"main", "isDraft":false,
            "baseRef":{"branchProtectionRule":rule},
            "author":{"login":"orbit-bot"}, "body":format!("Delivers {}.", f.task),
            "mergeable":"MERGEABLE", "commits":{"nodes":[{"commit":{
                "oid":f.head, "statusCheckRollup":{"contexts":{
                    "pageInfo":{"hasNextPage":false}, "nodes":nodes
                }}
            }}]}
        }}}})
        .to_string()
    };

    // Classic protection requires Build and Test; only Build reported.
    let classic = json!({"requiredStatusChecks":[{"context":"Build"},{"context":"Test"}]});
    std::fs::write(&rules, "[]").unwrap();
    std::fs::write(&graphql, graphql_with(classic.clone(), vec![node("Build")])).unwrap();
    let blockers = GhTaskPrForge
        .pull_request_blockers(&f.repo, 17)
        .unwrap()
        .unwrap();
    assert_eq!(blockers.passed_required_checks, vec!["Build"]);
    assert_eq!(blockers.unreported_required_checks, vec!["Test"]);
    let error = refused(&f, false, "classic partial");
    assert!(error.contains("have not reported"), "{error}");
    assert!(error.contains("Test"), "{error}");
    assert!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .all(|entry| entry.event != "published_owner_requeued"),
        "a refused requeue preserved nothing"
    );

    // Both reported and passing -> eligible.
    std::fs::write(
        &graphql,
        graphql_with(classic, vec![node("Build"), node("Test")]),
    )
    .unwrap();
    let blockers = GhTaskPrForge
        .pull_request_blockers(&f.repo, 17)
        .unwrap()
        .unwrap();
    assert!(blockers.unreported_required_checks.is_empty());

    // A ruleset requiring Build and Test with only Build reported -> refused.
    std::fs::write(&graphql, graphql_with(Value::Null, vec![node("Build")])).unwrap();
    std::fs::write(
        &rules,
        json!([{"type":"required_status_checks","parameters":{"required_status_checks":[
            {"context":"Build"},{"context":"Test"}]}}])
        .to_string(),
    )
    .unwrap();
    let error = refused(&f, false, "ruleset partial");
    assert!(error.contains("have not reported"), "{error}");
    assert!(error.contains("Test"), "{error}");

    // Every ruleset check reported and passing -> eligible.
    std::fs::write(
        &graphql,
        graphql_with(Value::Null, vec![node("Build"), node("Test")]),
    )
    .unwrap();
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog", "{answer}");
}

/// Ruleset requirements layer on top of classic branch protection, so a
/// passing classic check cannot make a partially reported published head green.
fn layered_classic_and_ruleset_checks_refuse_requeue_until_every_check_passes() {
    let (mut f, _, _) = escalated_completion_fixture();
    let graphql = f._root.path().join("graphql.json");
    let rules = f._root.path().join("rules.json");
    std::fs::write(
        f._root.path().join("bin/gh"),
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n'pr view') cat '{}' ;;\n'api graphql') cat '{}' ;;\n'api repos/'*) cat '{}' ;;\n'api user') echo orbit-bot ;;\n*) exit 99 ;;\nesac\n",
            f.answer.display(),
            graphql.display(),
            rules.display(),
        ),
    )
    .unwrap();
    f.runtime = f
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(GhTaskPrForge));
    let node = |name: &str| {
        json!({"__typename":"CheckRun", "name":name, "status":"COMPLETED",
            "conclusion":"SUCCESS", "isRequired":true})
    };
    let graphql_with = |nodes: Vec<Value>| {
        json!({"data":{"repository":{"pullRequest":{
            "number":17, "state":"OPEN", "headRefName":f.branch, "headRefOid":f.head,
            "baseRefName":"main", "isDraft":false,
            "baseRef":{"branchProtectionRule":{"requiredStatusChecks":[{"context":"Build"}] }},
            "author":{"login":"orbit-bot"}, "body":format!("Delivers {}.", f.task),
            "mergeable":"MERGEABLE", "commits":{"nodes":[{"commit":{
                "oid":f.head, "statusCheckRollup":{"contexts":{
                    "pageInfo":{"hasNextPage":false}, "nodes":nodes
                }}
            }}]}
        }}}})
        .to_string()
    };
    // A failed ruleset read must not let the passing classic check stand
    // for the complete policy. The missing rules file makes the gh stub fail.
    std::fs::write(&graphql, graphql_with(vec![node("Build")])).unwrap();
    let blockers = GhTaskPrForge
        .pull_request_blockers(&f.repo, 17)
        .unwrap()
        .unwrap();
    assert_eq!(blockers.passed_required_checks, vec!["Build"]);
    assert_eq!(blockers.requires_status_checks, None);
    let error = refused(&f, false, "unreadable ruleset");
    assert!(error.contains("policy is unreadable"), "{error}");

    std::fs::write(
        &rules,
        json!([{"type":"required_status_checks","parameters":{"required_status_checks":[
            {"context":"Test"}]}}])
        .to_string(),
    )
    .unwrap();

    // The recorded head reports only classic Build; the layered ruleset Test
    // remains unreported and blocks requeue without preserving the candidate.
    std::fs::write(&graphql, graphql_with(vec![node("Build")])).unwrap();
    let blockers = GhTaskPrForge
        .pull_request_blockers(&f.repo, 17)
        .unwrap()
        .unwrap();
    assert_eq!(blockers.passed_required_checks, vec!["Build"]);
    assert_eq!(blockers.unreported_required_checks, vec!["Test"]);
    let error = refused(&f, false, "layered partial");
    assert!(error.contains("Test"), "{error}");
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Blocked
    );
    assert!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .all(|entry| entry.event != "published_owner_requeued"),
        "a refused layered requeue preserves nothing"
    );

    // The same head is eligible once both policy sources' checks pass.
    std::fs::write(&graphql, graphql_with(vec![node("Build"), node("Test")])).unwrap();
    let blockers = GhTaskPrForge
        .pull_request_blockers(&f.repo, 17)
        .unwrap()
        .unwrap();
    assert!(blockers.unreported_required_checks.is_empty());
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog", "{answer}");
    assert_eq!(answer["resumes"]["pull_request"], "17", "{answer}");
    assert_eq!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == "published_owner_requeued")
            .count(),
        1,
        "the eligible requeue records candidate preservation"
    );
}

crate::scenario_table!(
    escalated_publication_scenarios,
    an_escalated_publication_with_green_checks_is_requeued_onto_its_pull_request,
    escalated_publication_requeue_refuses_unreadable_required_flags,
    escalated_publication_requeue_keeps_its_neighbouring_refusals,
    partially_reported_required_checks_refuse_requeue_until_every_check_passes,
    layered_classic_and_ruleset_checks_refuse_requeue_until_every_check_passes,
    escalated_publication_requeue_refuses_a_changed_specification_and_terminal_tasks,
    a_resume_whose_worktree_is_gone_is_refused_before_the_task_moves,
    cancelled_publication_with_green_checks_is_requeued_preserving_repair_budget,
    live_run_refusal_and_next_step_name_cancel,
    succeeded_review_publication_is_requeued_preserving_repair_budget,
    combined_specification_edit_and_backlog_move_refuses_a_no_receipt_publication,
    repository_with_no_required_checks_allows_requeue_and_distinguishes_absent_checks
);
