//! [ORB-10588] Pipeline-reliability aggregation.
//!
//! These cover the pure halves (`aggregate_job_runs` / `aggregate_recovery`),
//! which is where every judgement call the task had to settle actually lives:
//! which states count as failed, what the denominators are, and what happens
//! when a denominator is empty or too small to trust.

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_store::{ActivityInvocationCount, InvocationRunCoverage, JobRunOutcomeFact};
use orbit_types::workflow::JobActivityRoles;

use crate::metrics::reliability::{
    ActivityRole, BucketGranularity, MIN_CONFIDENT_SAMPLE, Rate, ReliabilityWindow,
    aggregate_job_runs, aggregate_recovery,
};

fn at(hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 1, hour, 30, 0)
        .single()
        .expect("valid timestamp")
}

fn fact(job_id: &str, state: &str, hour: u32) -> JobRunOutcomeFact {
    JobRunOutcomeFact {
        job_id: job_id.to_string(),
        state: state.to_string(),
        created_at: at(hour),
    }
}

fn hourly_window(from_hour: u32, to_hour: u32) -> ReliabilityWindow {
    ReliabilityWindow {
        label: format!("{}h", to_hour - from_hour),
        since: Utc
            .with_ymd_and_hms(2026, 8, 1, from_hour, 0, 0)
            .single()
            .expect("valid since"),
        until: Utc
            .with_ymd_and_hms(2026, 8, 1, to_hour, 0, 0)
            .single()
            .expect("valid until"),
        bucket: BucketGranularity::Hour,
    }
}

fn roles(step: &[&str], recovery: &[&str]) -> JobActivityRoles {
    JobActivityRoles {
        step: step.iter().map(|s| s.to_string()).collect(),
        recovery: recovery.iter().map(|s| s.to_string()).collect(),
    }
}

fn count(activity_id: &str, invocations: u64, runs: u64) -> ActivityInvocationCount {
    ActivityInvocationCount {
        activity_id: activity_id.to_string(),
        invocation_count: invocations,
        job_run_count: runs,
    }
}

#[test]
fn failed_timeout_and_interrupted_all_count_as_failures() {
    // Derived from the state values the store actually holds: a run that timed
    // out and a run whose owner died are both runs the pipeline meant to
    // finish and did not.
    let facts = vec![
        fact("j", "success", 1),
        fact("j", "failed", 1),
        fact("j", "timeout", 1),
        fact("j", "interrupted", 1),
    ];
    let result = aggregate_job_runs(&facts, &hourly_window(0, 3), false);

    assert_eq!(result.overall.succeeded, 1);
    assert_eq!(result.overall.failed, 3);
    assert_eq!(result.overall.settled(), 4);
    assert_eq!(result.overall.failure_rate().value, Some(0.75));
}

#[test]
fn cancelled_skipped_and_in_flight_stay_out_of_the_denominator() {
    let facts = vec![
        fact("j", "success", 1),
        fact("j", "failed", 1),
        fact("j", "cancelled", 1),
        fact("j", "skipped", 1),
        fact("j", "running", 1),
        fact("j", "pending", 1),
        fact("j", "retrying", 1),
    ];
    let result = aggregate_job_runs(&facts, &hourly_window(0, 3), false);

    assert_eq!(result.overall.total, 7);
    assert_eq!(result.overall.settled(), 2, "only success + failed settle");
    assert_eq!(result.overall.excluded(), 5);
    assert_eq!(result.overall.cancelled, 1);
    assert_eq!(result.overall.skipped, 1);
    assert_eq!(result.overall.in_flight, 3);
    // The whole point of the split: the two visible outcome counts must not be
    // mistakable for the population.
    assert_ne!(
        result.overall.succeeded + result.overall.failed,
        result.overall.total
    );
}

#[test]
fn an_unparseable_state_is_surfaced_rather_than_folded_into_an_outcome() {
    let facts = vec![fact("j", "success", 1), fact("j", "quantum_superposed", 1)];
    let result = aggregate_job_runs(&facts, &hourly_window(0, 3), false);

    assert_eq!(result.overall.unknown, 1);
    assert_eq!(result.overall.settled(), 1);
    assert_eq!(
        result.observed_states.get("quantum_superposed"),
        Some(&1),
        "the raw state value must remain visible as evidence"
    );
}

