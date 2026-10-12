//! The owner's landing of a claimed candidate.
//!
//! A fast-forward has no provider identity, so the owner's delivery consumers
//! can attribute it only from a direct landing intent retained before the
//! branch moves. The landing names the accepted handoff and its task in that
//! intent; a candidate that cannot fast-forward lands nothing and records none
//! [ORB-13894]. PR reconciliation resolves external uncertainty before stopping
//! a mismatched delivery, so subsequent attempts see the settled state. A stop
//! on a conflicting or stale base is marked repairable [ORB-14261]. A hosted
//! check the review owed the owner gates the merge [ORB-15677].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_engine::{
    HandoffLandingContext, HandoffLandingStep, HandoffLandingUpdate, RuntimeHost,
    execute_deterministic_action, observe_candidate,
};
use orbit_types::workflow::automation::DirectLandingRequest;
use orbit_types::workflow::handoff::HandoffDelivery;
use serde_json::{Value, json};
use tempfile::tempdir;

const TASK_ID: &str = "T1";
const RUN_ID: &str = "jrun-landing-test";

struct LandingHost {
    repo: PathBuf,
    context: HandoffLandingContext,
    steps: Mutex<Vec<HandoffLandingStep>>,
    /// Each retained intent with the landing branch's tip when it was retained.
    intents: Mutex<Vec<(DirectLandingRequest, String)>>,
}

impl RuntimeHost for LandingHost {
    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo.to_string_lossy().into_owned())
    }

    fn handoff_landing_context(
        &self,
        _handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.clone())
    }

    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        self.steps.lock().unwrap().push(update.step.clone());
        Ok(())
    }

    fn record_direct_landing_intent(
        &self,
        request: &DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        let tip = git(&self.repo, &["rev-parse", "main"]);
        self.intents.lock().unwrap().push((request.clone(), tip));
        Ok(())
    }
}

/// An owner checkout on `main` with the claimed candidate on `orbit/T1`, and
/// the host the landing job hands the accepted handoff to.
fn owner_checkout(repo: &Path) -> LandingHost {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "base\n").unwrap();
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["checkout", "-q", "-b", "orbit/T1"]);
    std::fs::write(repo.join("work.txt"), "delivered\n").unwrap();
    git(repo, &["add", "work.txt"]);
    git(repo, &["commit", "-q", "-m", "candidate"]);
    git(repo, &["checkout", "-q", "main"]);
    let candidate = observe_candidate(
        repo,
        Some("orbit/T1"),
        "main",
        "main",
        HandoffDelivery::LocalCandidate,
        "workspace",
        "local",
    )
    .unwrap();
    LandingHost {
        repo: repo.to_path_buf(),
        context: HandoffLandingContext {
            handoff_id: "handoff-1".into(),
            task_id: TASK_ID.into(),
            claim_id: "claim-1".into(),
            candidate,
            unresolved_merge_intent: None,
            workspace_path: repo.to_path_buf(),
            owed_hosted_checks: Vec::new(),
        },
        steps: Mutex::default(),
        intents: Mutex::default(),
    }
}

fn land(host: &impl RuntimeHost) -> Result<Value, OrbitError> {
    land_with(host, json!({}))
}

/// [`land`] with `extra` merged into the step input.
fn land_with(host: &impl RuntimeHost, extra: Value) -> Result<Value, OrbitError> {
    let mut input = json!({"handoff_id": "handoff-1", "task_id": TASK_ID, "run_id": RUN_ID});
    if let (Some(input), Value::Object(extra)) = (input.as_object_mut(), extra) {
        input.extend(extra);
    }
    execute_deterministic_action(
        host,
        "handoff_land",
        &json!({}),
        &input,
        false,
        &HashMap::new(),
        None,
    )
}

