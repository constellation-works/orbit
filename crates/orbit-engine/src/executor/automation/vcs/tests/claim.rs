//! The claimed handoff carries exactly the owner review policy captured by its claim [ORB-13894].

use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_types::workflow::handoff::{HandoffReview, TaskHandoff};
use serde_json::json;
use tempfile::TempDir;

use crate::context::{ClaimExecutionContext, RuntimeHost};

use super::super::claim::{claim_handoff, observe_candidate};

struct HandoffHost {
    context: ClaimExecutionContext,
    handoffs: Mutex<Vec<TaskHandoff>>,
}

impl RuntimeHost for HandoffHost {
    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError> {
        Ok(self.context.clone())
    }

    fn record_claim_handoff(&self, handoff: &TaskHandoff) -> Result<(), OrbitError> {
        self.handoffs.lock().unwrap().push(handoff.clone());
        Ok(())
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn workspace() -> TempDir {
    let workspace = tempfile::tempdir().expect("temporary git workspace");
    git(workspace.path(), &["init", "-b", "main"]);
    git(workspace.path(), &["config", "user.name", "Orbit test"]);
    git(
        workspace.path(),
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    std::fs::write(workspace.path().join("file.txt"), "base\n").unwrap();
    git(workspace.path(), &["add", "file.txt"]);
    git(workspace.path(), &["commit", "-m", "base"]);
    git(workspace.path(), &["switch", "-c", "candidate"]);
    std::fs::write(workspace.path().join("file.txt"), "candidate\n").unwrap();
    git(workspace.path(), &["add", "file.txt"]);
    git(workspace.path(), &["commit", "-m", "candidate"]);
    workspace
}

fn context(review_policy: &str) -> ClaimExecutionContext {
    ClaimExecutionContext {
        workspace_id: "workspace".into(),
        task_id: "ORB-TEST".into(),
        claim_id: "claim-1".into(),
        machine_id: "follower".into(),
        run_id: "leaf-1".into(),
        ship_mode: "local".into(),
        base_branch: "main".into(),
        landing_branch: "main".into(),
        review_policy: review_policy.into(),
        required_commands: vec!["test".into()],
    }
}

#[test]
fn claim_handoff_projects_the_captured_policy_to_its_typed_disposition() {
    let workspace = workspace();
    for (policy, disposition) in [
        (
            "none",
            orbit_types::workflow::handoff::HandoffReviewDisposition::NotRequired,
        ),
        (
            "after-landing",
            orbit_types::workflow::handoff::HandoffReviewDisposition::DeferredToLanding,
        ),
    ] {
        let context = context(policy);
        let candidate = observe_candidate(
            workspace.path(),
            None,
            &context.base_branch,
            &context.landing_branch,
            orbit_types::workflow::handoff::HandoffDelivery::LocalCandidate,
            &context.workspace_id,
            "local",
        )
        .unwrap();
        let host = HandoffHost {
            context,
            handoffs: Mutex::default(),
        };
        let output = claim_handoff(
            &host,
            &json!({
                "workspace_path": workspace.path(),
                "candidate": candidate,
                "validation": [{"path":"test.json", "sha256":"digest"}]
            }),
        )
        .unwrap();

        assert_eq!(output["handed_off"], true);
        let recorded = host.handoffs.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].review.disposition, disposition);
        assert_eq!(
            recorded[0].review,
            HandoffReview::for_policy(policy).unwrap()
        );
    }
}

#[test]
fn claim_handoff_refuses_a_captured_policy_with_no_distributed_disposition() {
    let workspace = workspace();
    let context = context("before-pr");
    let candidate = observe_candidate(
        workspace.path(),
        None,
        &context.base_branch,
        &context.landing_branch,
        orbit_types::workflow::handoff::HandoffDelivery::LocalCandidate,
        &context.workspace_id,
        "local",
    )
    .unwrap();
    let host = HandoffHost {
        context,
        handoffs: Mutex::default(),
    };

    let result = claim_handoff(
        &host,
        &json!({
            "workspace_path": workspace.path().to_string_lossy(),
            "candidate": candidate,
            "validation": [{"path":"test.json", "sha256":"digest"}]
        }),
    );

    assert!(result.is_err());
    assert!(host.handoffs.lock().unwrap().is_empty());
}