#[test]
fn observed_states_records_the_raw_store_values_behind_the_classification() {
    let facts = vec![
        fact("j", "success", 1),
        fact("j", "success", 1),
        fact("j", "interrupted", 1),
    ];
    let result = aggregate_job_runs(&facts, &hourly_window(0, 3), false);

    assert_eq!(result.observed_states.get("success"), Some(&2));
    assert_eq!(result.observed_states.get("interrupted"), Some(&1));
    assert_eq!(result.observed_states.len(), 2);
}

#[test]
fn a_zero_denominator_yields_no_percentage_at_all() {
    // A window holding only in-flight runs has nothing to divide by; emitting
    // 0% there would read as "nothing is failing".
    let facts = vec![fact("j", "running", 1)];
    let result = aggregate_job_runs(&facts, &hourly_window(0, 3), false);

    let rate = result.overall.failure_rate();
    assert_eq!(rate.denominator, 0);
    assert_eq!(rate.value, None);
    assert!(!rate.denominator_label.is_empty());
}

#[test]
fn a_rate_below_the_confidence_threshold_is_flagged_low_sample() {
    let small = Rate::new(1, MIN_CONFIDENT_SAMPLE - 1, "runs");
    let large = Rate::new(1, MIN_CONFIDENT_SAMPLE, "runs");

    assert!(small.low_sample, "a thin denominator must be marked");
    assert!(!large.low_sample);
    assert_eq!(large.denominator, MIN_CONFIDENT_SAMPLE);
}

#[test]
fn breakdowns_split_by_job_and_by_time_bucket() {
    let facts = vec![
        fact("alpha", "failed", 1),
        fact("alpha", "success", 1),
        fact("beta", "success", 2),
    ];
    let result = aggregate_job_runs(&facts, &hourly_window(0, 4), false);

    let alpha = result
        .by_job
        .iter()
        .find(|row| row.job_id == "alpha")
        .expect("alpha row");
    assert_eq!(alpha.counts.failed, 1);
    assert_eq!(alpha.counts.failure_rate().value, Some(0.5));

    let bucket_1 = result
        .over_time
        .iter()
        .find(|row| row.bucket_start.timestamp() == at(1).timestamp() - 30 * 60)
        .expect("hour-1 bucket");
    assert_eq!(bucket_1.counts.total, 2);
    let bucket_2 = result
        .over_time
        .iter()
        .find(|row| row.bucket_start.timestamp() == at(2).timestamp() - 30 * 60)
        .expect("hour-2 bucket");
    assert_eq!(bucket_2.counts.total, 1);
}

#[test]
fn every_bucket_in_the_window_is_present_including_empty_ones() {
    // A gap must render as a gap. Emitting only non-empty buckets would let a
    // quiet stretch read as continuous activity.
    let facts = vec![fact("j", "success", 0), fact("j", "success", 3)];
    let result = aggregate_job_runs(&facts, &hourly_window(0, 4), false);

    assert_eq!(result.over_time.len(), 4);
    assert_eq!(result.over_time[1].counts.total, 0);
    assert_eq!(result.over_time[2].counts.total, 0);
    assert_eq!(result.over_time[0].counts.total, 1);
    assert_eq!(result.over_time[3].counts.total, 1);
}

#[test]
fn window_bucket_width_follows_the_window_length() {
    let short = ReliabilityWindow::ending_at("24h", at(12), Duration::hours(24));
    let long = ReliabilityWindow::ending_at("30d", at(12), Duration::days(30));

    assert_eq!(short.bucket, BucketGranularity::Hour);
    assert_eq!(long.bucket, BucketGranularity::Day);
    assert_eq!(long.since, at(12) - Duration::days(30));
}

#[test]
fn truncation_is_carried_through_rather_than_dropped() {
    let result = aggregate_job_runs(&[fact("j", "success", 1)], &hourly_window(0, 3), true);
    assert!(result.truncated);
}

