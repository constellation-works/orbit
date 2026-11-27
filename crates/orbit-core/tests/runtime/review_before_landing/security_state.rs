use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::application::task::TaskUpdateParams;
use orbit_core::runtime::task_pr_forge::{
    ForgePullRequest, LeasePullRequest, PullRequestBlockers, PullRequestMergeOutcome, TaskPrForge,
};
use orbit_engine::{Dispatch, ReviewGate, RunLifecycle, execute_deterministic_action};
use orbit_types::task::{SECURITY_STATE_REVIEW_TAG, SKIP_REVIEW_TAG};
use orbit_types::workflow::{JobRunState, ReviewTiming};
use serde_json::{Value, json};

use super::{Fixture, candidate_reviewed, git, published};

/// A green PR that records whether Orbit requests its exact-head merge.
struct SecurityReviewForge {
    pull: ForgePullRequest,
    head: String,
    merged: AtomicBool,
}

impl TaskPrForge for SecurityReviewForge {
    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(if self.merged.load(Ordering::Relaxed) {
            Vec::new()
        } else {
            vec![self.pull.clone()]
        })
    }

    fn pull_request_blockers(
        &self,
        _: &Path,
        _: u64,
    ) -> Result<Option<PullRequestBlockers>, OrbitError> {
        Ok(Some(PullRequestBlockers {
            pull: self.pull.clone(),
            head_sha: self.head.clone(),
            conflicting: false,
            failed_required_checks: Vec::new(),
            pending_required_checks: Vec::new(),
            passed_required_checks: vec!["fixture check".into()],
            unreported_required_checks: Vec::new(),
            requires_status_checks: Some(true),
        }))
    }

    fn lease_pull_request(
        &self,
        _: &Path,
        _: &str,
    ) -> Result<Option<LeasePullRequest>, OrbitError> {
        Ok(Some(LeasePullRequest {
            number: self.pull.number,
            base_branch: "main".into(),
            merged: self.merged.load(Ordering::Relaxed),
        }))
    }

    fn merge_pull_request(
        &self,
        _: &Path,
        number: u64,
        head: &str,
        _: &orbit_types::workflow::MergeMessage,
    ) -> Result<PullRequestMergeOutcome, OrbitError> {
        assert_eq!(number, self.pull.number);
        assert_eq!(head, self.head);
        assert!(!self.merged.swap(true, Ordering::Relaxed));
        Ok(PullRequestMergeOutcome::Merged {
            merge_commit: self.head.clone(),
        })
    }

    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        Ok(self.pull.author.clone())
    }

    fn close_pull_request(&self, _: &Path, _: u64, _: &str) -> Result<(), OrbitError> {
        Ok(())
    }
}

