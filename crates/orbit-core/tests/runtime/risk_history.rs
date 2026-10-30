#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

//! Landed history that gives the owner's task-riskiness model a diff-lines
//! fit, so a captured selective policy freezes a risk-model identity that
//! carries its fit window and observations.

use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::TaskStatus;
use orbit_core::application::task::TaskAddParams;
use orbit_store::contracts::{
    DiffClassCounts, LandedDiffSource, LandedDiffStats, LandedDiffStoreBackend as _,
};
use orbit_types::task::{TaskRelation, TaskRelationType};
use orbit_types::workflow::SelectiveBeforeLandingPolicy;

/// Landed line counts of the seeded done tasks, with the outcome of each.
/// Regressed and clean sizes interleave so no size threshold separates them.
const OUTCOMES: [(u64, bool); 6] = [
    (5, false),
    (10, true),
    (30, false),
    (50, true),
    (60, false),
    (100, true),
];

/// Add one done task that landed `lines` changed lines, with a regression
/// task against it when `regressed`.
fn seed_landed_task(runtime: &OrbitRuntime, lines: u64, regressed: bool) {
    let id = runtime
        .add_task(TaskAddParams {
            title: format!("riskiness history {lines}"),
            description: "riskiness history fixture".into(),
            status: Some(TaskStatus::Done),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    if regressed {
        runtime
            .add_task(TaskAddParams {
                title: format!("regression of {lines}"),
                description: "riskiness history fixture".into(),
                relations: vec![TaskRelation {
                    relation_type: TaskRelationType::RegressionFrom,
                    target: id.parse().unwrap(),
                    regression: None,
                }],
                ..Default::default()
            })
            .unwrap();
    }
    runtime
        .sqlite_store()
        .unwrap()
        .record_landed_diff_stats(&LandedDiffStats {
            workspace_id: runtime.workspace_id().unwrap(),
            task_id: id.clone(),
            base_commit: format!("{:040x}", 2 * lines),
            landed_commit: format!("{:040x}", 2 * lines + 1),
            landed_at: Utc::now() - Duration::minutes(1),
            recorded_at: Utc::now(),
            job_run_id: None,
            source: LandedDiffSource::Landing,
            crew: None,
            model: None,
            complexity: None,
            task_type: None,
            code: DiffClassCounts {
                added: lines,
                deleted: 0,
                files: 1,
            },
            tests: DiffClassCounts::default(),
            other: DiffClassCounts::default(),
        })
        .unwrap();
}

/// Seed enough landed history, regressed and clean, for a diff-lines fit.
pub(crate) fn seed_fitted_history(runtime: &OrbitRuntime) {
    for (lines, regressed) in OUTCOMES {
        seed_landed_task(runtime, lines, regressed);
    }
}

/// Land one more clean task, so a later model refits over different history.
pub(crate) fn land_one_more_clean_task(runtime: &OrbitRuntime) {
    seed_landed_task(runtime, 200, false);
}

/// The policy configured now with the risk model frozen as an admission
/// freezes it, asserting the model carries a fit window.
pub(crate) fn frozen_selective_policy(runtime: &OrbitRuntime) -> SelectiveBeforeLandingPolicy {
    let mut policy = runtime
        .operation_policy()
        .selective_before_landing_policy()
        .expect("selective policy is active");
    let identity = runtime
        .task_riskiness_model()
        .unwrap()
        .diff_model_identity();
    assert!(
        identity.contains("since="),
        "seeded history must give the model a fit: {identity}"
    );
    policy.freeze_risk_model(identity);
    policy
}