#[test]
fn recovery_rates_use_discovered_roles_and_state_their_denominators() {
    let counts = vec![
        count("implement", 100, 40),
        count("rescue", 25, 20),
        count("unrelated", 5, 5),
    ];
    let result = aggregate_recovery(
        &counts,
        &roles(&["implement"], &["rescue"]),
        InvocationRunCoverage {
            total_job_runs: 40,
            matching_job_runs: 20,
        },
    );

    assert_eq!(result.recovery_activities, vec!["rescue".to_string()]);
    assert_eq!(result.per_step_invocation.numerator, 25);
    assert_eq!(result.per_step_invocation.denominator, 100);
    assert_eq!(result.per_step_invocation.value, Some(0.25));
    assert_eq!(
        result.per_step_invocation.denominator_label,
        "step-activity invocations"
    );
    assert_eq!(result.per_job_run.value, Some(0.5));
    assert_eq!(
        result.per_job_run.denominator_label,
        "job runs with any recorded invocation"
    );
}

#[test]
fn an_activity_no_job_declares_is_counted_but_attributed_to_neither_rate() {
    let counts = vec![count("implement", 10, 5), count("mystery", 90, 5)];
    let result = aggregate_recovery(
        &counts,
        &roles(&["implement"], &["rescue"]),
        InvocationRunCoverage {
            total_job_runs: 5,
            matching_job_runs: 0,
        },
    );

    let mystery = result
        .by_activity
        .iter()
        .find(|row| row.activity_id == "mystery")
        .expect("mystery row");
    assert_eq!(mystery.role, ActivityRole::Unknown);
    assert_eq!(
        result.per_step_invocation.denominator, 10,
        "an undeclared activity must not inflate the step denominator"
    );
    assert_eq!(result.per_step_invocation.numerator, 0);
}

#[test]
fn a_dual_role_activity_is_excluded_from_the_numerator_and_flagged() {
    let counts = vec![count("shared", 30, 10), count("implement", 60, 10)];
    let result = aggregate_recovery(
        &counts,
        &roles(&["implement", "shared"], &["shared"]),
        InvocationRunCoverage {
            total_job_runs: 10,
            matching_job_runs: 0,
        },
    );

    assert_eq!(result.ambiguous_activities, vec!["shared".to_string()]);
    assert!(result.recovery_activities.is_empty());
    assert_eq!(
        result.per_step_invocation.numerator, 0,
        "an unattributable activity must not be counted as recovery"
    );
    assert_eq!(
        result.per_step_invocation.denominator, 60,
        "nor as a step activity"
    );
    let shared = result
        .by_activity
        .iter()
        .find(|row| row.activity_id == "shared")
        .expect("shared row");
    assert_eq!(shared.role, ActivityRole::Ambiguous);
}

#[test]
fn no_declared_recovery_activity_yields_an_empty_set_not_a_zero_rate() {
    let result = aggregate_recovery(
        &[count("implement", 50, 10)],
        &roles(&["implement"], &[]),
        InvocationRunCoverage {
            total_job_runs: 10,
            matching_job_runs: 0,
        },
    );

    assert!(result.recovery_activities.is_empty());
    assert_eq!(result.per_step_invocation.numerator, 0);
    assert_eq!(result.per_step_invocation.denominator, 50);
}

/// End-to-end over a real runtime: catalog discovery, both store reads, and
/// the aggregation wired together.
///
/// This is the test that pins the identifier contract. `invocations.activity_id`
/// is *not* uniformly the catalog activity name — the job executor records a
/// dispatched step under its **step id** and a recovery dispatch under the
/// **recovery activity name**. Discovering only target activity names would
/// leave every step invocation unattributed and collapse the recovery
/// denominator to zero.
mod end_to_end {
    use chrono::{Duration, Utc};
    use orbit_store::InvocationInsertParams;
    use orbit_types::telemetry::InvocationTrace;
    use orbit_types::workflow::{JobRun, JobRunState};
    use tempfile::tempdir;

    use crate::OrbitRuntime;
    use crate::metrics::reliability::{ActivityRole, ReliabilityWindow};

    const JOB: &str = "reliability_pipeline";

    fn runtime_with_job() -> (tempfile::TempDir, OrbitRuntime) {
        let root = tempdir().expect("create tempdir");
        let global_root = root.path().join("global");
        let workspace_root = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global_root).expect("create global root");
        std::fs::create_dir_all(&workspace_root).expect("create workspace root");

        let jobs_dir = global_root.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
        // Mirrors the shipped pipeline shape: a nested step whose id differs
        // from its target activity name, guarded by a recovery activity.
        std::fs::write(
            jobs_dir.join(format!("{JOB}.yaml")),
            format!(
                r#"schemaVersion: 2
kind: Job
metadata:
  name: {JOB}
spec:
  state: enabled
  kind: workflow
  steps:
    - id: work_bundle
      loop:
        max_iterations: 8
        steps:
          - id: do_one_unit
            recovery_activity: rescue_the_step
            spec:
              type: deterministic
              action: sleep
              config: {{}}
"#
            ),
        )
        .expect("write job yaml");

