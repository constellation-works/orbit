//! A job's failure streak: its newest finished runs that failed or were
//! interrupted, up to the first success. Operator cancels and designed holds
//! are neither failures nor successes, so they neither count nor break it.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_core::OrbitRuntime;
use orbit_store::contracts::{JobRunStepParams, JobRunStoreBackend};
use orbit_types::workflow::{JobRunState, JobTargetType};
use serde_json::json;
use tempfile::TempDir;

const JOB: &str = "streak_fixture";

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
    next_minute: i64,
}

impl Fixture {
    fn new(config: &str) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(global.join("config.toml"), config).unwrap();
        write_job(&jobs_dir);
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            _root: root,
            runtime,
            jobs,
            next_minute: 0,
        }
    }

    fn at(&mut self) -> DateTime<Utc> {
        self.next_minute += 1;
        Utc.with_ymd_and_hms(2026, 10, 30, 0, 0, 0).unwrap() + Duration::minutes(self.next_minute)
    }

    /// One finished run of the job, newer than every run added before it.
    /// A failed run records `error` as its failing step's message. Returns the
    /// run id and creation time.
    fn run(&mut self, state: JobRunState, error: Option<&str>) -> (String, DateTime<Utc>) {
        let at = self.at();
        let run = self
            .jobs
            .insert_job_run(JOB, 1, at, Some(json!({})), None)
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, at, std::process::id())
            .unwrap();
        if let Some(error) = error {
            self.jobs
                .complete_job_run_step(
                    &run.run_id,
                    &JobRunStepParams {
                        lock_wait_ms: None,
                        step_index: 0,
                        target_type: JobTargetType::Activity,
                        target_id: "review".to_string(),
                        started_at: at,
                        finished_at: at,
                        duration_ms: Some(1),
                        exit_code: Some(1),
                        agent_response_json: None,
                        state: JobRunState::Failed,
                        error_code: None,
                        error_message: Some(error.to_string()),
                    },
                )
                .unwrap();
        }
        self.jobs
            .finalize_job_run(&run.run_id, state, at, Some(1))
            .unwrap();
        let created_at = self
            .jobs
            .get_job_run(&run.run_id)
            .unwrap()
            .unwrap()
            .created_at;
        (run.run_id.to_string(), created_at)
    }

    fn streak(&self) -> Option<orbit_core::application::job::JobFailureStreak> {
        self.runtime
            .job_failure_streaks(&[JOB.to_string()])
            .unwrap()
            .into_iter()
            .next()
    }
}

fn write_job(jobs_dir: &Path) {
    std::fs::write(
        jobs_dir.join(format!("{JOB}.yaml")),
        serde_json::to_string(&json!({
            "schemaVersion": 2, "kind": "Job",
            "metadata": {"name": JOB},
            "spec": {"state": "enabled", "steps": [{
                "id": "noop",
                "spec": {"type": "deterministic", "action": "pipeline_success_guard", "config": {}},
                "default_input": {"context": "fixture", "results": []},
            }]}
        }))
        .unwrap(),
    )
    .unwrap();
}

fn a_success_ends_the_streak_and_the_newest_failures_are_counted_scenario() {
    let mut fixture = Fixture::new("");
    fixture.run(JobRunState::Failed, Some("older failure"));
    fixture.run(JobRunState::Failed, Some("older failure"));
    fixture.run(JobRunState::Success, None);
    let (_, since) = fixture.run(JobRunState::Failed, Some("first of three"));
    fixture.run(JobRunState::Interrupted, None);
    let (latest, _) = fixture.run(JobRunState::Failed, Some("newest"));

    let streak = fixture.streak().expect("the newest run failed");
    assert_eq!(streak.count, 3, "{streak:?}");
    assert_eq!(streak.since, since);
    assert_eq!(streak.latest_run_id, latest);
    assert_eq!(streak.latest_error.as_deref(), Some("review: newest"));
    assert!(!streak.truncated);
}

fn a_job_whose_newest_run_succeeded_has_no_streak_scenario() {
    let mut fixture = Fixture::new("");
    fixture.run(JobRunState::Failed, Some("failure"));
    fixture.run(JobRunState::Success, None);
    assert_eq!(fixture.streak(), None);
}

fn cancels_and_holds_neither_count_nor_break_the_streak_scenario() {
    let mut fixture = Fixture::new("");
    fixture.run(JobRunState::Success, None);
    let (_, since) = fixture.run(JobRunState::Failed, Some("failure"));
    fixture.run(JobRunState::Cancelled, None);
    fixture.run(JobRunState::Failed, Some("failure"));
    fixture.run(JobRunState::Held, None);
    let (latest, _) = fixture.run(JobRunState::Failed, Some("failure"));
    fixture.run(JobRunState::Cancelled, None);

    let streak = fixture
        .streak()
        .expect("three failures sit around a cancel and a hold");
    assert_eq!(streak.count, 3, "{streak:?}");
    assert_eq!(streak.since, since);
    assert_eq!(
        streak.latest_run_id, latest,
        "a cancel is never the latest failed run"
    );
}

fn runs_agree_on_one_error_signature_despite_ids_and_counts_scenario() {
    let mut fixture = Fixture::new("");
    fixture.run(
        JobRunState::Failed,
        Some("[provider_capacity] retry 3 for jrun-20261030-0101-a1"),
    );
    fixture.run(
        JobRunState::Failed,
        Some("[provider_capacity] retry 9 for jrun-20261031-1902-b2"),
    );
    let streak = fixture.streak().unwrap();
    assert!(streak.same_error, "{streak:?}");

    fixture.run(JobRunState::Failed, Some("sandbox refused\nsecond line"));
    let streak = fixture.streak().unwrap();
    assert!(!streak.same_error, "{streak:?}");
    assert_eq!(
        streak.latest_error.as_deref(),
        Some("review: sandbox refused")
    );
}

fn the_warn_threshold_defaults_to_five_and_zero_turns_it_off_scenario() {
    assert_eq!(Fixture::new("").runtime.job_failure_streak_warn(), 5);
    let configured = Fixture::new("[jobs]\nfailure_streak_warn = 8\n");
    assert_eq!(configured.runtime.job_failure_streak_warn(), 8);
    let off = Fixture::new("[jobs]\nfailure_streak_warn = 0\n");
    assert_eq!(off.runtime.job_failure_streak_warn(), 0);
}

crate::scenario_table!(
    a_success_ends_the_streak_and_the_newest_failures_are_counted,
    a_success_ends_the_streak_and_the_newest_failures_are_counted_scenario
);
crate::scenario_table!(
    a_job_whose_newest_run_succeeded_has_no_streak,
    a_job_whose_newest_run_succeeded_has_no_streak_scenario
);
crate::scenario_table!(
    cancels_and_holds_neither_count_nor_break_the_streak,
    cancels_and_holds_neither_count_nor_break_the_streak_scenario
);
crate::scenario_table!(
    runs_agree_on_one_error_signature_despite_ids_and_counts,
    runs_agree_on_one_error_signature_despite_ids_and_counts_scenario
);
crate::scenario_table!(
    the_warn_threshold_defaults_to_five_and_zero_turns_it_off,
    the_warn_threshold_defaults_to_five_and_zero_turns_it_off_scenario
);
