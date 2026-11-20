//! The standing-drain keeper's crash breaker latches: once `max_failed_resubmits`
//! keeper drains fail within an hour, `drain_breaker_open` stays reported until
//! a person starts a drain, however much time has passed. Checked at the
//! public `orbit.attention` boundary with keeper drains seeded in the store.

use chrono::{DateTime, Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_store::contracts::{DrainIntentRecord, DrainIntentState, JobRunStoreBackend};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{
    JobRunState, JobRunTrigger, PipelineState, WORKSPACE_AUTO_PIPELINE_JOB,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;

const KEEPER: &str = "standing-drain-keeper";

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("home/.orbit");
        let orbit = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&orbit).unwrap();
        std::fs::write(
            orbit.join("config.toml"),
            "[workflow.standing_drain]\nenabled = true\nmax_failed_resubmits = 3\n",
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &orbit).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            _root: root,
            runtime,
            jobs,
        }
    }

    fn intent_on(&self, updated_at: DateTime<Utc>) {
        self.intent(DrainIntentState::On, updated_at);
    }

    fn intent_paused(&self, updated_at: DateTime<Utc>) {
        self.intent(DrainIntentState::Paused, updated_at);
    }

    fn intent(&self, intent: DrainIntentState, updated_at: DateTime<Utc>) {
        self.jobs
            .record_drain_intent(&DrainIntentRecord {
                intent,
                actor: "tester".into(),
                source: "cli".into(),
                reason: None,
                updated_at,
            })
            .unwrap();
    }

    /// A keeper drain that ended `failed` at `finished_at`.
    fn failed_keeper_drain(&self, finished_at: DateTime<Utc>) {
        let run = self
            .jobs
            .insert_job_run(
                WORKSPACE_AUTO_PIPELINE_JOB,
                1,
                Utc::now(),
                Some(json!({})),
                None,
            )
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        let mut state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
        state.trigger = Some(JobRunTrigger::state_routine(KEEPER, KEEPER));
        self.jobs.write_run_state(&run.run_id, &state).unwrap();
        self.jobs
            .finalize_job_run(&run.run_id, JobRunState::Failed, finished_at, Some(1))
            .unwrap();
    }

    fn breaker_items(&self) -> Vec<Value> {
        let session = ToolSessionContext {
            effective_capabilities: [McpCapability::Operator].into(),
            ..Default::default()
        };
        let response = self
            .runtime
            .execute_tool_command_dispatch_with_session_context(
                "orbit.attention",
                json!({"kind": "drain_breaker_open"}),
                None,
                None,
                ToolEntryPoint::Mcp,
                session,
            )
            .unwrap()
            .value;
        response["items"].as_array().cloned().unwrap_or_default()
    }
}

fn three_failures_in_an_hour_stay_open_after_the_hour_passes() {
    let fixture = Fixture::new();
    let now = Utc::now();
    fixture.intent_on(now - Duration::hours(8));
    // Three failures 11 minutes apart, the newest 3 hours ago: the window a
    // rolling count would have closed long since.
    for minutes_ago in [200, 189, 178] {
        fixture.failed_keeper_drain(now - Duration::minutes(minutes_ago));
    }
    let items = fixture.breaker_items();
    assert_eq!(
        items.len(),
        1,
        "breaker must stay open past the hour: {items:?}"
    );
    assert_eq!(items[0]["detail"]["failed_in_last_hour"], 3, "{items:?}");
    let view = fixture.runtime.standing_drain_keeper_view().unwrap();
    assert!(
        view.breaker.unwrap().open(),
        "readiness reads the same latch"
    );
}

fn a_hand_start_closes_the_latched_breaker() {
    let fixture = Fixture::new();
    let now = Utc::now();
    fixture.intent_on(now - Duration::hours(8));
    for minutes_ago in [200, 189, 178] {
        fixture.failed_keeper_drain(now - Duration::minutes(minutes_ago));
    }
    assert_eq!(fixture.breaker_items().len(), 1);
    // A person starts a drain: the intent is re-recorded after the failed
    // drains were created.
    fixture.intent_on(Utc::now() + Duration::seconds(1));
    assert!(fixture.breaker_items().is_empty());
    assert!(
        !fixture
            .runtime
            .standing_drain_keeper_view()
            .unwrap()
            .breaker
            .unwrap()
            .open()
    );
}

fn a_pause_after_a_hand_start_does_not_reopen_the_closed_breaker() {
    let fixture = Fixture::new();
    let now = Utc::now();
    fixture.intent_on(now - Duration::hours(8));
    for minutes_ago in [200, 189, 178] {
        fixture.failed_keeper_drain(now - Duration::minutes(minutes_ago));
    }
    assert_eq!(fixture.breaker_items().len(), 1);
    // A hand start closes the breaker, then a hand pause overwrites the
    // intent record: the failures before the start must stay closed.
    fixture.intent_on(Utc::now() + Duration::seconds(1));
    fixture.intent_paused(Utc::now() + Duration::seconds(2));
    let view = fixture.runtime.standing_drain_keeper_view().unwrap();
    let breaker = view.breaker.unwrap();
    assert!(
        !breaker.open(),
        "a paused intent must not count failures a hand start closed: {breaker:?}"
    );
}

fn failures_spread_over_more_than_an_hour_do_not_trip_the_breaker() {
    let fixture = Fixture::new();
    let now = Utc::now();
    fixture.intent_on(now - Duration::hours(8));
    for minutes_ago in [300, 200, 100] {
        fixture.failed_keeper_drain(now - Duration::minutes(minutes_ago));
    }
    assert!(fixture.breaker_items().is_empty());
}

crate::scenario_table!(
    standing_drain_keeper,
    three_failures_in_an_hour_stay_open_after_the_hour_passes,
    a_hand_start_closes_the_latched_breaker,
    a_pause_after_a_hand_start_does_not_reopen_the_closed_breaker,
    failures_spread_over_more_than_an_hour_do_not_trip_the_breaker,
);