        let runtime =
            OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
        (root, runtime)
    }

    fn seed_run(runtime: &OrbitRuntime, run_id: &str, state: JobRunState) {
        let now = Utc::now();
        let run = JobRun {
            executed_on: None,
            run_id: run_id.to_string(),
            job_id: JOB.to_string(),
            attempt: 1,
            state,
            scheduled_at: now,
            started_at: Some(now),
            finished_at: state.is_terminal().then_some(now),
            duration_ms: state.is_terminal().then_some(0),
            created_at: now,
            pid: None,
            pid_start_time: None,
            input: None,
            retry_source_run_id: None,
            knowledge_metrics: None,
            resolved_crew: None,
            crew_model: None,
            steps: Vec::new(),
        };
        let workspace_id = runtime.workspace_id().expect("workspace id");
        runtime
            .sqlite_store()
            .expect("store")
            .upsert_job_run_for_workspace(&workspace_id, &run, None)
            .expect("insert run");
    }

    fn seed_invocation(runtime: &OrbitRuntime, run_id: &str, activity_id: &str) {
        runtime
            .insert_invocation_trace_record(&InvocationInsertParams {
                job_run_id: run_id.to_string(),
                activity_id: activity_id.to_string(),
                agent: "agent".to_string(),
                model: Some("model".to_string()),
                task_ids: Vec::new(),
                trace: InvocationTrace::default(),
            })
            .expect("insert invocation");
    }

    #[test]
    fn step_invocations_recorded_under_their_step_id_are_attributed_as_step_work() {
        let (_root, runtime) = runtime_with_job();
        seed_run(&runtime, "jrun-1", JobRunState::Success);
        seed_run(&runtime, "jrun-2", JobRunState::Failed);
        // The executor writes the *step id*, not `sleep` or any target name.
        seed_invocation(&runtime, "jrun-1", "do_one_unit");
        seed_invocation(&runtime, "jrun-2", "do_one_unit");
        seed_invocation(&runtime, "jrun-2", "do_one_unit");
        // A recovery dispatch has no step, so it is written under the
        // recovery activity's own name.
        seed_invocation(&runtime, "jrun-2", "rescue_the_step");

        let window = ReliabilityWindow::ending_at("24h", Utc::now(), Duration::hours(24));
        let result = runtime
            .pipeline_reliability(&window)
            .expect("compute reliability");

        assert_eq!(result.job_runs.overall.total, 2);
        assert_eq!(result.job_runs.overall.failed, 1);
        assert_eq!(result.job_runs.overall.failure_rate().denominator, 2);

        let by_activity = &result.recovery.by_activity;
        let step = by_activity
            .iter()
            .find(|row| row.activity_id == "do_one_unit")
            .expect("step row");
        assert_eq!(
            step.role,
            ActivityRole::Step,
            "a step invocation recorded under its step id must not fall through to Unknown"
        );
        let recovery = by_activity
            .iter()
            .find(|row| row.activity_id == "rescue_the_step")
            .expect("recovery row");
        assert_eq!(recovery.role, ActivityRole::Recovery);

        assert_eq!(
            result.recovery.recovery_activities,
            vec!["rescue_the_step".to_string()],
            "the recovery activity must be discovered from the job catalog"
        );
        assert_eq!(result.recovery.per_step_invocation.numerator, 1);
        assert_eq!(
            result.recovery.per_step_invocation.denominator, 3,
            "the step denominator must count the step-id invocations"
        );
        assert_eq!(result.recovery.per_job_run.numerator, 1);
        assert_eq!(result.recovery.per_job_run.denominator, 2);
    }

    #[test]
    fn a_window_with_since_at_or_after_until_is_rejected() {
        let (_root, runtime) = runtime_with_job();
        let now = Utc::now();
        let window = ReliabilityWindow {
            label: "bad".to_string(),
            since: now,
            until: now,
            bucket: crate::metrics::reliability::BucketGranularity::Hour,
        };
        assert!(runtime.pipeline_reliability(&window).is_err());
    }
}