pub(super) fn security_state_review_is_required_at_the_captured_delivery_layer() {
    for (timing, reviewer_available) in [
        (ReviewTiming::None, true),
        (ReviewTiming::BeforePr, true),
        (ReviewTiming::BeforeLanding, true),
        (ReviewTiming::None, false),
    ] {
        let mut fixture = Fixture::security_review(timing, reviewer_available);
        let gate = |fixture: &Fixture, input: &Value| {
            orbit_core::test_support::EngineHost::new(&fixture.runtime).run_deterministic(
                "review_gate_admit",
                &json!({}),
                input,
                Default::default(),
            )
        };
        fixture
            .runtime
            .update_task_with_identity(
                &fixture.task_id,
                TaskUpdateParams {
                    tags: Some(vec![SKIP_REVIEW_TAG.into()]),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        let untagged = gate(&fixture, &fixture.input).unwrap();
        assert_eq!(untagged["applies"], false);
        assert_eq!(
            untagged["reason"],
            match timing {
                ReviewTiming::BeforePr => "skip_review_light_delivery",
                ReviewTiming::BeforeLanding => "review_before_landing",
                _ => "review_before_pr_off",
            }
        );

        fixture
            .runtime
            .update_task_with_identity(
                &fixture.task_id,
                TaskUpdateParams {
                    tags: Some(vec![
                        SECURITY_STATE_REVIEW_TAG.into(),
                        SKIP_REVIEW_TAG.into(),
                    ]),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();

        let err = gate(
            &fixture,
            &json!({
                "job_run_id": fixture.input["job_run_id"],
                "completed_task_ids": [fixture.task_id],
                "mode": "local",
            }),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains(SECURITY_STATE_REVIEW_TAG) && err.contains("PR route"),
            "{err}"
        );

        let head = git(&fixture.repo, &["rev-parse", "HEAD"]);
        if timing == ReviewTiming::None && reviewer_available {
            let base = git(&fixture.repo, &["rev-parse", "main"]);
            git(
                &fixture.repo,
                &["worktree", "add", ".orbit/tmp/local-base", "main"],
            );
            let host = orbit_core::test_support::EngineHost::new(&fixture.runtime);
            let mut local_input = fixture.input.clone();
            local_input["run_id"] = fixture.input["job_run_id"].clone();
            let err = execute_deterministic_action(
                &host,
                "git_merge",
                &json!({}),
                &local_input,
                false,
                &Default::default(),
                None,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains(SECURITY_STATE_REVIEW_TAG), "{err}");
            assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), base);
        }
        let forge = Arc::new(SecurityReviewForge {
            pull: ForgePullRequest {
                number: 42,
                head_branch: format!("orbit/{}-01234567", fixture.task_id),
                body: format!("Delivers {}.", fixture.task_id),
                author: "orbit-bot".into(),
            },
            head: head.clone(),
            merged: AtomicBool::new(false),
        });
        if timing != ReviewTiming::BeforePr {
            published(&fixture);
            let host = orbit_core::test_support::EngineHost::new(&fixture.runtime);
            let run_id = fixture.input["job_run_id"].as_str().unwrap();
            host.mark_job_run_running(run_id, Utc::now(), std::process::id())
                .unwrap();
            host.finalize_job_run(run_id, JobRunState::Success, Utc::now(), None)
                .unwrap();
            fixture.runtime = fixture.runtime.clone().with_task_pr_forge(forge.clone());
            let refused = fixture
                .runtime
                .land_task_pull_request(&fixture.task_id, None)
                .unwrap_err()
                .to_string();
            assert!(refused.contains("required pre-landing review"), "{refused}");
            assert!(!forge.merged.load(Ordering::Relaxed));
        }

        fixture.input["before_landing"] = json!(timing != ReviewTiming::BeforePr);
        let admitted = gate(&fixture, &fixture.input);
        if !reviewer_available {
            let err = admitted.unwrap_err().to_string();
            assert!(
                err.contains("review_crew_unconfigured") && err.contains(SECURITY_STATE_REVIEW_TAG),
                "{err}"
            );
            continue;
        }
        let admitted = admitted.unwrap();
        assert_eq!(admitted["applies"], true, "{admitted}");
        assert_eq!(admitted["timing_source"], SECURITY_STATE_REVIEW_TAG);
        assert_eq!(
            admitted["timing"],
            if timing == ReviewTiming::BeforePr {
                "before-pr"
            } else {
                "before-landing"
            }
        );
        fixture.input["admission"] = admitted;
        fixture.report("accept");
        let settled = fixture.settle().unwrap();
        assert_eq!(settled["gate"], "passed");
        assert_eq!(
            settled["reviewed_head_sha"],
            git(&fixture.repo, &["rev-parse", "HEAD"])
        );

        let mut other_layer = fixture.input.clone();
        other_layer["before_landing"] = json!(timing == ReviewTiming::BeforePr);
        assert_eq!(gate(&fixture, &other_layer).unwrap()["applies"], false);
        assert_eq!(candidate_reviewed(&fixture).0.attempts.len(), 1);
        let host = orbit_core::test_support::EngineHost::new(&fixture.runtime);
        let task = fixture.runtime.get_task(&fixture.task_id).unwrap();
        assert!(host.security_state_review_ready(&task, &head).unwrap());
        let base = git(&fixture.repo, &["rev-parse", "main"]);
        assert!(!host.security_state_review_ready(&task, &base).unwrap());
        if timing != ReviewTiming::BeforePr {
            let landing = fixture
                .runtime
                .land_task_pull_request(&fixture.task_id, None)
                .unwrap();
            assert_eq!(landing.merge_commit, head);
            assert!(forge.merged.load(Ordering::Relaxed));
        }
    }
}
