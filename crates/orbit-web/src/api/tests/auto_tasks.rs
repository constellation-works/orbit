//! Tests for the dashboard auto-task JSON API [ORB-10876].

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Duration, Timelike, Utc};
use orbit_common::governance::authorization::OPERATOR_OVERRIDE_ENV;
use orbit_core::application::auto_tasks::schedule::decide_due;
use orbit_core::application::auto_tasks::{AutoTaskDueDecision, cursor_state_path};
use orbit_core::application::task::TaskUpdateParams;
use orbit_core::{AutoTaskAddParams, OrbitRuntime};
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::automation::{CoverageClass, DeliveryTrigger};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use serde_json::Value;
use tower::ServiceExt;

use super::super::router;
use super::test_support::body_json;
use crate::state::DashboardState;

fn runtime() -> OrbitRuntime {
    OrbitRuntime::in_memory().expect("build runtime")
}

fn chore_params(name: &str) -> AutoTaskAddParams {
    AutoTaskAddParams {
        name: name.to_string(),
        description: format!("Definition {name}"),
        schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
        template: AutoTaskTemplate {
            title: format!("Chore {name}"),
            description: "Recurring chore body.".to_string(),
            acceptance_criteria: vec!["The chore is observable.".to_string()],
            task_type: TaskType::Chore,
            tags: vec![],
            required_tools: Vec::new(),
            priority: TaskPriority::Medium,
            complexity: Some(TaskComplexity::Medium),
            crew: None,
            status: TaskStatus::Backlog,
        },
        dedupe: DedupePolicy::SkipIfOpen,
    }
}

fn delivery_params(name: &str, coverage: CoverageClass) -> AutoTaskAddParams {
    AutoTaskAddParams {
        name: name.to_string(),
        description: format!("Delivery definition {name}"),
        schedule: AutoTaskSchedule::Deliveries {
            deliveries_landed: DeliveryTrigger {
                owner_machine: None,
                branch: "agent-main".to_string(),
                threshold: 3,
                max_wait_minutes: 60,
                coverage,
                max_items: 20,
                retries: 0,
            },
        },
        template: AutoTaskTemplate {
            title: format!("Delivery {name}"),
            description: "Recurring delivery coverage.".to_string(),
            acceptance_criteria: vec!["Delivery coverage is observable.".to_string()],
            task_type: TaskType::Chore,
            tags: vec![],
            required_tools: Vec::new(),
            priority: TaskPriority::Medium,
            complexity: Some(TaskComplexity::Hard),
            crew: None,
            status: TaskStatus::Backlog,
        },
        dedupe: DedupePolicy::SkipIfOpen,
    }
}

fn state(runtime: OrbitRuntime) -> (DashboardState, Arc<OrbitRuntime>) {
    let runtime = Arc::new(runtime);
    (DashboardState::single(runtime.clone()), runtime)
}

async fn send(
    state: DashboardState,
    method: Method,
    uri: &str,
    body: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header("host", "localhost:7878");
    if !matches!(method, Method::GET) {
        builder = builder
            .header("origin", "http://localhost:7878")
            .header("content-type", "application/json");
    }
    router()
        .with_state(state)
        .oneshot(
            builder
                .body(Body::from(body.unwrap_or("").to_string()))
                .expect("request"),
        )
        .await
        .expect("response")
}

/// Pin the process signals `CallerCapabilities::resolve` reads, for the whole
/// request rather than just its construction.
///
/// The guard in `orbit_common::test_env` is process-wide, so it serializes two
/// tests only when *both* take it. Every case whose expected status depends on
/// caller identity therefore goes through this helper — a case that merely
/// reads `ORBIT_OPERATOR` without holding the guard can observe the override a
/// sibling set concurrently and turn an expected 403 into a 200 [ORB-10894].
#[allow(clippy::await_holding_lock)]
async fn with_caller_env<'a, T>(
    vars: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let _env = orbit_common::test_env::scoped(vars);
    fut.await
}

/// Resolve as an explicit operator. The override outranks every other signal,
/// so nothing else needs pinning to reach `Operator`.
async fn as_operator<T>(fut: impl std::future::Future<Output = T>) -> T {
    with_caller_env([(OPERATOR_OVERRIDE_ENV, Some("1"))], fut).await
}

