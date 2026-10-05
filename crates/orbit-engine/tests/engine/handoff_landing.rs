//! The owner's landing of an owner-local claimed candidate [ORB-13894].
//!
//! A fast-forward has no provider identity, so the owner's delivery consumers
//! can attribute it only from a direct landing intent retained before the
//! branch moves. The landing names the accepted handoff and its task in that
//! intent; a candidate that cannot fast-forward lands nothing and records none.

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
use serde_json::json;
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

fn land(host: &LandingHost) -> Result<serde_json::Value, OrbitError> {
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
