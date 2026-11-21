//! Regression for the owner sign-off failure handoff that required requeue,
//! a hand merge and a forced completion (the published handoff incident).

use super::decision_answer::raise_block_triage;
use super::failed_publication::{escalated_completion_fixture, escalated_handoff_fixture};
use super::*;
use orbit_core::application::task::DecisionListProfile;
use orbit_core::runtime::task_pr_forge::{
    ForgePullRequest, HandoffLandingPullRequest, LeasePullRequest, MergedPullRequest,
    PullRequestBlockers, PullRequestMergeOutcome, TaskPrForge,
};
use orbit_core::{ActorIdentity, OrbitError};
use orbit_types::task::{DecisionRequest, DecisionRequestState};
use std::path::Path;
use std::sync::{Arc, Mutex};

const MERGE: &str = "1234567890123456789012345678901234567890";

#[derive(Clone)]
struct Forge(Arc<Mutex<ForgeState>>);

struct ForgeState {
    pull: Option<HandoffLandingPullRequest>,
    merged: Option<MergedPullRequest>,
    merge_refusal: Option<String>,
    merges: usize,
}

impl Forge {
    fn for_fixture(f: &Fixture) -> Self {
        Self(Arc::new(Mutex::new(ForgeState {
            pull: Some(HandoffLandingPullRequest {
                blockers: PullRequestBlockers {
                    pull: ForgePullRequest {
                        number: 17,
                        head_branch: f.branch.clone(),
                        body: format!("Delivers {}.", f.task),
                        author: "orbit-bot".into(),
                    },
                    head_sha: f.head.clone(),
                    conflicting: false,
                    failed_required_checks: vec![],
                    pending_required_checks: vec![],
                    passed_required_checks: vec!["Check".into()],
                },
                base_branch: "main".into(),
                draft: false,
            }),
            merged: None,
            merge_refusal: None,
            merges: 0,
        })))
    }

    fn merge(&self) {
        let mut state = self.0.lock().unwrap();
        let pull = state.pull.take().unwrap();
        state.merged = Some(MergedPullRequest {
            pull: pull.blockers.pull,
            url: "https://github.com/owner/repository/pull/17".into(),
            base_branch: pull.base_branch,
            head_sha: pull.blockers.head_sha,
            merge_commit: MERGE.into(),
            merged_at: Utc::now(),
        });
        state.merges += 1;
    }
}

impl TaskPrForge for Forge {
    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .pull
            .iter()
            .map(|p| p.blockers.pull.clone())
            .collect())
    }
    fn handoff_landing_pull_request(
        &self,
        _: &Path,
        _: u64,
    ) -> Result<Option<HandoffLandingPullRequest>, OrbitError> {
        Ok(self.0.lock().unwrap().pull.clone())
    }
    fn merged_pull_requests(&self, _: &Path) -> Result<Vec<MergedPullRequest>, OrbitError> {
        Ok(self.0.lock().unwrap().merged.iter().cloned().collect())
    }
    fn lease_pull_request(
        &self,
        _: &Path,
        _: &str,
    ) -> Result<Option<LeasePullRequest>, OrbitError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .pull
            .as_ref()
            .map(|pull| LeasePullRequest {
                number: pull.blockers.pull.number,
                base_branch: pull.base_branch.clone(),
                merged: false,
            }))
    }
    fn pull_request_blockers(
        &self,
        _: &Path,
        _: u64,
    ) -> Result<Option<PullRequestBlockers>, OrbitError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .pull
            .as_ref()
            .map(|pull| pull.blockers.clone()))
    }
    fn merge_pull_request(
        &self,
        _: &Path,
        number: u64,
        head: &str,
        _: &orbit_types::workflow::MergeMessage,
    ) -> Result<PullRequestMergeOutcome, OrbitError> {
        {
            let state = self.0.lock().unwrap();
            let pull = state.pull.as_ref().unwrap();
            assert_eq!(number, 17);
            assert_eq!(
                head, pull.blockers.head_sha,
                "merge uses the receipt's exact candidate lease"
            );
            if let Some(reason) = &state.merge_refusal {
                return Ok(PullRequestMergeOutcome::Refused {
                    reason: reason.clone(),
                });
            }
        }
        self.merge();
        Ok(PullRequestMergeOutcome::Merged {
            merge_commit: MERGE.into(),
        })
    }
    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        Ok("orbit-bot".into())
    }
    fn close_pull_request(&self, _: &Path, _: u64, _: &str) -> Result<(), OrbitError> {
        Ok(())
    }
}

fn land(f: &Fixture, request: &DecisionRequest) -> Result<DecisionRequest, OrbitError> {
    f.runtime.answer_decision_request_as_human(
        &request.id,
        "land",
        Some("Owner accepts the candidate".into()),
        "human:owner".into(),
        Some("cli".into()),
    )
}