/// Resolve as an agent: override cleared, agent envelope declared.
///
/// The envelope is *set* rather than cleared on purpose. With nothing declared,
/// resolution falls through to its interactive-terminal probe, and `cargo test`
/// run from a terminal inherits both handles — the caller would resolve to
/// `Operator` and the denial assertion would fail for a reason that has nothing
/// to do with the code under test. Declaring the agent stops resolution one
/// rule earlier, so the expected 403 holds in CI, in a managed run, and in an
/// interactive shell alike. The unidentified-caller branch is covered without
/// process state in `orbit_common::governance::tests::authorization`.
async fn as_agent<T>(fut: impl std::future::Future<Output = T>) -> T {
    with_caller_env(
        [
            (OPERATOR_OVERRIDE_ENV, None),
            ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
            ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
        ],
        fut,
    )
    .await
}

#[tokio::test]
async fn list_requires_a_concrete_workspace_and_stays_read_only_without_one() {
    let (state, _) = state(runtime());
    let response = send(state, Method::GET, "/auto-tasks", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["definitions"], serde_json::json!([]));
    assert_eq!(json["controls_authorized"], false);
    assert!(
        json["read_only_reason"]
            .as_str()
            .expect("reason")
            .contains("All-workspace mode is read-only"),
        "{json}"
    );
}

#[tokio::test]
async fn list_reports_enabled_and_disabled_definitions() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("nightly")).expect("add");
    runtime.auto_task_toggle("nightly", false).expect("disable");
    let mut hourly_params = chore_params("hourly");
    hourly_params.template.required_tools = vec![
        "github.run.list".to_string(),
        "github.auth.status".to_string(),
        "github.run.list".to_string(),
    ];
    runtime.auto_task_add(hourly_params).expect("add");
    let path = cursor_state_path(&runtime.paths().state_dir);
    std::fs::create_dir_all(path.parent().expect("state dir")).expect("mkdir");
    std::fs::write(
        path,
        r#"{
  "definitions": {
    "hourly": {
      "baseline_at": "2026-01-01T00:00:00+00:00",
      "last_slot": "2026-01-01T01:00:00+00:00",
      "last_fired_at": "2026-01-01T01:00:05+00:00",
      "last_task_id": "ORB-00001"
    }
  }
}"#,
    )
    .expect("write cursor");

    let (state, _) = state(runtime);
    let response = send(state, Method::GET, "/auto-tasks?workspace=default", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json["definitions"].as_array().expect("definitions");
    assert_eq!(items.len(), 2, "{json}");
    let nightly = items
        .iter()
        .find(|item| item["name"] == "nightly")
        .expect("nightly");
    let hourly = items
        .iter()
        .find(|item| item["name"] == "hourly")
        .expect("hourly");
    assert_eq!(nightly["enabled"], false);
    assert_eq!(hourly["enabled"], true);
    assert_eq!(hourly["dedupe"], "skip_if_open");
    assert_eq!(hourly["template"]["complexity"], "medium");
    assert_eq!(
        hourly["template"]["required_tools"],
        serde_json::json!(["github.auth.status", "github.run.list"])
    );
    assert!(
        hourly["template_summary"]
            .as_str()
            .expect("summary")
            .contains("[auto-task] Chore hourly"),
        "{hourly}"
    );
    assert!(
        hourly["template_summary"]
            .as_str()
            .expect("summary")
            .contains("complexity medium"),
        "{hourly}"
    );
    assert_eq!(hourly["last_evaluation"]["kind"], "fired");
    assert_eq!(hourly["last_evaluation"]["last_task_id"], "ORB-00001");
    assert_eq!(hourly["last_minted_task_id"], "ORB-00001");
    assert_eq!(hourly["next_evaluation"]["state"], "scheduled");
    assert_eq!(hourly["next_evaluation"]["hypothetical"], false);
    assert!(
        hourly["next_evaluation"]["at"]
            .as_str()
            .is_some_and(|at| chrono::DateTime::parse_from_rfc3339(at).is_ok()),
        "{hourly}"
    );
    assert_eq!(hourly["schedule_summary"], "every 60 minutes");
    assert_eq!(nightly["last_evaluation"], serde_json::Value::Null);
    assert_eq!(nightly["next_evaluation"]["state"], "disabled");
    assert_eq!(nightly["next_evaluation"]["hypothetical"], true);
    assert!(nightly["next_evaluation"]["at"].is_null(), "{nightly}");
    assert!(json["cursor_state_error"].is_null(), "{json}");
}

