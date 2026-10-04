//! Attribution of landed pull requests to the tasks whose accepted handoffs
//! delivered them [ORB-13894]: only an exact repository, landing branch and
//! pull request number names a task, and attribution only ever adds.

use chrono::Utc;
use orbit_types::workflow::ReviewTiming;
use orbit_types::workflow::automation::{Delivery, SourceRevision};
use orbit_types::workflow::handoff::{
    AcceptedHandoff, HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition,
    TaskHandoff,
};

use super::super::provider::attribute;

fn revision(fill: &str) -> SourceRevision {
    SourceRevision {
        commit: fill.repeat(40),
        tree: fill.repeat(40),
    }
}

fn delivery(key: &str, task_ids: &[&str]) -> Delivery {
    Delivery {
        key: key.into(),
        repository: "owner/repo".into(),
        branch: "agent-main".into(),
        before: revision("a"),
        after: revision("b"),
        commits: vec!["b".repeat(40)],
        task_ids: task_ids.iter().map(|id| (*id).to_string()).collect(),
        evidence_reference: "https://example.invalid/pull/1".into(),
        evidence_digest: "digest".into(),
        landed_at: Utc::now(),
    }
}

fn handoff(
    task_id: &str,
    repository: &str,
    landing_branch: &str,
    delivery: HandoffDelivery,
) -> AcceptedHandoff {
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
                candidate: revision("c"),
                base: revision("d"),
                delivery,
            },
            review: HandoffReview {
                policy: ReviewTiming::AfterLanding,
                disposition: HandoffReviewDisposition::DeferredToLanding,
            },
            execution_summary: "Outcome: success".into(),
            validation: vec![],
        },
        required_commands: vec!["test".into()],
        accepted_at: Utc::now(),
    }
}

#[test]
fn only_an_exact_pull_request_identity_attributes_a_landing() {
    let pr = |number| HandoffDelivery::PullRequest { number };
    let handoffs = [
        handoff("T-1", "owner/repo", "agent-main", pr(7)),
        // The same pull request handed off again after a stopped landing.
        handoff("T-1", "owner/repo", "agent-main", pr(7)),
        handoff("T-2", "other/repo", "agent-main", pr(8)),
        handoff("T-3", "owner/repo", "main", pr(9)),
        handoff(
            "T-4",
            "owner/repo",
            "agent-main",
            HandoffDelivery::LocalCandidate,
        ),
        handoff("T-5", "owner/repo", "agent-main", pr(70)),
    ];
    let mut deliveries = vec![
        delivery("pr:owner/repo:agent-main:7", &[]),
        delivery("pr:owner/repo:agent-main:8", &[]),
        delivery("pr:owner/repo:agent-main:9", &[]),
        delivery("direct:owner/repo:agent-main:run-1", &["T-0"]),
        delivery("pr:owner/repo:agent-main:70", &["T-9"]),
    ];

    attribute(&mut deliveries, &handoffs);

    let attributed = deliveries
        .iter()
        .map(|delivery| (delivery.key.as_str(), delivery.task_ids.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        attributed,
        vec![
            ("pr:owner/repo:agent-main:7", vec!["T-1".to_string()]),
            // Another repository's PR 8 and another branch's PR 9 are not these.
            ("pr:owner/repo:agent-main:8", vec![]),
            ("pr:owner/repo:agent-main:9", vec![]),
            (
                "direct:owner/repo:agent-main:run-1",
                vec!["T-0".to_string()]
            ),
            (
                "pr:owner/repo:agent-main:70",
                vec!["T-9".to_string(), "T-5".to_string()]
            ),
        ]
    );
}