#[test]
fn a_local_candidate_landing_names_its_task_before_the_branch_moves() {
    let sandbox = tempdir().unwrap();
    let host = owner_checkout(sandbox.path());
    let base = git(&host.repo, &["rev-parse", "main"]);
    let candidate = host.context.candidate.candidate.commit.clone();

    land(&host).expect("the candidate fast-forwards");

    assert_eq!(git(&host.repo, &["rev-parse", "main"]), candidate);
    assert_eq!(
        *host.intents.lock().unwrap(),
        vec![(
            DirectLandingRequest {
                run_id: RUN_ID.into(),
                branch: "main".into(),
                before_commit: base.clone(),
                after_commit: candidate,
                task_ids: vec![TASK_ID.into()],
                handoff_id: Some("handoff-1".into()),
            },
            base,
        )],
        "one intent, naming the handoff and its task, retained while main was still at the base"
    );
    assert_eq!(
        host.steps.lock().unwrap().last(),
        Some(&HandoffLandingStep::Complete)
    );
}

#[test]
fn a_candidate_that_cannot_fast_forward_records_no_landing_intent() {
    let sandbox = tempdir().unwrap();
    let host = owner_checkout(sandbox.path());
    std::fs::write(host.repo.join("other.txt"), "another landing\n").unwrap();
    git(&host.repo, &["add", "other.txt"]);
    git(&host.repo, &["commit", "-q", "-m", "another landing"]);

    land(&host).expect_err("a diverged landing branch stops the attempt");

    assert!(host.intents.lock().unwrap().is_empty());
    assert_eq!(
        host.steps.lock().unwrap().last(),
        Some(&HandoffLandingStep::Stop { repairable: true }),
        "a landing branch that moved past the candidate's base earns an automatic repair"
    );
}

/// Model the owner's journal guards at the RuntimeHost boundary: Stop needs a
/// resolved intent, and a settled attempt cannot accept more landing writes.
struct ReconciliationHost {
    context: Mutex<HandoffLandingContext>,
    updates: Mutex<Vec<HandoffLandingUpdate>>,
    status: Value,
}

impl RuntimeHost for ReconciliationHost {
    fn handoff_landing_context(
        &self,
        _handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.lock().unwrap().clone())
    }

    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        let mut updates = self.updates.lock().unwrap();
        if updates
            .last()
            .is_some_and(|u| matches!(u.step, HandoffLandingStep::Stop { .. }))
        {
            return Err(OrbitError::InvalidInput(
                "landing attempt is already settled".into(),
            ));
        }
        let mut context = self.context.lock().unwrap();
        match &update.step {
            HandoffLandingStep::ResolveIntent { intent_id, .. } => {
                assert_eq!(context.unresolved_merge_intent.as_ref(), Some(intent_id));
                context.unresolved_merge_intent = None;
            }
            HandoffLandingStep::Stop { .. } => {
                assert!(
                    context.unresolved_merge_intent.is_none(),
                    "Stop must follow intent resolution"
                );
            }
            other => panic!("a mismatched reconciled PR must not reach {other:?}"),
        }
        updates.push(update.clone());
        Ok(())
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        _input: Value,
    ) -> Result<Value, OrbitError> {
        assert_eq!(
            operation, "pr.status",
            "reconciliation must not send another merge"
        );
        Ok(json!({"pull_request": self.status}))
    }
}

