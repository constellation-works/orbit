//! The shipped workflow, with deterministic reservation and time boundaries.
use super::*;

#[derive(Default)]
struct State {
    elapsed: Duration,
    sleeps: Vec<Duration>,
    attempts: Vec<Duration>,
    dispatched: usize,
    released: usize,
}

struct GateHost {
    state: Arc<Mutex<State>>,
    opens_at: Duration,
    reserve_cost: Duration,
    cancelled: bool,
}

impl orbit_engine::test_support::NullRuntimeHost for GateHost {
    fn run_deterministic(
        &self,
        action: &str,
        _: &Value,
        input: &Value,
        _: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        let mut state = self.state.lock().unwrap();
        Ok(match action {
            "resolve_delivery_job" => json!({"job_name": "task_local_pipeline"}),
            "reserve_locks" => {
                let now = state.elapsed;
                state.attempts.push(now);
                state.elapsed += self.reserve_cost;
                json!({
                    "reserved": now >= self.opens_at,
                    "reservation_id": "fixture-reservation",
                    "footprint_deferred": false,
                    "conflicts": [{"file": "held.rs", "held_by": "task", "held_by_id": "holder"}],
                    "waiting_on_deps": [],
                })
            }
            // Also makes the pre-fix shipped loop deterministic: its sleep
            // activity advances virtual time rather than waiting on the host.
            "sleep" => {
                let delay = Duration::from_secs_f64(input["seconds"].as_f64().unwrap());
                state.sleeps.push(delay);
                state.elapsed += delay;
                json!({"slept_seconds": delay.as_secs_f64()})
            }
            "gate_starvation_defer" => {
                let reservation = input
                    .get("reservation")
                    .cloned()
                    .unwrap_or_else(|| input.clone());
                let expired = input["expired"] == true;
                if expired && reservation["reserved"] == true {
                    state.released += 1;
                }
                let deferred = expired || reservation["reserved"] != true;
                json!({"deferred": deferred, "reservation": {
                    "reservation_id": "fixture-reservation", "footprint_deferred": false
                }})
            }
            "invoke_and_wait" => {
                if input["deferral"]["deferred"] == true {
                    json!({"status": "success", "skipped": true})
                } else {
                    state.dispatched += 1;
                    json!({"status": if self.cancelled {"cancelled"} else {"success"}})
                }
            }
            "release_locks" => {
                assert_eq!(input["reservation_id"], "fixture-reservation");
                state.released += 1;
                json!({})
            }
            "pipeline_success_guard" => {
                if input["result"]["status"] == "cancelled" {
                    return Err(DispatchError::ChildRunCancelled {
                        message: "fixture child cancelled".into(),
                    });
                }
                json!({})
            }
            other => panic!("unexpected action: {other}"),
        })
    }
}

fn shipped_gate() -> orbit_types::workflow::JobV2 {
    let assets = workspace_root().join("crates/orbit-core/assets");
    let mut job = load_job_asset(
        &std::fs::read_to_string(assets.join("jobs/task_gate_pipeline.yaml")).unwrap(),
    )
    .unwrap()
    .spec;
    let mut catalog = V2ActivityCatalog::new();
    for name in [
        "resolve_delivery_job",
        "reserve_locks",
        "sleep",
        "gate_starvation_defer",
        "invoke_and_wait",
        "release_locks",
        "pipeline_success_guard",
    ] {
        let activity = orbit_engine::activity_job::load_activity_asset(
            &std::fs::read_to_string(assets.join(format!("activities/{name}.yaml"))).unwrap(),
        )
        .unwrap();
        catalog.insert(name, activity.spec);
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    job
}

#[test]
fn shipped_gate_obeys_elapsed_budget_and_preserves_cleanup() {
    crate::git_fixture::isolated(
        module_path!(),
        "shipped_gate_obeys_elapsed_budget_and_preserves_cleanup",
        || {
            // The window opens exactly at expiry in deferred cases: a late poll
            // would reserve and dispatch, making the regression observable.
            for (budget, interval, cost, opens, expected_attempts, expected_sleeps, cancelled) in [
                (1, 30, 0, 1, vec![0], vec![1], false),
                (0, 30, 0, 0, vec![], vec![], false),
                (3, 1, 0, 3, vec![0, 1, 2], vec![1, 1, 1], false),
                (3, 2, 0, 3, vec![0, 2], vec![2, 1], false),
                (3, 30, 2, 3, vec![0], vec![1], false),
                (1, 30, 2, 0, vec![0], vec![], false),
                (3, 0, 1, 3, vec![0, 1, 2], vec![], false),
                (3, 1, 0, 1, vec![0, 1], vec![1], false),
                (3, 1, 0, 1, vec![0, 1], vec![1], true),
            ] {
                let state = Arc::new(Mutex::new(State::default()));
                let host = GateHost {
                    state: state.clone(),
                    opens_at: Duration::from_secs(opens),
                    reserve_cost: Duration::from_secs(cost),
                    cancelled,
                };
                let epoch = std::time::Instant::now();
                let clock = state.clone();
                let sleeper = state.clone();
                let audit = tempfile::tempdir().unwrap();
                let (writer, _, _) = build_writer_and_sinks(audit.path(), "gate-deadline");
                let outcome = orbit_engine::test_support::with_loop_clock(
                    move || epoch + clock.lock().unwrap().elapsed,
                    move |delay| {
                        let mut state = sleeper.lock().unwrap();
                        state.sleeps.push(delay);
                        state.elapsed += delay;
                    },
                    || {
                        execute_job_with_resume(&shipped_gate(), json!({"task_ids": ["fixture-task"], "max_wait_seconds": budget, "poll_interval_seconds": interval}), "gate-deadline", writer, &host, None).unwrap()
                    },
                );
                let state = state.lock().unwrap();
                assert_eq!(
                    state.attempts,
                    expected_attempts
                        .into_iter()
                        .map(Duration::from_secs)
                        .collect::<Vec<_>>(),
                    "budget={budget}, interval={interval}, cost={cost}"
                );
                assert_eq!(
                    state.sleeps,
                    expected_sleeps
                        .into_iter()
                        .map(Duration::from_secs)
                        .collect::<Vec<_>>()
                );
                let late_grant = opens < budget && cost >= budget;
                let admitted = opens < budget && !late_grant;
                assert_eq!(state.dispatched, usize::from(admitted));
                assert_eq!(
                    state.released,
                    usize::from(admitted || late_grant),
                    "terminal child and late-grant cleanup, including cancellation"
                );
                assert_eq!(outcome.success, !cancelled);
                assert_eq!(outcome.child_cancelled, cancelled);
                if !admitted {
                    assert_eq!(state.elapsed, Duration::from_secs(budget.max(cost)));
                    assert_eq!(outcome.pipeline["starvation_check"]["deferred"], true);
                }
            }
        },
    );
}