#[tokio::test]
async fn list_surfaces_malformed_cursor_state_instead_of_baselining() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("hourly")).expect("add");
    let path = cursor_state_path(&runtime.paths().state_dir);
    std::fs::create_dir_all(path.parent().expect("state dir")).expect("mkdir");
    std::fs::write(&path, "{not json").expect("corrupt cursor");

    let (dashboard, runtime) = state(runtime);
    let response = send(
        dashboard,
        Method::GET,
        "/auto-tasks?workspace=default",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(
        json["cursor_state_error"]
            .as_str()
            .is_some_and(|error| error.contains("malformed auto-task cursor state")),
        "{json}"
    );
    let hourly = json["definitions"]
        .as_array()
        .expect("definitions")
        .iter()
        .find(|item| item["name"] == "hourly")
        .expect("hourly");
    assert_eq!(hourly["last_evaluation"], serde_json::Value::Null);
    assert_eq!(hourly["next_evaluation"]["state"], "unavailable");
    assert_eq!(
        std::fs::read_to_string(&path).expect("raw"),
        "{not json",
        "list must not rewrite corrupt cursor evidence"
    );
    drop(runtime);
}

fn write_cursor(runtime: &OrbitRuntime, name: &str, last_task_id: &str) {
    let path = cursor_state_path(&runtime.paths().state_dir);
    std::fs::create_dir_all(path.parent().expect("state dir")).expect("mkdir");
    let existing =
        std::fs::read_to_string(&path).unwrap_or_else(|_| r#"{"definitions":{}}"#.to_string());
    let mut doc: serde_json::Value = serde_json::from_str(&existing).expect("parse cursor");
    doc["definitions"][name] = serde_json::json!({
        "baseline_at": "2026-01-01T00:00:00+00:00",
        "last_slot": "2026-01-01T01:00:00+00:00",
        "last_fired_at": "2026-01-01T01:00:05+00:00",
        "last_task_id": last_task_id
    });
    std::fs::write(
        path,
        serde_json::to_string_pretty(&doc).expect("serialize cursor"),
    )
    .expect("write cursor");
}

#[tokio::test]
async fn list_labels_disabled_next_evaluation_as_hypothetical() {
    let runtime = runtime();
    runtime
        .auto_task_add(chore_params("paused-chore"))
        .expect("add");
    runtime
        .auto_task_toggle("paused-chore", false)
        .expect("disable");
    write_cursor(&runtime, "paused-chore", "ORB-00001");

    let (state, _) = state(runtime);
    let response = send(state, Method::GET, "/auto-tasks?workspace=default", None).await;
    let json = body_json(response).await;
    let item = json["definitions"]
        .as_array()
        .expect("definitions")
        .iter()
        .find(|item| item["name"] == "paused-chore")
        .expect("paused-chore");
    assert_eq!(item["enabled"], false);
    assert_eq!(item["next_evaluation"]["state"], "disabled");
    assert_eq!(item["next_evaluation"]["hypothetical"], true);
    assert!(item["next_evaluation"]["at"].as_str().is_some(), "{item}");
}

const STALE_BASELINE: &str = "2020-01-01T00:00:00+00:00";
const STALE_LAST_SLOT: &str = "2020-01-01T01:00:00+00:00";

/// A cursor whose baseline and last consumed slot are both far in the past, so
/// the scheduler still owes a catch-up fire the moment it next runs.
fn write_stale_cursor(runtime: &OrbitRuntime, name: &str) {
    let path = cursor_state_path(&runtime.paths().state_dir);
    std::fs::create_dir_all(path.parent().expect("state dir")).expect("mkdir");
    let doc = serde_json::json!({"definitions": {name: {
        "baseline_at": STALE_BASELINE,
        "last_slot": STALE_LAST_SLOT,
        "last_fired_at": "2020-01-01T01:00:05+00:00",
        "last_task_id": "ORB-00001"
    }}});
    std::fs::write(
        path,
        serde_json::to_string_pretty(&doc).expect("serialize cursor"),
    )
    .expect("write cursor");
}

fn utc(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339)
        .expect("rfc3339 timestamp")
        .with_timezone(&Utc)
}

/// Sends the list request and returns its only row with the clock readings
/// taken immediately before and after it. The handler observes "now" somewhere
/// inside that bracket, so projection checks accept every answer that is
/// correct for some instant in it instead of comparing against a later clock
/// read that may already sit past a schedule boundary.
async fn list_single_row_bracketed(state: DashboardState) -> (Value, DateTime<Utc>, DateTime<Utc>) {
    let before = Utc::now();
    let response = send(state, Method::GET, "/auto-tasks?workspace=default", None).await;
    let after = Utc::now();
    let json = body_json(response).await;
    let item = json["definitions"].as_array().expect("definitions")[0].clone();
    (item, before, after)
}