#[test]
fn a_mismatched_merged_pr_resolves_the_intent_before_stopping_later_attempts() {
    for (field, replacement) in [
        ("headRefName", "another-branch"),
        ("baseRefName", "another-base"),
        ("headRefOid", "another-commit"),
    ] {
        let sandbox = tempdir().unwrap();
        let mut context = owner_checkout(sandbox.path()).context;
        context.candidate.delivery = HandoffDelivery::PullRequest { number: 42 };
        let intent_id = "handoff-1:42".to_string();
        // A lost merge reply leaves this intent for the next attempt to read.
        context.unresolved_merge_intent = Some(intent_id.clone());
        let mut status = json!({
            "state": "MERGED",
            "headRefName": context.candidate.source_branch,
            "baseRefName": context.candidate.base_branch,
            "headRefOid": context.candidate.candidate.commit,
            "mergeCommit": {"oid": "provider-merge"},
        });
        status[field] = json!(replacement);
        let host = ReconciliationHost {
            context: Mutex::new(context),
            updates: Mutex::default(),
            status,
        };

        let error = land(&host).expect_err("a foreign merged identity cannot complete the handoff");
        let updates = host.updates.lock().unwrap().clone();
        assert_eq!(
            updates.len(),
            2,
            "one resolved intent and one durable Stop for {field}"
        );
        assert_eq!(
            updates[0].step,
            HandoffLandingStep::ResolveIntent {
                intent_id,
                merged: true
            }
        );
        assert_eq!(
            updates[1].step,
            HandoffLandingStep::Stop { repairable: false },
            "stale delivery evidence is not a base conflict a rebase repairs"
        );
        let reason: String = serde_json::from_str(&updates[1].evidence).unwrap();
        assert!(
            reason.contains("delivery_evidence_stale")
                && reason.contains(field)
                && reason.contains(replacement),
            "the Stop must retain the observed identity mismatch: {reason}"
        );
        assert!(
            matches!(error, OrbitError::Execution(ref message) if message == &format!("handoff_land: {reason}"))
        );
        let resolved: Value = serde_json::from_str(&updates[0].evidence).unwrap();
        assert_eq!(resolved["provider_state"], host.status);
        assert_eq!(resolved["delivery_error"], reason);
        assert!(
            host.handoff_landing_context("handoff-1")
                .unwrap()
                .unresolved_merge_intent
                .is_none()
        );

        let retry = land(&host).expect_err("a later attempt sees the settled Stop");
        assert!(
            matches!(retry, OrbitError::InvalidInput(ref message) if message == "landing attempt is already settled")
        );
        assert_eq!(
            *host.updates.lock().unwrap(),
            updates,
            "retry retains the original Stop evidence"
        );
    }
}

/// A pull request the provider reports open with a fixed merge state and
/// status rollup. It answers only status reads; any other operation is
/// recorded and refused, so a test sees whether the landing tried to merge.
struct OpenPullRequestHost {
    context: HandoffLandingContext,
    steps: Mutex<Vec<HandoffLandingStep>>,
    merge_state: &'static str,
    rollup: Value,
    sent: Mutex<Vec<String>>,
}

impl OpenPullRequestHost {
    fn new(context: HandoffLandingContext, merge_state: &'static str, rollup: Value) -> Self {
        Self {
            context,
            steps: Mutex::default(),
            merge_state,
            rollup,
            sent: Mutex::default(),
        }
    }
}

impl RuntimeHost for OpenPullRequestHost {
    fn handoff_landing_context(
        &self,
        _handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        Ok(self.context.clone())
    }

    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        self.steps.lock().unwrap().push(update.step.clone());
        Ok(())
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        _input: Value,
    ) -> Result<Value, OrbitError> {
        if operation != "pr.status" {
            self.sent.lock().unwrap().push(operation.to_string());
            return Err(OrbitError::Execution(format!(
                "fixture sends no {operation}"
            )));
        }
        let candidate = &self.context.candidate;
        Ok(json!({"pull_request": {
            "state": "OPEN",
            "headRefName": candidate.source_branch,
            "baseRefName": candidate.base_branch,
            "headRefOid": candidate.candidate.commit,
            "mergeStateStatus": self.merge_state,
            "reviewDecision": "",
            "statusCheckRollup": self.rollup,
        }}))
    }
}

