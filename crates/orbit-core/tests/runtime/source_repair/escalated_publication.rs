//! A run that published its own pull request, failed completion and was
//! escalated leaves its task blocked with that PR open and unmerged. Once
//! every required check has passed on the recorded head, an operator requeue
//! resumes the PR and the next run adopts it without re-implementing. Source
//! repair names that route instead of pointing back at requeue. A moved head,
//! unfinished, failed, missing or stale checks, a closed or merged PR, a
//! foreign owner, a changed specification and a terminal task stay refused
//! with nothing written. A plain resume whose worktree is gone is refused
//! before the task moves.
use super::failed_publication::escalated_completion_fixture;
use super::handoff_requeue::{assert_next_run_resumes_the_published_candidate, refused, requeue};
use super::*;
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, PullRequestBlockers, TaskPrForge};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// A forge reporting the fixture's PR 17 with the required checks a case
/// sets.
#[derive(Clone)]
struct ChecksForge(Arc<Mutex<PullRequestBlockers>>);

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
    fn set(&self, head_sha: &str, failed: &[&str], pending: &[&str], passed: &[&str]) {
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
fn green_forge(f: &mut Fixture) -> ChecksForge {
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
/// requeue pointed at source repair and source repair pointed back. Source
/// repair now names requeue, and the requeue resumes the kept PR.
fn an_escalated_publication_with_green_checks_is_requeued_onto_its_pull_request() {
    let (mut f, _, _) = escalated_completion_fixture();
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Blocked
    );
    let repair = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap_err()
        .to_string();
    assert!(
        repair.contains("source repair refused") && repair.contains("orbit task requeue"),
        "source repair names the route that applies: {repair}"
    );
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

crate::scenario_table!(
    escalated_publication_scenarios,
    an_escalated_publication_with_green_checks_is_requeued_onto_its_pull_request,
    escalated_publication_requeue_keeps_its_neighbouring_refusals,
    escalated_publication_requeue_refuses_a_changed_specification_and_terminal_tasks,
    a_resume_whose_worktree_is_gone_is_refused_before_the_task_moves
);