fn projected_at(item: &Value) -> DateTime<Utc> {
    utc(item["next_evaluation"]["at"]
        .as_str()
        .expect("projected slot"))
}

/// Checks an interval projection for the stale cursor against the owed
/// catch-up slot at each end of the request bracket.
///
/// The projection must be exactly one period past the slot the scheduler owed
/// at the handler's observation, which lies between the slots owed at
/// `before` and `after` (they differ only when a boundary fell inside the
/// bracket). The owed slot itself, or any later arrival, is rejected.
fn check_interval_projection(
    schedule: &AutoTaskSchedule,
    projected: DateTime<Utc>,
    before: DateTime<Utc>,
    after: DateTime<Utc>,
) -> Result<(), String> {
    let AutoTaskSchedule::Interval { every_minutes } = schedule else {
        return Err(format!("expected an interval schedule, got {schedule:?}"));
    };
    let period = Duration::minutes(i64::try_from(*every_minutes).expect("interval minutes"));
    let baseline = utc(STALE_BASELINE);
    let owed_at = |now: DateTime<Utc>| -> Result<DateTime<Utc>, String> {
        match decide_due(schedule, baseline, Some(utc(STALE_LAST_SLOT)), now)
            .map_err(|err| err.to_string())?
        {
            AutoTaskDueDecision::Fire { slot } => Ok(utc(&slot)),
            AutoTaskDueDecision::NotDue => Err(format!(
                "a years-old cursor must still owe a catch-up fire at {now}"
            )),
        }
    };
    let (earliest_owed, latest_owed) = (owed_at(before)?, owed_at(after)?);
    if earliest_owed > before {
        return Err(format!(
            "owed catch-up slot {earliest_owed} is not in the past"
        ));
    }

    // Both answers come from the same anchored arithmetic: consecutive period
    // boundaries off the recorded baseline.
    let preceding = projected - period;
    let aligned = (preceding - baseline).num_milliseconds() % period.num_milliseconds() == 0;
    if !aligned || preceding < earliest_owed || preceding > latest_owed {
        return Err(format!(
            "projection {projected} is not the arrival after the owed slot \
             ({earliest_owed}..={latest_owed}) observed in {before}..={after}"
        ));
    }
    Ok(())
}

/// Checks a `* * * * *` projection against the request bracket: pinned to a
/// whole minute, strictly ahead of every instant the handler could have
/// observed, and no later than the minute after the last of them.
fn check_minutely_cron_projection(
    projected: DateTime<Utc>,
    before: DateTime<Utc>,
    after: DateTime<Utc>,
) -> Result<(), String> {
    // The canonical cron projection pins every occurrence to its minute, so the
    // rendered slot never carries the poll's sub-minute component.
    if projected.second() != 0 || projected.nanosecond() != 0 {
        return Err(format!(
            "projection {projected} is not pinned to the minute"
        ));
    }
    if projected <= before || projected > after + Duration::minutes(1) {
        return Err(format!(
            "projection {projected} is not the next minute for an observation in {before}..={after}"
        ));
    }
    Ok(())
}

#[tokio::test]
async fn list_projects_the_next_interval_slot_not_the_owed_catch_up_slot() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("stale")).expect("add");
    write_stale_cursor(&runtime, "stale");

    let (state, runtime) = state(runtime);
    let (item, before, after) = list_single_row_bracketed(state).await;
    let schedule = runtime
        .auto_task_show("stale")
        .expect("show")
        .expect("definition")
        .schedule;

    assert_eq!(item["next_evaluation"]["state"], "scheduled");
    // The scheduler still owes a fire for a boundary years in the past. The two
    // answers are different by design, so the row must project the schedule's
    // next arrival rather than the owed slot.
    let checked = check_interval_projection(&schedule, projected_at(&item), before, after);
    assert_eq!(checked, Ok(()), "{item}");
}

#[test]
fn interval_projection_check_tolerates_an_hour_boundary_inside_the_request() {
    let schedule = chore_params("stale").schedule;
    let before = utc("2026-09-27T12:59:59.999+00:00");
    let after = utc("2026-09-27T13:00:00.001+00:00");

    // Observed just before 13:00 the next arrival is 13:00; just after, 14:00.
    for projected in ["2026-09-27T13:00:00+00:00", "2026-09-27T14:00:00+00:00"] {
        assert_eq!(
            check_interval_projection(&schedule, utc(projected), before, after),
            Ok(()),
            "{projected}"
        );
    }
}