/// [ORB-14261] Only a stop a rebase can cure earns the automatic repair: a
/// conflicting or stale base. A protection refusal stays an operator's call.
#[test]
fn only_a_conflicting_or_stale_base_stops_a_pull_request_for_repair() {
    for (merge_state, repairable) in [("DIRTY", true), ("BEHIND", true), ("DRAFT", false)] {
        let sandbox = tempdir().unwrap();
        let mut context = owner_checkout(sandbox.path()).context;
        context.candidate.delivery = HandoffDelivery::PullRequest { number: 42 };
        let host = OpenPullRequestHost::new(context, merge_state, json!([]));

        land(&host).expect_err("an unmergeable pull request stops the landing");

        assert_eq!(
            *host.steps.lock().unwrap(),
            vec![HandoffLandingStep::Stop { repairable }],
            "merge state {merge_state}"
        );
        assert!(
            host.sent.lock().unwrap().is_empty(),
            "a stopped landing sends no merge"
        );
    }
}

/// [ORB-15677] A check the before-landing review owed the pull request's
/// hosted CI gates the merge itself, even where branch protection does not
/// require it: the landing merges only once that check passed on the pinned
/// head, stops when it failed, and never merges while it is absent or still
/// running. A local candidate, which no hosted CI runs, never lands with one.
#[test]
fn an_owed_hosted_check_must_pass_before_the_owner_merges() {
    const OWED: &str = "CodeQL / Analyze (rust)";
    let check = |conclusion: Option<&str>| {
        json!({
            "__typename": "CheckRun",
            "name": "Analyze (rust)",
            "workflowName": "CodeQL",
            "status": if conclusion.is_some() { "COMPLETED" } else { "IN_PROGRESS" },
            "conclusion": conclusion.unwrap_or_default(),
        })
    };
    let other = json!({"__typename": "CheckRun", "name": "fast", "workflowName": "CI",
                       "status": "COMPLETED", "conclusion": "SUCCESS"});
    for (case, rollup, merges, stop_reason) in [
        ("passed", json!([other, check(Some("SUCCESS"))]), true, None),
        (
            "failed",
            json!([other, check(Some("FAILURE"))]),
            false,
            Some("failed"),
        ),
        (
            "running",
            json!([other, check(None)]),
            false,
            Some("not yet passed"),
        ),
        ("absent", json!([other]), false, Some("not yet passed")),
    ] {
        let sandbox = tempdir().unwrap();
        let mut context = owner_checkout(sandbox.path()).context;
        context.candidate.delivery = HandoffDelivery::PullRequest { number: 42 };
        context.owed_hosted_checks = vec![OWED.into()];
        let host = OpenPullRequestHost::new(context, "CLEAN", rollup);

        let error = land_with(&host, json!({"max_wait_seconds": 0}))
            .expect_err("the fixture never reports a merge");

        let sent = host.sent.lock().unwrap().clone();
        assert_eq!(!sent.is_empty(), merges, "{case}: {sent:?} {error}");
        let steps = host.steps.lock().unwrap().clone();
        match stop_reason {
            None => assert!(steps.is_empty(), "{case}: {steps:?}"),
            Some(reason) => {
                assert_eq!(
                    steps,
                    vec![HandoffLandingStep::Stop { repairable: false }],
                    "{case}"
                );
                let message = error.to_string();
                assert!(
                    message.contains(OWED) && message.contains(reason),
                    "{case}: {message}"
                );
            }
        }
    }

    let sandbox = tempdir().unwrap();
    let mut host = owner_checkout(sandbox.path());
    host.context.owed_hosted_checks = vec![OWED.into()];
    let tip = git(&host.repo, &["rev-parse", "main"]);
    let error = land(&host).expect_err("no hosted CI runs a local candidate");
    assert!(error.to_string().contains(OWED), "{error}");
    assert_eq!(
        *host.steps.lock().unwrap(),
        vec![HandoffLandingStep::Stop { repairable: false }]
    );
    assert_eq!(
        git(&host.repo, &["rev-parse", "main"]),
        tip,
        "nothing landed"
    );
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=test@orbit.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}
