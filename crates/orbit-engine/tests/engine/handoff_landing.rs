//! The owner's landing of a claimed candidate.
//!
//! A fast-forward has no provider identity, so the owner's delivery consumers
//! can attribute it only from a direct landing intent retained before the
//! branch moves. The landing names the accepted handoff and its task in that
//! intent; a candidate that cannot fast-forward lands nothing and records none
//! [ORB-13894]. PR reconciliation resolves external uncertainty before stopping
//! a mismatched delivery, so subsequent attempts see the settled state.

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
        },
        steps: Mutex::default(),
        intents: Mutex::default(),
    }
}

fn land(host: &impl RuntimeHost) -> Result<Value, OrbitError> {
    execute_deterministic_action(
        host,
        "handoff_land",
        &json!({}),
        &json!({"handoff_id": "handoff-1", "task_id": TASK_ID, "run_id": RUN_ID}),
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
        Some(&HandoffLandingStep::Stop)
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
            .is_some_and(|u| u.step == HandoffLandingStep::Stop)
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
            HandoffLandingStep::Stop => {
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
        assert_eq!(updates[1].step, HandoffLandingStep::Stop);
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