#[test]
fn interval_projection_check_rejects_the_owed_slot_and_misaligned_arrivals() {
    let schedule = chore_params("stale").schedule;
    let observed = utc("2026-09-27T12:30:00+00:00");
    assert_eq!(
        check_interval_projection(
            &schedule,
            utc("2026-09-27T13:00:00+00:00"),
            observed,
            observed
        ),
        Ok(())
    );

    for wrong in [
        // The owed catch-up slot.
        "2026-09-27T12:00:00+00:00",
        // Off the baseline's hourly boundaries.
        "2026-09-27T13:00:30+00:00",
        // One arrival too far.
        "2026-09-27T14:00:00+00:00",
    ] {
        assert!(
            check_interval_projection(&schedule, utc(wrong), observed, observed).is_err(),
            "{wrong} must be rejected"
        );
    }
}

#[tokio::test]
async fn list_projects_cron_definitions_pinned_to_the_minute() {
    let runtime = runtime();
    let mut params = chore_params("minutely");
    params.schedule = AutoTaskSchedule::Cron {
        cron: "* * * * *".to_string(),
    };
    runtime.auto_task_add(params).expect("add");
    write_cursor(&runtime, "minutely", "ORB-00001");

    let (state, _) = state(runtime);
    let (item, before, after) = list_single_row_bracketed(state).await;

    assert_eq!(item["next_evaluation"]["state"], "scheduled");
    let checked = check_minutely_cron_projection(projected_at(&item), before, after);
    assert_eq!(checked, Ok(()), "{item}");
}

#[test]
fn cron_projection_check_tolerates_a_minute_boundary_inside_the_request() {
    let before = utc("2026-09-27T12:34:59.999+00:00");
    let after = utc("2026-09-27T12:35:00.001+00:00");

    // Observed just before 12:35 the next minute is 12:35; at or after, 12:36.
    for projected in ["2026-09-27T12:35:00+00:00", "2026-09-27T12:36:00+00:00"] {
        assert_eq!(
            check_minutely_cron_projection(utc(projected), before, after),
            Ok(()),
            "{projected}"
        );
    }
}

#[test]
fn cron_projection_check_rejects_rounded_occurrences() {
    let observed = utc("2026-09-27T12:34:30.250+00:00");
    assert_eq!(
        check_minutely_cron_projection(utc("2026-09-27T12:35:00+00:00"), observed, observed),
        Ok(())
    );

    for wrong in [
        // The observation plus a minute, keeping its sub-minute component.
        "2026-09-27T12:35:30.250+00:00",
        // Rounded down to the minute already under way.
        "2026-09-27T12:34:00+00:00",
        // Rounded past the next minute.
        "2026-09-27T12:36:00+00:00",
    ] {
        assert!(
            check_minutely_cron_projection(utc(wrong), observed, observed).is_err(),
            "{wrong} must be rejected"
        );
    }
}

#[tokio::test]
async fn list_reports_never_observed_when_the_cursor_is_missing() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("fresh")).expect("add");
    let (state, _) = state(runtime);
    let response = send(state, Method::GET, "/auto-tasks?workspace=default", None).await;
    let json = body_json(response).await;
    let item = &json["definitions"].as_array().expect("definitions")[0];
    assert_eq!(item["enabled"], true);
    assert_eq!(item["next_evaluation"]["state"], "never_observed");
    assert_eq!(item["next_evaluation"]["hypothetical"], false);
    assert!(item["next_evaluation"]["at"].is_null(), "{item}");
}

#[tokio::test]
async fn list_reports_disabled_and_unavailable_delivery_definitions() {
    let runtime = runtime();
    runtime
        .auto_task_add(delivery_params(
            "delivery-qa",
            CoverageClass::IntegratedQaV1,
        ))
        .expect("add");
    let (state, runtime) = state(runtime);
    let disabled = body_json(
        send(
            state.clone(),
            Method::GET,
            "/auto-tasks?workspace=default",
            None,
        )
        .await,
    )
    .await;
    let disabled_item = &disabled["definitions"].as_array().expect("definitions")[0];
    assert_eq!(disabled_item["next_evaluation"]["state"], "disabled");
    assert!(
        disabled_item["next_evaluation"]["at"].is_null(),
        "{disabled_item}"
    );

    runtime
        .auto_task_toggle("delivery-qa", true)
        .expect("enable delivery definition");
    let enabled =
        body_json(send(state, Method::GET, "/auto-tasks?workspace=default", None).await).await;
    let enabled_item = &enabled["definitions"].as_array().expect("definitions")[0];
    assert_eq!(
        enabled_item["next_evaluation"]["state"], "unavailable",
        "in-memory runtimes have no delivery coverage; inspect failure is unavailable, not waiting: {enabled_item}"
    );
    assert!(
        enabled_item["next_evaluation"]["at"].is_null(),
        "{enabled_item}"
    );
}

