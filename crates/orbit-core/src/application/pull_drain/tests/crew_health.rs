//! One crew with one unchanged exclusion cause keeps one open friction,
//! across drains [ORB-22362].

use chrono::{Duration, Utc};
use orbit_store::contracts::FrictionListFilter;
use orbit_types::record::FrictionStatus;
use orbit_types::workflow::{CrewExclusion, CrewExclusionSource, CrewPoolExclusion, PipelineState};
use serde_json::json;

use super::super::crew_health::{file_hook, record_pool_crew_health};
use crate::application::test_support::runtime_with_workspace_layout;

const CREW: &str = "test-crew";
const CAUSE: &str =
    "antigravity usage window exhausted (limit 90%) until 2026-11-09T09:26:26+00:00";
const OTHER_CAUSE: &str = "crew is disabled on this host";

fn input() -> serde_json::Value {
    json!({
        "auto_crew_pools": {
            "hard": {
                "crews": [{"name": CREW, "weight": 1}],
                "source": "test"
            }
        }
    })
}

fn exclusion(reason: &str) -> Vec<CrewExclusion> {
    vec![CrewExclusion {
        crew: CREW.to_string(),
        source: CrewExclusionSource::ProviderLimit,
        reason: reason.to_string(),
        until: None,
    }]
}

fn seed_run(jobs: &dyn orbit_store::contracts::JobRunStoreBackend, run_id: &str, reason: &str) {
    let mut state = PipelineState::new(
        run_id.to_string(),
        "workspace_auto_pipeline".to_string(),
        json!({}),
    );
    let ago = Utc::now() - Duration::minutes(3);
    state.pull_drain.crew_pool_exclusions.insert(
        CREW.to_string(),
        CrewPoolExclusion {
            since: ago,
            last_observed_at: ago,
            passes: 1,
            reason: reason.to_string(),
        },
    );
    jobs.write_run_state(run_id, &state)
        .expect("seed run state");
}

fn open_pool_crew_frictions(
    runtime: &crate::OrbitRuntime,
    crew: &str,
) -> Vec<orbit_store::contracts::StoredFrictionRecord> {
    let frictions = crate::runtime::friction::store_for(runtime).expect("friction store");
    frictions
        .list(&FrictionListFilter {
            statuses: vec![FrictionStatus::Open],
            q: Some(format!(
                "Pool crew `{crew}` excluded on every admission pass"
            )),
            ..Default::default()
        })
        .expect("list frictions")
}

#[test]
fn same_crew_same_cause_across_drains_keeps_one_open_friction() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &same_crew_same_cause_across_drains_keeps_one_open_friction,
    )) {
        return;
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let input = input();

    let run_a = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert run a");
    let run_b = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert run b");
    seed_run(jobs, &run_a.run_id, CAUSE);
    seed_run(jobs, &run_b.run_id, CAUSE);

    record_pool_crew_health(&runtime, &run_a.run_id, &input, None, &exclusion(CAUSE));
    let after_first = open_pool_crew_frictions(&runtime, CREW);
    assert_eq!(
        after_first.len(),
        1,
        "ORB-22362: first sustained drain files one open friction"
    );

    record_pool_crew_health(&runtime, &run_b.run_id, &input, None, &exclusion(CAUSE));
    let after_second = open_pool_crew_frictions(&runtime, CREW);
    assert_eq!(
        after_second.len(),
        1,
        "ORB-22362: a second drain with the same crew and unchanged cause must not file a second open friction"
    );

    let warnings_a = runtime
        .read_run_state(&run_a.run_id)
        .expect("read run a")
        .map(|state| state.pull_drain.pool_crew_exclusion_warnings())
        .unwrap_or_default();
    let warnings_b = runtime
        .read_run_state(&run_b.run_id)
        .expect("read run b")
        .map(|state| state.pull_drain.pool_crew_exclusion_warnings())
        .unwrap_or_default();
    assert!(
        warnings_a.iter().any(|warning| warning.contains(CAUSE)),
        "ORB-22362: the per-drain run warning for the first drain is unchanged"
    );
    assert!(
        warnings_b.iter().any(|warning| warning.contains(CAUSE)),
        "ORB-22362: the per-drain run warning for the second drain is unchanged"
    );

    let run_c = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .expect("insert run c");
    seed_run(jobs, &run_c.run_id, OTHER_CAUSE);
    record_pool_crew_health(
        &runtime,
        &run_c.run_id,
        &input,
        None,
        &exclusion(OTHER_CAUSE),
    );
    let after_different_cause = open_pool_crew_frictions(&runtime, CREW);
    assert_eq!(
        after_different_cause.len(),
        2,
        "ORB-22362: a different exclusion cause must still file a new friction"
    );

    let first_id = after_first[0].record.id.clone();
    let frictions = crate::runtime::friction::store_for(&runtime).expect("friction store");
    frictions
        .resolve(&first_id, Utc::now())
        .expect("resolve first friction");
    record_pool_crew_health(&runtime, &run_b.run_id, &input, None, &exclusion(CAUSE));
    let after_resolve = open_pool_crew_frictions(&runtime, CREW);
    assert_eq!(
        after_resolve.len(),
        2,
        "ORB-22362: after the earlier friction was resolved, the same cause must file anew"
    );
    assert!(
        after_resolve
            .iter()
            .any(|row| row.record.body.contains(CAUSE)),
        "ORB-22362: the refiled friction carries the same cause"
    );
}

#[test]
fn concurrent_drains_with_same_crew_and_cause_file_one_friction() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &concurrent_drains_with_same_crew_and_cause_file_one_friction,
    )) {
        return;
    }
    const DRAINS: usize = 2;
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let input = input();
    let run_ids: Vec<String> = (0..DRAINS)
        .map(|_| {
            let run = jobs
                .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
                .expect("insert run");
            seed_run(jobs, &run.run_id, CAUSE);
            run.run_id
        })
        .collect();

    // Hold both drains at the filing step so each reaches it before either
    // has written; the dedupe must then hold under the store's write lock.
    let rendezvous = std::sync::Arc::new(std::sync::Barrier::new(DRAINS));
    file_hook::install({
        let rendezvous = rendezvous.clone();
        move || {
            rendezvous.wait();
        }
    });
    std::thread::scope(|scope| {
        for run_id in &run_ids {
            let (runtime, input) = (&runtime, &input);
            scope.spawn(move || {
                record_pool_crew_health(runtime, run_id, input, None, &exclusion(CAUSE));
            });
        }
    });
    file_hook::clear();

    assert_eq!(
        open_pool_crew_frictions(&runtime, CREW).len(),
        1,
        "ORB-22391: drains racing on the same crew and cause must leave exactly one open friction"
    );
}
