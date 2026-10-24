//! [ORB-17773] A pull follower reports per-crew weekly usage pressure
//! from its own readings in its pull capability, and its claimed leaf
//! carries the task's automatic crew source and complexity for the
//! follower-side re-draw. Under `static` the follower reports no
//! pressure and pull admission is unchanged.

use chrono::Duration;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};

use super::*;

fn crews_config(extra: &str) -> String {
    format!(
        "[workflow]\ndefault_crew = \"sol\"\n{extra}\n\n\
         [crews.sol]\nprovider = \"codex\"\nmodel = \"gpt-sol\"\n\n\
         [crews.opus]\nprovider = \"claude\"\nmodel = \"claude-opus\"\n"
    )
}

/// A weekly reading: `used_percent` used, resetting in `resets_in`.
fn week(provider: &str, used_percent: f64, resets_in: Duration) -> ProviderLimitObservation {
    ProviderLimitObservation {
        provider: provider.into(),
        account: None,
        model: None,
        window: Some("seven_day".into()),
        exhausted: false,
        source: ProviderLimitSource::Event,
        resets_at: Some(Utc::now() + resets_in),
        used_percent: Some(used_percent),
        window_minutes: Some(10080),
        gating: true,
        partial: false,
        observed_at: Utc::now(),
        run_id: None,
        crew: None,
        detail: String::new(),
    }
}

fn reported(pull: &Value, crew: &str) -> Option<u64> {
    pull["crews"]["usage_pressure"]
        .as_array()?
        .iter()
        .find(|entry| entry["crew"] == crew)
        .and_then(|entry| entry["pressure_milli"].as_u64())
}

/// Claude's week is 10% used with six hours left on the follower
/// (pressure about 25); the owner's Claude reading is the opposite.
fn a_follower_reports_its_own_pressure_and_the_leaf_carries_its_source() {
    let follower = crews_config("crew_selection = \"usage_pressure\"");
    let pair = Pair::with_configs(&crews_config(""), &follower, &[Some("sol")]);
    pair.wire
        .owner
        .record_provider_limit(&week("claude", 95.0, Duration::days(6)))
        .unwrap();
    pair.follower
        .record_provider_limit(&week("claude", 10.0, Duration::hours(6)))
        .unwrap();
    let drain = pair.run_drain();
    pair.pass(&drain);

    let pulls = pair.wire.calls("orbit.task.pull");
    let pull = pulls.last().expect("a pull request");
    let opus = reported(pull, "opus").expect("opus pressure reported");
    assert!(opus > 20_000, "the follower's own reading: {pull:#}");
    assert_eq!(
        reported(pull, "sol"),
        Some(1000),
        "no Codex reading is neutral 1.0: {pull:#}"
    );

    let leaf = pair.leaf_runs().into_iter().next().expect("a claimed leaf");
    let run = pair
        .follower_jobs
        .get_job_run(&leaf)
        .unwrap()
        .expect("leaf run");
    let claimed = &run.input.expect("leaf input")["claimed_task"];
    assert_eq!(claimed["crew"], "sol", "{claimed}");
    assert_eq!(claimed["crew_source"], "explicit", "{claimed}");
    assert_eq!(claimed["complexity"], "low", "{claimed}");
}

/// Under `static`, the same readings report nothing: the pull request
/// carries no pressure field, so the owner reads neutral 1.0 and
/// admission proceeds as before.
fn a_static_follower_reports_no_pressure() {
    let follower = crews_config("crew_selection = \"static\"");
    let pair = Pair::with_configs(&crews_config(""), &follower, &[Some("sol")]);
    pair.follower
        .record_provider_limit(&week("claude", 10.0, Duration::hours(6)))
        .unwrap();
    let drain = pair.run_drain();
    pair.pass(&drain);

    let pulls = pair.wire.calls("orbit.task.pull");
    let pull = pulls.last().expect("a pull request");
    assert!(
        pull["crews"].get("usage_pressure").is_none(),
        "static reports no pressure: {pull:#}"
    );
    assert_eq!(pair.owner_claims().len(), 1, "admission is unchanged");
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    follower_usage_pressure_scenarios,
    a_follower_reports_its_own_pressure_and_the_leaf_carries_its_source,
    a_static_follower_reports_no_pressure
);