#[tokio::test]
async fn list_separates_manual_mint_from_scheduler_evaluation() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("nightly")).expect("add");
    write_cursor(&runtime, "nightly", "ORB-00001");
    let (state, runtime) = state(runtime);
    let minted = as_operator(send(
        state.clone(),
        Method::POST,
        "/auto-tasks/mint?workspace=default",
        Some(r#"{"name":"nightly","acknowledge_unconditional":true}"#),
    ))
    .await;
    assert_eq!(minted.status(), StatusCode::OK);
    let minted_id = body_json(minted).await["task_id"]
        .as_str()
        .expect("minted id")
        .to_string();

    let response = send(state, Method::GET, "/auto-tasks?workspace=default", None).await;
    let json = body_json(response).await;
    let item = &json["definitions"].as_array().expect("definitions")[0];
    assert_eq!(item["last_evaluation"]["last_task_id"], "ORB-00001");
    assert_eq!(item["last_minted_task_id"], minted_id);
    assert_ne!(minted_id, "ORB-00001");
    let cursor = runtime
        .list_tasks_by_tags(&["auto-task:nightly".into()])
        .expect("list");
    assert_eq!(cursor[0].id.to_string(), minted_id);
}

#[tokio::test]
async fn list_uses_delivery_coverage_wire_name_in_schedule_summary() {
    let runtime = runtime();
    runtime
        .auto_task_add(delivery_params(
            "delivery-qa",
            CoverageClass::IntegratedQaV1,
        ))
        .expect("add");
    let (state, _) = state(runtime);

    let response = send(state, Method::GET, "/auto-tasks?workspace=default", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let definition = json["definitions"]
        .as_array()
        .expect("definitions")
        .first()
        .expect("delivery definition");

    assert_eq!(
        definition["schedule_summary"],
        "3 deliveries on agent-main (integrated_qa_v1)"
    );
    assert_eq!(
        definition["schedule"]["deliveries_landed"]["coverage"],
        "integrated_qa_v1"
    );
}

#[tokio::test]
async fn unknown_workspace_stays_read_only() {
    let (state, _) = state(runtime());
    let response = send(state, Method::GET, "/auto-tasks?workspace=missing", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["definitions"], serde_json::json!([]));
    assert_eq!(json["controls_authorized"], false);
    assert!(
        json["read_only_reason"]
            .as_str()
            .expect("reason")
            .contains("not a concrete active selection"),
        "{json}"
    );
}

#[tokio::test]
async fn toggle_requires_an_explicit_workspace() {
    let (state, _) = state(runtime());
    let response = send(
        state,
        Method::POST,
        "/auto-tasks/toggle",
        Some(r#"{"name":"nightly","expected_enabled":true,"enabled":false}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = body_json(response).await;
    assert_eq!(json["code"], "workspace_required");
}

#[tokio::test]
async fn toggle_denies_a_caller_without_operator_capability() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("nightly")).expect("add");
    let (state, runtime) = state(runtime);
    let response = as_agent(send(
        state,
        Method::POST,
        "/auto-tasks/toggle?workspace=default",
        Some(r#"{"name":"nightly","expected_enabled":true,"enabled":false}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let json = body_json(response).await;
    assert_eq!(json["code"], "authorization_denied");
    assert_eq!(json["operation"], "auto_task.toggle");
    assert!(
        runtime
            .auto_task_show("nightly")
            .expect("show")
            .expect("present")
            .enabled
    );
}

/// A missing auto-task is a permanent 404, not a 409 a client would refresh
/// and retry forever.
#[tokio::test]
async fn toggle_of_an_unknown_auto_task_is_not_found() {
    let (state, _) = state(runtime());
    let response = as_operator(send(
        state,
        Method::POST,
        "/auto-tasks/toggle?workspace=default",
        Some(r#"{"name":"no-such-auto-task","expected_enabled":true,"enabled":false}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let json = body_json(response).await;
    assert_eq!(json["code"], "auto_task_not_found");
}

#[tokio::test]
async fn toggle_rolls_back_when_expected_state_is_stale() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("nightly")).expect("add");
    runtime
        .auto_task_toggle("nightly", false)
        .expect("disable first");
    let (state, runtime) = state(runtime);
    let response = as_operator(send(
        state,
        Method::POST,
        "/auto-tasks/toggle?workspace=default",
        Some(r#"{"name":"nightly","expected_enabled":true,"enabled":false}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let json = body_json(response).await;
    assert_eq!(json["code"], "stale_auto_task_state");
    assert_eq!(json["actual_enabled"], false);
    assert!(
        !runtime
            .auto_task_show("nightly")
            .expect("show")
            .expect("present")
            .enabled
    );
}

#[tokio::test]
async fn authorized_toggle_disables_and_records_audit() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("nightly")).expect("add");
    let (state, runtime) = state(runtime);
    let response = as_operator(send(
        state,
        Method::POST,
        "/auto-tasks/toggle?workspace=default",
        Some(r#"{"name":"nightly","expected_enabled":true,"enabled":false}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["enabled"], false);
    assert_eq!(json["changed"], true);
    assert_eq!(json["message"], "Auto-task disabled");
    assert!(
        !runtime
            .auto_task_show("nightly")
            .expect("show")
            .expect("present")
            .enabled
    );
    let events = runtime
        .list_audit_events_with_kind(
            None,
            None,
            Some("auto_task.toggle".to_string()),
            None,
            None,
            20,
        )
        .expect("audit");
    assert!(
        events
            .iter()
            .any(|event| event.status.to_string() == "success"
                && event.target_id.as_deref() == Some("nightly")),
        "{events:?}"
    );
}

#[tokio::test]
async fn mint_requires_unconditional_acknowledgement() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("nightly")).expect("add");
    let (state, runtime) = state(runtime);
    let response = as_operator(send(
        state,
        Method::POST,
        "/auto-tasks/mint?workspace=default",
        Some(r#"{"name":"nightly"}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = body_json(response).await;
    assert_eq!(json["code"], "unconditional_mint_not_acknowledged");
    assert!(
        json["error"]
            .as_str()
            .expect("error")
            .contains("ignores this definition's schedule"),
        "{json}"
    );
    assert!(
        runtime
            .list_tasks_by_tags(&["auto-task:nightly".into()])
            .expect("list")
            .is_empty()
    );
}

#[tokio::test]
async fn mint_denies_a_caller_without_operator_capability() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("nightly")).expect("add");
    let (state, _) = state(runtime);
    let response = as_agent(send(
        state,
        Method::POST,
        "/auto-tasks/mint?workspace=default",
        Some(r#"{"name":"nightly","acknowledge_unconditional":true}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let json = body_json(response).await;
    assert_eq!(json["code"], "authorization_denied");
    assert_eq!(json["operation"], "auto_task.mint");
}

#[tokio::test]
async fn mint_returns_the_created_task_id() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("nightly")).expect("add");
    let (state, runtime) = state(runtime);
    let response = as_operator(send(
        state,
        Method::POST,
        "/auto-tasks/mint?workspace=default",
        Some(r#"{"name":"nightly","acknowledge_unconditional":true}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let task_id = json["task_id"].as_str().expect("task id");
    assert!(!task_id.is_empty(), "{json}");
    assert_eq!(json["status"], "backlog");
    assert!(json["message"].as_str().expect("message").contains(task_id));
    let tasks = runtime
        .list_tasks_by_tags(&["auto-task:nightly".into()])
        .expect("list");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id.to_string(), task_id);
    assert_eq!(tasks[0].complexity, Some(TaskComplexity::Medium));
}

#[tokio::test]
async fn mint_returns_the_exact_server_error() {
    let (state, _) = state(runtime());
    let response = as_operator(send(
        state,
        Method::POST,
        "/auto-tasks/mint?workspace=default",
        Some(r#"{"name":"missing","acknowledge_unconditional":true}"#),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = body_json(response).await;
    assert!(
        json["error"]
            .as_str()
            .expect("error")
            .contains("no such auto-task 'missing'"),
        "{json}"
    );
}

#[tokio::test]
async fn list_capabilities_match_toggle_and_mint_authorization() {
    let (state, _) = state(runtime());
    for operator in [false, true] {
        let request = send(
            state.clone(),
            Method::GET,
            "/auto-tasks?workspace=default",
            None,
        );
        let response = if operator {
            as_operator(request).await
        } else {
            as_agent(request).await
        };
        let json = body_json(response).await;
        for action in ["auto_task_toggle", "auto_task_mint"] {
            assert_eq!(json["capabilities"][action]["authorized"], operator);
            if operator {
                assert!(json["capabilities"][action]["reason"].is_null());
            } else {
                assert!(
                    json["capabilities"][action]["reason"]
                        .as_str()
                        .expect("denial")
                        .contains("operator")
                );
            }
        }
    }
}

#[tokio::test]
async fn toggle_both_directions_reads_back_persisted_state() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("nightly")).expect("add");
    let (state, _) = state(runtime);
    for enabled in [false, true] {
        let body =
            serde_json::json!({"name":"nightly", "expected_enabled": !enabled, "enabled": enabled})
                .to_string();
        let response = as_operator(send(
            state.clone(),
            Method::POST,
            "/auto-tasks/toggle?workspace=default",
            Some(&body),
        ))
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let listed = body_json(
            send(
                state.clone(),
                Method::GET,
                "/auto-tasks?workspace=default",
                None,
            )
            .await,
        )
        .await;
        assert_eq!(listed["definitions"][0]["enabled"], enabled);
    }
}

#[tokio::test]
async fn list_reports_no_open_duplicate_when_only_instance_is_someday() {
    let runtime = runtime();
    runtime
        .auto_task_add(chore_params("parked-chore"))
        .expect("add");

    // Mint an instance and park it in Someday.
    let task = runtime.auto_task_mint("parked-chore").expect("mint");
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Someday),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("park task");

    let (app_state, runtime) = state(runtime);
    let response = send(
        app_state.clone(),
        Method::GET,
        "/auto-tasks?workspace=default",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let item = json["definitions"]
        .as_array()
        .expect("definitions")
        .iter()
        .find(|item| item["name"] == "parked-chore")
        .expect("parked-chore");

    assert_eq!(item["open_duplicate"], false);
    assert_eq!(item["may_create_open_duplicate"], false);
    assert_eq!(item["last_minted_task_status"], "someday");

    // Once an active instance exists, the same query marks open_duplicate as true.
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("activate task");

    let response = send(
        app_state,
        Method::GET,
        "/auto-tasks?workspace=default",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let item = json["definitions"]
        .as_array()
        .expect("definitions")
        .iter()
        .find(|item| item["name"] == "parked-chore")
        .expect("parked-chore");

    assert_eq!(item["open_duplicate"], true);
    assert_eq!(item["may_create_open_duplicate"], true);
    assert_eq!(item["last_minted_task_status"], "backlog");
}

/// A definition seeded by a plugin this host never enabled is hidden from the
/// panel and its counts; the opt-in lists it marked inactive with the reason.
#[tokio::test]
async fn list_hides_an_inactive_plugins_definition_unless_asked() {
    let runtime = runtime();
    runtime.auto_task_add(chore_params("nightly")).expect("add");
    runtime
        .auto_task_add(chore_params("ghost-reindex"))
        .expect("add");
    let path = orbit_core::application::auto_tasks::definition_path(
        &runtime.paths().local_dir,
        "ghost-reindex",
    );
    let body = std::fs::read_to_string(&path).expect("read");
    std::fs::write(&path, format!("# provenance: plugin:ghost@1.0.0\n{body}"))
        .expect("seed provenance");
    let (state, _) = state(runtime);

    let json = body_json(
        send(
            state.clone(),
            Method::GET,
            "/auto-tasks?workspace=default",
            None,
        )
        .await,
    )
    .await;
    let names: Vec<&str> = json["definitions"]
        .as_array()
        .expect("definitions")
        .iter()
        .filter_map(|item| item["name"].as_str())
        .collect();
    assert_eq!(names, vec!["nightly"], "{json}");
    assert_eq!(json["inactive_plugin_count"], 1);

    let json = body_json(
        send(
            state,
            Method::GET,
            "/auto-tasks?workspace=default&include_inactive_plugins=true",
            None,
        )
        .await,
    )
    .await;
    let definitions = json["definitions"].as_array().expect("definitions");
    let ghost = definitions
        .iter()
        .find(|item| item["name"] == "ghost-reindex")
        .expect("listed on request");
    assert_eq!(ghost["plugin_inactive"], true);
    assert_eq!(ghost["inactive_plugin"]["namespace"], "ghost");
    assert!(
        ghost["skipped_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("orbit plugin enable ghost")),
        "{ghost}"
    );
    let nightly = definitions
        .iter()
        .find(|item| item["name"] == "nightly")
        .expect("live definition");
    assert_eq!(nightly["plugin_inactive"], false);
}