fn owner_lands_green_handoff_and_refusals_change_nothing() {
    let (mut f, _) = escalated_handoff_fixture(Some(false), None);
    let config_path = f.repo.join(".orbit/config.toml");
    let config = std::fs::read_to_string(&config_path).unwrap();
    std::fs::write(&config_path, format!("{config}\n[[completion.hold]]\nname = \"owner sign-off\"\ntypes = [\"feature\", \"bug\", \"chore\", \"refactor\"]\n")).unwrap();
    f.runtime = orbit_core::OrbitRuntime::from_roots(
        &f._root.path().join("global"),
        &f.repo.join(".orbit"),
    )
    .unwrap()
    .with_automation_machine_identity(Some("hm_fixture".into()));
    let forge = Forge::for_fixture(&f);
    f.runtime = f
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(forge.clone()))
        .with_actor(ActorIdentity::human("human:owner"));
    let request = raise_block_triage(&f);
    assert_eq!(
        request.answers.len(),
        2,
        "legacy stored request has only its original options"
    );
    for profile in [DecisionListProfile::Tool, DecisionListProfile::Dashboard] {
        let listed = f
            .runtime
            .list_decision_requests_projected(
                Some("block_triage"),
                Some(&f.task),
                None,
                false,
                profile,
            )
            .unwrap();
        let rows = listed
            .get("requests")
            .or_else(|| listed.get("items"))
            .unwrap();
        assert!(
            rows[0]["answers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["id"] == "land"),
            "both operator surfaces receive the derived answer"
        );
    }
    let compact = f.runtime.open_decisions_json(&f.task).unwrap();
    let card = compact
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == request.id)
        .unwrap();
    assert!(
        card["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["id"] == "land"),
        "task detail receives the same land option as decision list"
    );
    let baseline = serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap();
    let history = f.runtime.get_task_history(&f.task).unwrap();
    let comments = f.runtime.get_task_comments(&f.task).unwrap();
    let green = forge.0.lock().unwrap().pull.clone();
    for condition in [
        "head",
        "failed",
        "pending",
        "closed",
        "draft",
        "conflict",
        "forge refusal",
        "target",
    ] {
        {
            let mut state = forge.0.lock().unwrap();
            state.pull = green.clone();
            state.merge_refusal = None;
            let pull = state.pull.as_mut().unwrap();
            match condition {
                "head" => pull.blockers.head_sha = "f".repeat(40),
                "failed" => pull.blockers.failed_required_checks.push("failed".into()),
                "pending" => pull.blockers.pending_required_checks.push("pending".into()),
                "closed" => state.pull = None,
                "draft" => pull.draft = true,
                "conflict" => pull.blockers.conflicting = true,
                "target" => pull.base_branch = "other".into(),
                _ => state.merge_refusal = Some(condition.into()),
            }
        }
        let error = land(&f, &request).unwrap_err().to_string();
        let expected = if condition == "target" {
            "target branch"
        } else {
            condition
        };
        assert!(error.contains(expected), "{condition}: {error}");
        assert_eq!(
            serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap(),
            baseline
        );
        assert_eq!(f.runtime.get_task_history(&f.task).unwrap(), history);
        assert_eq!(f.runtime.get_task_comments(&f.task).unwrap(), comments);
        assert_eq!(
            f.runtime
                .get_decision_request(&request.id)
                .unwrap()
                .unwrap()
                .state,
            DecisionRequestState::Open
        );
        assert_eq!(forge.0.lock().unwrap().merges, 0);
    }
    forge.0.lock().unwrap().pull = green;
    forge.0.lock().unwrap().merge_refusal = None;
    let agent = f.runtime.clone().with_actor(ActorIdentity::agent("codex"));
    let error = agent
        .answer_decision_request_as_human(&request.id, "land", None, "human:spoofed".into(), None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("only the operator"), "{error}");
    for actor in ["system", "precedent", "codex"] {
        assert!(
            f.runtime
                .answer_decision_request_as_human(&request.id, "land", None, actor.into(), None)
                .is_err(),
            "automation cannot choose land: {actor}"
        );
    }
    let answered = land(&f, &request).unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    assert_eq!(answered.resolution.unwrap().answered_by, "human:owner");
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Done
    );
    let history = f.runtime.get_task_history(&f.task).unwrap();
    assert!(
        history
            .iter()
            .any(|e| e.event == "decision_answered" && e.by == "human:owner")
    );
    assert!(history.iter().any(|e| {
        e.to_status == Some(TaskStatus::Done)
            && e.note
                .as_ref()
                .is_some_and(|note| note.contains(MERGE) && note.contains("#17"))
    }));
    assert!(
        f.runtime
            .get_task_comments(&f.task)
            .unwrap()
            .iter()
            .any(|c| c.by == "human:owner" && c.message.contains("Owner accepts the candidate"))
    );
    assert!(history.iter().any(|e| matches!(&e.decision,
        Some(orbit_types::task::TaskDecision::CompletionHeld(hold)) if hold.rule == "owner sign-off")),
        "human approval records and satisfies the completion hold");
    assert_eq!(forge.0.lock().unwrap().merges, 1);
}

