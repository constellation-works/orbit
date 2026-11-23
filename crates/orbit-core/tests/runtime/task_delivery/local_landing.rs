//! A local `git_merge` landing checkpoint read through public delivery
//! observation. A valid completion hold is deliberate no-landing evidence;
//! malformed output stays unavailable; a real fast-forward is still a landing.

use std::path::Path;

use chrono::Duration;
use orbit_types::workflow::{
    CommitObservationStatus, DeliveryEvidenceGap, JobRunState, LandingMethod,
    LandingObservationStatus, PipelineState, RunDeliveryStatus,
};
use serde_json::{Value, json};

use super::Fixture;

const RUN: &str = "local";
const JOB: &str = "fixture_local_delivery";
const JOB_YAML: &str = "schemaVersion: 2\nkind: Job\nmetadata:\n  name: fixture_local_delivery\nspec:\n  state: enabled\n  task_delivery: {}\n  steps:\n    - id: locks\n      target: activity:release_locks\n    - id: commit\n      target: activity:git_commit\n    - id: merge\n      target: activity:git_merge\n";

impl Fixture {
    /// The shipped local route: release locks, commit, then the real
    /// `git_merge` as the landing step.
    fn local_delivery_job(&self) {
        let resources = self._root.path().join("home/.orbit/resources");
        let activities = resources.join("activities");
        std::fs::create_dir_all(&activities).unwrap();
        for name in ["git_commit", "git_merge", "release_locks"] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("assets/activities/{name}.yaml")),
                activities.join(format!("{name}.yaml")),
            )
            .unwrap();
        }
        std::fs::write(resources.join("jobs").join(format!("{JOB}.yaml")), JOB_YAML).unwrap();
    }
}

/// A completion-authorized run whose commit was performed and whose local
/// landing checkpoint is `merge`.
fn local_run(fixture: &Fixture, merge: Value) -> String {
    fixture.local_delivery_job();
    let mut run = fixture.run(
        RUN,
        JOB,
        1,
        Some(json!({"task_ids": [fixture.task], "completion": "done"})),
    );
    run.state = JobRunState::Success;
    run.finished_at = Some(run.created_at + Duration::seconds(3));
    fixture.save_run(&run);
    fixture.step(&run, 1, "commit");
    fixture.step(&run, 2, "merge");
    let commit = json!({
        "phase": "commit",
        "decision": "performed",
        "committed": true,
        "task_id": fixture.task,
        "job_run_id": run.run_id,
        "base_sha": "a".repeat(40),
        "commit_sha": "b".repeat(40),
    });
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    state.record_step(0, JobRunState::Success, Some(json!({})), None);
    state.record_pipeline_output("locks", json!({}));
    state.record_step(1, JobRunState::Success, Some(commit.clone()), None);
    state.record_pipeline_output("commit", commit);
    state.record_step(2, JobRunState::Success, Some(merge.clone()), None);
    state.record_pipeline_output("merge", merge);
    fixture
        .runtime
        .write_run_state(&run.run_id, &state)
        .unwrap();
    run.run_id
}

/// Each case is a fresh fixture. Only the valid completion hold is a
/// deliberate no-landing; the malformed shapes keep the landing unavailable,
/// and a real fast-forward is still merged.
pub(super) fn completion_held_local_merge_is_observed_as_no_landing() {
    for case in [
        "completion_held",
        "base_not_text",
        "merged_not_boolean",
        "merged_true_without_base",
        "local_fast_forward",
    ] {
        let fixture = Fixture::new();
        let merge = match case {
            "completion_held" => json!({
                "merged": false,
                "reason": "completion_held",
                "held_task_ids": [fixture.task],
                "completion_hold": {
                    "rule": "docs",
                    "field": "paths",
                    "pattern": "docs/**",
                    "matched": "docs/guide.md",
                    "run_id": RUN,
                },
            }),
            "base_not_text" => json!({"base": 7}),
            "merged_not_boolean" => json!({"merged": "false", "reason": "completion_held"}),
            "merged_true_without_base" => json!({"merged": true}),
            "local_fast_forward" => json!({
                "merged": true,
                "base": "main",
                "base_commit": "c".repeat(40),
                "landed_commit": "c".repeat(40),
                "workspace_path": "/fixture/workspace",
                "workspace_branch": "orbit/fixture",
            }),
            _ => unreachable!(),
        };
        let run_id = local_run(&fixture, merge);
        let observed = fixture
            .runtime
            .observe_task_delivery(&fixture.task, Some(&run_id))
            .unwrap();
        assert_eq!(
            observed.commit.status,
            CommitObservationStatus::Committed,
            "{case}: {:?}",
            observed.commit
        );
        match case {
            "completion_held" => {
                assert_eq!(
                    observed.landing.status,
                    LandingObservationStatus::NotRequested,
                    "{case}: {:?}",
                    observed.landing
                );
                assert!(
                    observed.landing.reason.is_none(),
                    "{case}: {:?}",
                    observed.landing
                );
                assert_eq!(
                    observed.delivery_status,
                    RunDeliveryStatus::Committed,
                    "{case}: {observed:?}"
                );
            }
            "local_fast_forward" => {
                assert_eq!(
                    observed.landing.status,
                    LandingObservationStatus::Merged,
                    "{case}: {:?}",
                    observed.landing
                );
                assert!(
                    matches!(
                        observed.landing.method,
                        Some(LandingMethod::LocalFastForward)
                    ),
                    "{case}: {:?}",
                    observed.landing
                );
                assert_eq!(
                    observed.landing.landed_commit,
                    Some("c".repeat(40)),
                    "{case}: {:?}",
                    observed.landing
                );
                assert_eq!(
                    observed.delivery_status,
                    RunDeliveryStatus::Landed,
                    "{case}: {observed:?}"
                );
            }
            _ => {
                assert_eq!(
                    observed.landing.status,
                    LandingObservationStatus::Unavailable,
                    "{case}: {:?}",
                    observed.landing
                );
                assert!(
                    matches!(
                        observed.landing.reason,
                        Some(DeliveryEvidenceGap::OutputMalformed)
                    ),
                    "{case}: {:?}",
                    observed.landing
                );
                assert_eq!(
                    observed.delivery_status,
                    RunDeliveryStatus::Unavailable,
                    "{case}: {observed:?}"
                );
            }
        }
    }
}
