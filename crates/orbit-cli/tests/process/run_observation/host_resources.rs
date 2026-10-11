//! Fresh CLI processes exercise the shared Mach baseline and its expiry.
use super::{Fixture, isolated_run_observation};
use serde_json::Value;
use std::time::{Duration, Instant};

fn readiness(fixture: &Fixture) -> Duration {
    let started = Instant::now();
    let output = fixture
        .orbit()
        .args(["run", "readiness", "--json"])
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    assert!(output.status.success(), "readiness failed: {output:?}");
    let payload: Value = serde_json::from_slice(&output.stdout).unwrap();
    let unknown = payload["capacity"]["resource_telemetry_unknown"]
        .as_array()
        .unwrap();
    assert!(
        !unknown
            .iter()
            .any(|value| value.as_str().is_some_and(|value| value.starts_with("cpu"))),
        "fresh process must report real CPU usage: {payload}"
    );
    elapsed
}

#[test]
#[allow(
    clippy::print_stderr,
    reason = "report acceptance timings from the isolated fixture"
)]
fn fresh_readiness_processes_reuse_cpu_ticks_and_reject_expired_history() {
    if !isolated_run_observation(
        "run_observation::host_resources::fresh_readiness_processes_reuse_cpu_ticks_and_reject_expired_history",
    ) {
        return;
    }
    let fixture = Fixture::init();
    let history_path = fixture
        .home
        .join(".orbit/cache/host-resource-pressure.json");
    assert!(
        !history_path.exists(),
        "fixture setup must leave the CPU baseline cold"
    );
    let cold = readiness(&fixture);
    assert!(
        cold >= Duration::from_millis(1100),
        "a missing baseline must measure a real window: {cold:?}"
    );
    let mut warm = (0..5).map(|_| readiness(&fixture)).collect::<Vec<_>>();
    let measurements = warm.clone();
    warm.sort();
    // Report the acceptance measurement, but do not pin wall-clock performance
    // in the full suite: unrelated fixtures share this host's build budget.

    let mut recovery = Vec::new();
    for mode in ["stale", "future", "legacy"] {
        let mut history: Value =
            serde_json::from_slice(&std::fs::read(&history_path).unwrap()).unwrap();
        if mode == "legacy" {
            history.as_object_mut().unwrap().remove("cpu_ticks");
        } else {
            let offset = if mode == "stale" { -30 } else { 30 };
            let at = (chrono::Utc::now() + chrono::Duration::seconds(offset)).to_rfc3339();
            for end in ["previous", "latest"] {
                history["cpu_ticks"][end]["sampled_at"] = Value::String(at.clone());
            }
        }
        std::fs::write(&history_path, serde_json::to_vec(&history).unwrap()).unwrap();
        let elapsed = readiness(&fixture);
        assert!(
            elapsed >= Duration::from_millis(1100),
            "{mode} baseline must take a fresh real window: {elapsed:?}"
        );
        let fresh: Value = serde_json::from_slice(&std::fs::read(&history_path).unwrap()).unwrap();
        assert_eq!(fresh["schema_version"], 1);
        assert!(fresh["cpu_ticks"]["previous"]["ticks"].is_array());
        assert!(fresh["cpu_ticks"]["latest"]["ticks"].is_array());
        recovery.push((mode, elapsed.as_secs_f64()));
    }
    eprintln!(
        "macOS readiness measurements: {}",
        serde_json::json!({"cold_seconds": cold.as_secs_f64(), "warm_seconds": measurements.iter().map(Duration::as_secs_f64).collect::<Vec<_>>(), "p50_seconds": warm[2].as_secs_f64(), "recovery_seconds": recovery})
    );
}