fn requeued_failure_handoff_settles_after_external_merge() {
    let (mut f, _) = escalated_handoff_fixture(None, None);
    let forge = Forge::for_fixture(&f);
    f.runtime = f
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(forge.clone()))
        .with_actor(ActorIdentity::human("human:owner"));
    let request = raise_block_triage(&f);
    f.runtime
        .answer_decision_request_as_human(&request.id, "2", None, "human:owner".into(), None)
        .unwrap();
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Backlog
    );
    assert!(
        f.runtime.settle_externally_merged_task(&f.task).is_err(),
        "an unmerged PR never completes backlog work"
    );
    forge.merge();
    let settled = f.runtime.settle_externally_merged_tasks().unwrap();
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].previous_status, TaskStatus::Backlog);
    assert_eq!(settled[0].merge_commit, MERGE);
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Done
    );
    assert!(
        !f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .any(|e| e.event == "forced")
    );
}

fn required_review_hides_land_for_an_unreviewed_handoff() {
    for (reviewer, offered) in [(Some(true), false), (Some(false), true)] {
        let (mut f, _) = escalated_handoff_fixture(reviewer, None);
        f.runtime = f
            .runtime
            .clone()
            .with_actor(ActorIdentity::human("human:owner"));
        let request = raise_block_triage(&f);
        let current = f
            .runtime
            .get_decision_request(&request.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            current.answers.iter().any(|a| a.id == "land"),
            offered,
            "only the exact settled review candidate may land"
        );
        assert!(
            current.answers.iter().any(|a| a.id == "2"),
            "requeue remains available"
        );
    }
}

fn handoff_landing_reads_draft_and_checks_from_the_real_forge_adapter() {
    use orbit_core::runtime::task_pr_forge::GhTaskPrForge;
    let (f, _) = escalated_handoff_fixture(None, None);
    let response = f._root.path().join("landing-rollup.json");
    let mut document = json!({"data":{"repository":{"pullRequest":{
        "state":"OPEN", "isDraft":false, "baseRefName":"main", "mergeable":"MERGEABLE",
        "headRefName": f.branch, "headRefOid": f.head, "body":format!("Delivers {}.", f.task),
        "author":{"login":"orbit-bot"}, "commits":{"nodes":[{"commit":{
            "oid":f.head, "statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},
                "nodes":[{"__typename":"CheckRun", "name":"Check", "status":"COMPLETED",
                    "conclusion":"SUCCESS", "isRequired":true}]}}
        }}]}
    }}}});
    std::fs::write(
        f._root.path().join("bin/gh"),
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n'api graphql') cat '{}' ;;\n*) exit 99;;\nesac\n",
            response.display()
        ),
    )
    .unwrap();
    for draft in [false, true] {
        document["data"]["repository"]["pullRequest"]["isDraft"] = json!(draft);
        std::fs::write(&response, document.to_string()).unwrap();
        let read = GhTaskPrForge
            .handoff_landing_pull_request(&f.repo, 17)
            .unwrap()
            .unwrap();
        assert_eq!(read.draft, draft);
        assert_eq!(read.base_branch, "main");
        assert_eq!(read.blockers.head_sha, f.head);
        assert_eq!(read.blockers.passed_required_checks, vec!["Check"]);
    }
    document["data"]["repository"]["pullRequest"]["state"] = json!("CLOSED");
    std::fs::write(&response, document.to_string()).unwrap();
    assert!(
        GhTaskPrForge
            .handoff_landing_pull_request(&f.repo, 17)
            .unwrap()
            .is_none()
    );
}

