//! Regression for the owner sign-off failure handoff that required requeue,
//! a hand merge and a forced completion (the published handoff incident).

use super::decision_answer::raise_block_triage;
use super::failed_publication::escalated_handoff_fixture;
use super::*;
use orbit_core::application::task::DecisionListProfile;
use orbit_core::runtime::task_pr_forge::{
    ForgePullRequest, HandoffLandingPullRequest, MergedPullRequest, PullRequestBlockers,
    PullRequestMergeOutcome, TaskPrForge,
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
            base_branch: pull.base_branch,
            head_sha: pull.blockers.head_sha,
            merge_commit: MERGE.into(),
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
    fn merge_pull_request(
        &self,
        _: &Path,
        number: u64,
        head: &str,
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

crate::scenario_table!(
    handoff_landing_regressions,
    owner_lands_green_handoff_and_refusals_change_nothing,
    requeued_failure_handoff_settles_after_external_merge,
    required_review_hides_land_for_an_unreviewed_handoff,
    handoff_landing_reads_draft_and_checks_from_the_real_forge_adapter,
);
