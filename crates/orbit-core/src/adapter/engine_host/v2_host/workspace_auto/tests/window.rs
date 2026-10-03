use chrono::{SecondsFormat, Utc};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;

use super::support::seed_running_drain;

fn drain_window(runtime: &OrbitRuntime, input: Value) -> Value {
    runtime
        .run_deterministic("drain_window", &json!({}), &input, ToolContext::default())
        .expect("drain window")
}

#[test]
fn an_absent_window_is_expired_on_its_first_answer() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    // `break_when` is evaluated after the loop body, so an already-expired
    // window still yields exactly one iteration — today's one-tick behavior.
    let stamped = drain_window(&runtime, json!({}));
    assert_eq!(stamped["expired"], true);
    assert_eq!(stamped["remaining_seconds"], 0.0);

    // The template over an absent `for_seconds` renders an empty string.
    let rendered_absent = drain_window(&runtime, json!({ "for_seconds": "" }));
    assert_eq!(rendered_absent["expired"], true);
}

#[test]
fn a_stamped_window_answers_expiry_against_its_own_deadline() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    let stamped = drain_window(&runtime, json!({ "for_seconds": 600 }));
    assert_eq!(stamped["expired"], false);
    let remaining = stamped["remaining_seconds"]
        .as_f64()
        .expect("remaining seconds");
    assert!(
        (595.0..=600.0).contains(&remaining),
        "expected ~600s remaining, got {remaining}"
    );

    // Re-reading the stamp is a pure function of the deadline the first call
    // returned; nothing durable is written between the two.
    let reread = drain_window(&runtime, json!({ "deadline": stamped["deadline"] }));
    assert_eq!(reread["expired"], false);
    assert_eq!(reread["deadline"], stamped["deadline"]);

    let past =
        (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    assert_eq!(
        drain_window(&runtime, json!({ "deadline": past }))["expired"],
        true
    );
}

#[test]
fn a_stopped_drain_expires_the_window_without_waiting_for_the_deadline() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let drain_run_id = seed_running_drain(&runtime, 5);
    runtime
        .stop_workspace_auto_admissions(crate::application::job::DrainAdmissionsStopRequest {
            actor: "tester",
            source: "unit",
            reason: None,
            claim_token: None,
        })
        .expect("stop drain");

    let stamped = drain_window(
        &runtime,
        json!({ "run_id": drain_run_id, "for_seconds": 600 }),
    );
    assert_eq!(stamped["expired"], true);
    assert_eq!(stamped["expired_reason"], "admissions_stopped");
    assert_eq!(stamped["remaining_seconds"], 0.0);
}

#[test]
fn a_drain_window_rejects_an_unparseable_deadline_or_an_oversize_request() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    for input in [
        json!({ "deadline": "not-a-timestamp" }),
        json!({ "for_seconds": 86_401 }),
        json!({ "for_seconds": -1 }),
    ] {
        assert!(
            runtime
                .run_deterministic("drain_window", &json!({}), &input, ToolContext::default())
                .is_err(),
            "expected {input} to be refused"
        );
    }
}