fn periodic_settlement_recovers_a_stopped_completion_during_rest_rate_limiting() {
    let (f, _, _) = escalated_completion_fixture();
    let recent = f._root.path().join("recent-merges.json");
    std::fs::write(
        f._root.path().join("bin/gh"),
        format!(
            "#!/bin/sh\ncase \"$1 $2 $3 $4\" in\n\
             'pr list --state merged') cat '{}' ;;\n\
             'pr list --state open') printf '[]' ;;\n\
             'pr view 17 --json') cat '{}' ;;\n\
             'api user --jq .login') printf 'gh: API rate limit exceeded (HTTP 403)\\n' >&2; exit 1 ;;\n\
             *) exit 99 ;;\nesac\n",
            recent.display(),
            f.answer.display(),
        ),
    )
    .unwrap();
    let mut pr: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    pr["state"] = json!("CLOSED");
    pr["author"] = json!({"login":"orbit-bot"});
    pr["body"] = json!(format!("Delivers {}.", f.task));
    std::fs::write(&f.answer, pr.to_string()).unwrap();
    std::fs::write(&recent, "[]").unwrap();
    assert!(
        f.runtime
            .settle_externally_merged_tasks()
            .unwrap()
            .is_empty()
    );
    assert!(f.runtime.settle_externally_merged_task(&f.task).is_err());
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Blocked
    );

    pr["mergeCommit"] = json!({"oid": MERGE});
    let merged_at = "2026-11-20T02:41:33Z";
    pr["mergedAt"] = json!(merged_at);
    // Even with a merge SHA, a closed answer is not a merged one.
    std::fs::write(&recent, json!([&pr]).to_string()).unwrap();
    assert!(
        f.runtime
            .settle_externally_merged_tasks()
            .unwrap()
            .is_empty()
    );
    pr["state"] = json!("MERGED");
    for (field, wrong) in [
        ("baseRefName", json!("release")),
        ("headRefOid", json!(MERGE)),
        (
            "url",
            json!("https://github.com/elsewhere/repository/pull/17"),
        ),
        ("mergedAt", Value::Null),
    ] {
        let mut stale = pr.clone();
        stale[field] = wrong;
        std::fs::write(&recent, json!([stale]).to_string()).unwrap();
        assert!(
            f.runtime
                .settle_externally_merged_tasks()
                .unwrap()
                .is_empty(),
            "{field}"
        );
        assert_eq!(
            f.runtime.get_task(&f.task).unwrap().status,
            TaskStatus::Blocked
        );
    }
    std::fs::write(&recent, json!([pr]).to_string()).unwrap();
    let settled = f.runtime.settle_externally_merged_tasks().unwrap();
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0].task_id, f.task);
    assert_eq!(settled[0].pr_number, 17);
    assert_eq!(settled[0].merge_commit, MERGE);
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Done
    );
    let history = f.runtime.get_task_history(&f.task).unwrap();
    let entry = history
        .iter()
        .find(|entry| entry.event == "external_merge_settled")
        .expect("settlement in task history");
    assert_eq!(entry.from_status, Some(TaskStatus::Blocked));
    assert_eq!(entry.to_status, Some(TaskStatus::Done));
    let note = entry.note.as_deref().unwrap();
    for detail in ["#17", MERGE, merged_at] {
        assert!(note.contains(detail), "{note}");
    }
}

fn the_owner_sweep_merges_a_stopped_publication_only_while_the_pull_request_is_unchanged() {
    // ORB-25994: a stopped native run published head A. A pull request that
    // moved to B must not be merged: settlement would reject B afterwards.
    let (mut f, _, _) = escalated_completion_fixture();
    f.source.state = JobRunState::Success;
    super::published_owner::save_run(&f, &f.source);
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();
    let forge = Forge::for_fixture(&f);
    f.runtime = f
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(forge.clone()));
    let moved = "b".repeat(40);
    forge
        .0
        .lock()
        .unwrap()
        .pull
        .as_mut()
        .unwrap()
        .blockers
        .head_sha = moved.clone();
    for _ in 0..2 {
        assert!(
            f.runtime
                .land_completion_authorized_tasks()
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        forge.0.lock().unwrap().merges,
        0,
        "the moved head is not merged"
    );
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Review
    );
    let recorded = f
        .runtime
        .get_task_history(&f.task)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == "landing_follow_up")
        .map(|entry| entry.note.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(recorded.len(), 1, "recorded once: {recorded:?}");
    assert!(
        recorded[0].contains("publication_identity_changed") && recorded[0].contains(&moved),
        "{}",
        recorded[0]
    );

    forge
        .0
        .lock()
        .unwrap()
        .pull
        .as_mut()
        .unwrap()
        .blockers
        .head_sha = f.head.clone();
    let landed = f.runtime.land_completion_authorized_tasks().unwrap();
    assert_eq!(landed.len(), 1, "{landed:?}");
    assert_eq!(forge.0.lock().unwrap().merges, 1);
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Done
    );
}

crate::scenario_table!(
    handoff_landing_regressions,
    owner_lands_green_handoff_and_refusals_change_nothing,
    requeued_failure_handoff_settles_after_external_merge,
    required_review_hides_land_for_an_unreviewed_handoff,
    handoff_landing_reads_draft_and_checks_from_the_real_forge_adapter,
    periodic_settlement_recovers_a_stopped_completion_during_rest_rate_limiting,
    the_owner_sweep_merges_a_stopped_publication_only_while_the_pull_request_is_unchanged,
);
