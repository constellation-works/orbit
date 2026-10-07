// Integration coverage for the `proc.spawn` timeout ceiling: the fixed
// ceiling outside a managed activity, and the activity's remaining budget
// under the operator's ceiling inside one.

use std::time::{Duration, SystemTime};

use orbit_tools::{ProcSpawnBudget, ToolContext, ToolRegistry};
use serde_json::{Value, json};

const MINUTE_MS: u64 = 60_000;

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    registry
}

fn activity_context(program: &str, budget: Option<ProcSpawnBudget>) -> ToolContext {
    ToolContext {
        proc_allowed_programs: vec![program.to_string()],
        proc_spawn_activity_scoped: true,
        proc_spawn_budget: budget,
        ..Default::default()
    }
}

fn budget(remaining: Duration, max_timeout_ms: u64) -> ProcSpawnBudget {
    ProcSpawnBudget {
        deadline: SystemTime::now() + remaining,
        max_timeout_ms,
    }
}

fn spawn(ctx: &ToolContext, mut input: Value, requested: Option<u64>) -> Value {
    if let Some(requested) = requested {
        input["timeout_ms"] = json!(requested);
    }
    registry()
        .execute("proc.spawn", ctx, input)
        .expect("proc.spawn returns a process result")
}

#[test]
fn timeout_metadata_reports_defaults_explicit_deadlines_and_clamping() {
    // An interactive call and an activity whose host attests no deadline
    // both keep the fixed ceiling.
    for ctx in [ToolContext::default(), activity_context("/bin/echo", None)] {
        for (requested, applied, clamped) in [
            (None, 15_000, false),
            (Some(5_000), 5_000, false),
            (Some(60_000), 60_000, false),
            (Some(60_001), 60_000, true),
            (Some(180_000), 60_000, true),
            (Some(u64::MAX), 60_000, true),
        ] {
            let value = spawn(
                &ctx,
                json!({ "program": "/bin/echo", "args": ["ok"] }),
                requested,
            );
            assert_eq!(value["success"], json!(true), "{value:?}");
            assert_eq!(value["stdout"], json!("ok\n"));
            assert_eq!(value["timeout_ms"], json!(applied));
            assert_eq!(value["timeout_ceiling_ms"], json!(60_000));
            assert_eq!(value["timeout_ceiling_source"], json!("unscoped"));
            assert_eq!(value["timeout_clamped"], json!(clamped));
            assert_eq!(value.get("timeout_notice").is_some(), clamped, "{value:?}");
            assert_eq!(
                value.get("requested_timeout_ms").cloned(),
                requested.map(|ms| json!(ms))
            );
            assert_eq!(value["timed_out"], json!(false));
            assert!(
                value.get("hint").is_none(),
                "completed command needs no timeout hint"
            );
        }
    }
}

#[test]
fn timed_out_command_recommends_native_shell_transport() {
    let ctx = activity_context("/bin/sleep", None);
    let value = spawn(
        &ctx,
        json!({ "program": "/bin/sleep", "args": ["10"] }),
        Some(50),
    );
    assert_eq!(value["success"], json!(false));
    assert_eq!(value["timed_out"], json!(true));
    assert_eq!(value["timeout_ms"], json!(50));
    assert_eq!(value["timeout_clamped"], json!(false));
    assert_eq!(value["hint"]["transport"], json!("native_shell"));
    assert!(
        value["hint"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
}

/// The failure this budget repairs: a managed implementer's cargo build ran
/// past the 60 s ceiling and was killed before any gate finished.
#[cfg(unix)]
#[test]
fn activity_budget_lets_a_command_outlast_the_unscoped_ceiling() {
    let ctx = activity_context(
        "/bin/sleep",
        Some(budget(Duration::from_secs(10 * 60), 45 * MINUTE_MS)),
    );
    let value = spawn(
        &ctx,
        json!({ "program": "/bin/sleep", "args": ["90"] }),
        Some(180_000),
    );
    assert_eq!(value["success"], json!(true), "{value:?}");
    assert_eq!(value["timed_out"], json!(false), "{value:?}");
    assert_eq!(value["timeout_ms"], json!(180_000));
    assert_eq!(value["timeout_clamped"], json!(false));
    assert_eq!(value["timeout_ceiling_source"], json!("activity_remaining"));
    let ceiling = value["timeout_ceiling_ms"].as_u64().expect("ceiling");
    assert!(
        (180_000..=10 * MINUTE_MS).contains(&ceiling),
        "ceiling is the remaining budget: {value:?}"
    );
    assert!(
        value["duration_ms"].as_u64().expect("duration") >= 90_000,
        "{value:?}"
    );
}

#[test]
fn request_above_the_budget_is_clamped_to_the_smaller_bound() {
    // Two minutes left under a 45 minute ceiling: the activity's remaining
    // budget bounds the call.
    let ctx = activity_context(
        "/bin/echo",
        Some(budget(Duration::from_secs(120), 45 * MINUTE_MS)),
    );
    let value = spawn(&ctx, json!({ "program": "/bin/echo" }), Some(180_000));
    let applied = value["timeout_ms"].as_u64().expect("timeout_ms");
    assert!(
        (110_000..=120_000).contains(&applied),
        "remaining budget bounds the call: {value:?}"
    );
    assert_eq!(value["timeout_ceiling_ms"], json!(applied));
    assert_eq!(value["timeout_ceiling_source"], json!("activity_remaining"));
    assert_eq!(value["timeout_clamped"], json!(true));
    assert_eq!(value["requested_timeout_ms"], json!(180_000));
    assert!(value["timeout_notice"].is_string(), "{value:?}");

    // An hour left under a 90 s operator ceiling: the ceiling bounds it.
    let ctx = activity_context(
        "/bin/echo",
        Some(budget(Duration::from_secs(3_600), 90_000)),
    );
    let value = spawn(&ctx, json!({ "program": "/bin/echo" }), Some(180_000));
    assert_eq!(value["timeout_ms"], json!(90_000));
    assert_eq!(value["timeout_ceiling_ms"], json!(90_000));
    assert_eq!(value["timeout_ceiling_source"], json!("configured"));
    assert_eq!(value["timeout_clamped"], json!(true));

    // A call that names no timeout keeps the default inside a budget too.
    let value = spawn(&ctx, json!({ "program": "/bin/echo" }), None);
    assert_eq!(value["timeout_ms"], json!(15_000));
    assert_eq!(value["timeout_clamped"], json!(false));
}
