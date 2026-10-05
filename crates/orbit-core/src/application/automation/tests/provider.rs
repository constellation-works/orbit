//! A landed pull request names the task whose accepted handoff delivered it
//! only when repository, landing branch and number all match [ORB-13894]: the
//! same number in another repository or on another branch is a different PR.

use chrono::Utc;
use orbit_types::workflow::ReviewTiming;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{
    AcceptedHandoff, HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition,
    TaskHandoff,
};

use super::super::provider::handoff_tasks;

fn handoff(
    task_id: &str,
    repository: &str,
    landing_branch: &str,
    delivery: HandoffDelivery,
) -> AcceptedHandoff {
    let revision = |fill: &str| SourceRevision {
        commit: fill.repeat(40),
        tree: fill.repeat(40),
    };
    AcceptedHandoff {
        handoff_id: format!("handoff-{task_id}"),
        handoff: TaskHandoff {
            schema_version: 1,
            workspace_id: "workspace".into(),
            task_id: task_id.into(),
            claim_id: format!("claim-{task_id}"),
            machine_id: "follower".into(),
            run_id: "leaf".into(),
            candidate: HandoffCandidate {
                repository: repository.into(),
                source_branch: format!("orbit/{task_id}"),
                base_branch: landing_branch.into(),
                landing_branch: landing_branch.into(),
                candidate: revision("a"),
                base: revision("b"),
                delivery,
            },
            review: HandoffReview {
                policy: ReviewTiming::None,
                disposition: HandoffReviewDisposition::NotRequired,
            },
            execution_summary: "Outcome: success".into(),
            validation: vec![],
            footprint_widening: vec![],
        },
        required_commands: vec![],
        accepted_at: Utc::now(),
    }
}

#[test]
fn only_the_exact_pull_request_identity_names_a_handoffs_task() {
    let pr = |number| HandoffDelivery::PullRequest { number };
    let handoffs = [
        handoff("T-1", "owner/repo", "main", pr(7)),
        handoff("T-2", "other/repo", "main", pr(7)),
        handoff("T-3", "owner/repo", "release", pr(7)),
        handoff("T-4", "owner/repo", "main", pr(70)),
        handoff("T-5", "owner/repo", "main", HandoffDelivery::LocalCandidate),
    ];

    assert_eq!(
        handoff_tasks(&handoffs, "pr:owner/repo:main:7"),
        vec!["T-1".to_string()]
    );
    assert!(handoff_tasks(&handoffs, "pr:owner/repo:main:8").is_empty());
    assert!(handoff_tasks(&handoffs, "direct:owner/repo:main:run-1").is_empty());
}
