//! Every auto-task listing reports one decision: the CLI table and records,
//! MCP `orbit.auto_task.list` and its bounded view agree on whether a
//! definition fires here and why not.

use std::fs;

use chrono::{Duration, Utc};
use orbit_core::AutoTaskAddParams;
use orbit_core::application::automation::{consumer_key, evaluate_auto_task};
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::automation::{AcceptedCoverage, BatchState};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use serde_json::{Value, json};

use crate::auto_task_lifecycle_cli::git;
use crate::isolated_cli_fixture::Fixture;
use crate::review_after_landing_cli::{
    commit, enable_review_crew, in_isolated_child, land_and_evaluate, open_runtime, retarget,
    toggle, trigger,
};

const CONSUMER: &str = "delivery-code-review";

fn named<'a>(rows: &'a Value, name: &str) -> &'a Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("{name} is listed: {rows}"))
}

/// The bounded view is an operator surface.
fn bounded(fixture: &Fixture) -> Value {
    let input = json!({"view":"bounded","workspace":"audit-qa","limit":50}).to_string();
    let output = fixture
        .command(&["tool", "run", "orbit.auto_task.list", "--input", &input])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap()
}

/// [ORB-19112] Plain agent-capability listing exposes only the latest
/// scheduler fire or skip, with the minted task's current status.
#[test]
fn listing_reports_last_fire_outcomes_from_scheduler_cursors() {
    const TEST: &str =
        "auto_task_listing_cli::listing_reports_last_fire_outcomes_from_scheduler_cursors";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let runtime = open_runtime(&fixture);
    for name in ["fired", "skipped", "never"] {
        runtime
            .auto_task_add(AutoTaskAddParams {
                name: name.into(),
                description: "last fire listing fixture".into(),
                schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
                template: AutoTaskTemplate {
                    title: format!("Last fire {name}"),
                    description: "Last fire listing fixture".into(),
                    acceptance_criteria: vec![],
                    task_type: TaskType::Chore,
                    tags: vec![],
                    required_tools: vec![],
                    context_files: vec![],
                    priority: TaskPriority::Medium,
                    complexity: None,
                    crew: None,
                    status: TaskStatus::Backlog,
                },
                dedupe: DedupePolicy::Always,
            })
            .unwrap();
    }
    let task = runtime.auto_task_mint("fired").unwrap();
    let task_id = task.id.to_string();
    let cursor_path =
        orbit_core::application::auto_tasks::cursor_state_path(&runtime.paths().state_dir);
    let baseline_at = (Utc::now() - Duration::hours(2)).to_rfc3339();
    let fired_at = (Utc::now() - Duration::hours(1)).to_rfc3339();
    let skipped_at = Utc::now().to_rfc3339();
    fs::create_dir_all(cursor_path.parent().unwrap()).unwrap();
    fs::write(
        cursor_path,
        json!({"definitions": {
            "fired": {
                "baseline_at": baseline_at,
                "last_slot": fired_at,
                "last_fired_at": fired_at,
                "last_task_id": task_id,
            },
            "skipped": {
                "baseline_at": baseline_at,
                "last_skip": {
                    "at": skipped_at,
                    "slot": skipped_at,
                    "reason": "no_blocked_candidates",
                },
            },
            "never": {"baseline_at": baseline_at},
        }})
        .to_string(),
    )
    .unwrap();

    let rows = runtime
        .run_tool_as_human("orbit.auto_task.list", json!({}))
        .unwrap();
    let fired = named(&rows, "fired");
    assert_eq!(
        fired["last_fire"],
        json!({
            "kind": "fired",
            "at": fired_at,
            "task_id": task.id,
            "task_status": task.status,
        }),
        "the fired task's current status is read by id"
    );
    let skipped = named(&rows, "skipped");
    assert_eq!(
        skipped["last_fire"],
        json!({
            "kind": "skipped",
            "at": skipped_at,
            "skip_reason": "no_blocked_candidates",
        })
    );
    assert!(named(&rows, "never")["last_fire"].is_null());
    for row in [fired, skipped] {
        assert!(row.get("run_id").is_none(), "{row}");
        assert!(row.get("logs").is_none(), "{row}");
        assert!(row.get("step_output").is_none(), "{row}");
    }
}

/// [ORB-19122] A delivery schedule never writes the time cursor, so its
/// `last_fire` comes from scheduler-owned automation records: the task a
/// real delivery admission minted, not `null`, and not a stale interval-era
/// cursor left behind when the definition was retargeted to deliveries.
#[test]
fn listing_reports_last_fire_from_a_delivery_admission() {
    const TEST: &str = "auto_task_listing_cli::listing_reports_last_fire_from_a_delivery_admission";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    enable_review_crew(&fixture);
    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    commit(&fixture, "baseline\n");
    retarget(&fixture, &trigger());
    toggle(&fixture, "on");
    let runtime = open_runtime(&fixture);

    // A stale interval-era cursor naming another definition's task.
    runtime
        .auto_task_add(AutoTaskAddParams {
            name: "stale-source".into(),
            description: "stale cursor fixture".into(),
            schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
            template: AutoTaskTemplate {
                title: "Stale source".into(),
                description: "Stale cursor fixture".into(),
                acceptance_criteria: vec![],
                task_type: TaskType::Chore,
                tags: vec![],
                required_tools: vec![],
                context_files: vec![],
                priority: TaskPriority::Medium,
                complexity: None,
                crew: None,
                status: TaskStatus::Backlog,
            },
            dedupe: DedupePolicy::Always,
        })
        .unwrap();
    let stale_task = runtime.auto_task_mint("stale-source").unwrap();
    let cursor_path =
        orbit_core::application::auto_tasks::cursor_state_path(&runtime.paths().state_dir);
    let baseline_at = (Utc::now() - Duration::hours(3)).to_rfc3339();
    let stale_at = (Utc::now() - Duration::hours(2)).to_rfc3339();
    fs::create_dir_all(cursor_path.parent().unwrap()).unwrap();
    fs::write(
        &cursor_path,
        json!({"definitions": {
            CONSUMER: {
                "baseline_at": baseline_at,
                "last_fired_at": stale_at,
                "last_task_id": stale_task.id,
            },
        }})
        .to_string(),
    )
    .unwrap();

    let rows = runtime
        .run_tool_as_human("orbit.auto_task.list", json!({}))
        .unwrap();
    assert!(
        named(&rows, CONSUMER)["last_fire"].is_null(),
        "a delivery definition that has not fired ignores its stale time cursor: {rows}"
    );

    let older_id = land_and_evaluate(&fixture, &runtime, "older delivery\n")
        .expect("the first delivery mints a review task");
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    let key = consumer_key(&runtime, "auto-task", CONSUMER).unwrap();
    let store = runtime.automation_store().unwrap();
    let state = store.automation_state(&key).unwrap().unwrap();
    let active = state.active.as_ref().unwrap();
    let mut next = state.clone();
    next.generation += 1;
    next.covered = active.batch.through_inclusive.clone();
    next.pending_commits.drain(..active.batch.commits.len());
    next.pending.retain(|delivery| {
        !delivery
            .commits
            .iter()
            .all(|sha| active.batch.commits.contains(sha))
    });
    next.waived.retain(|delivery| {
        !delivery
            .commits
            .iter()
            .all(|sha| active.batch.commits.contains(sha))
    });
    next.excluded.retain(|excluded| {
        !excluded
            .delivery
            .commits
            .iter()
            .all(|sha| active.batch.commits.contains(sha))
    });
    next.active = None;
    let evidence = b"fixture evidence".to_vec();
    let receipt = AcceptedCoverage {
        batch_id: active.batch.id.clone(),
        action_id: older_id.clone(),
        input_digest: active.input_digest.clone(),
        evidence_digest: orbit_common::security::release::sha256_hex(&evidence),
        evidence,
        evidence_reference: "fixture".into(),
        submitted_by: "fixture".into(),
        accepted_at: Utc::now() - Duration::hours(1),
    };
    assert!(
        store
            .automation_commit(&state, &next, Some(&receipt))
            .unwrap()
    );
    fixture.json(&[
        "task", "update", &older_id, "--status", "rejected", "--force", "--json",
    ]);
    let minted_id = land_and_evaluate(&fixture, &runtime, "landed delivery\n")
        .expect("the next delivery mints a review task");
    assert_ne!(minted_id, stale_task.id.to_string());
    let minted = runtime.get_task(&minted_id).unwrap();

    let rows = runtime
        .run_tool_as_human("orbit.auto_task.list", json!({}))
        .unwrap();
    let consumer = named(&rows, CONSUMER);
    assert_eq!(
        consumer["last_fire"],
        json!({
            "kind": "fired",
            "at": minted.created_at,
            "task_id": minted.id,
            "task_status": minted.status,
        }),
        "the delivery fire reports its minted task and current status: {consumer}"
    );
    fixture.json(&[
        "task", "update", &minted_id, "--status", "rejected", "--force", "--json",
    ]);
    let now = Utc::now();
    evaluate_auto_task(&runtime, &definition, false, now).unwrap();
    let active = store
        .automation_state(&key)
        .unwrap()
        .unwrap()
        .active
        .unwrap();
    assert_eq!(active.state, BatchState::Claimed);
    assert_eq!(active.attempt, 2);
    assert!(active.action_id.is_none());
    let retry_at = active.retry_after.unwrap();
    assert!(retry_at > now);
    let closed = runtime.get_task(&minted_id).unwrap();
    let rows = runtime
        .run_tool_as_human("orbit.auto_task.list", json!({}))
        .unwrap();
    assert_eq!(
        named(&rows, CONSUMER)["last_fire"],
        json!({
            "kind": "fired", "at": minted.created_at, "task_id": minted.id,
            "task_status": closed.status,
        }),
        "the closed task remains visible during backoff despite an older receipt"
    );
    evaluate_auto_task(
        &runtime,
        &definition,
        false,
        retry_at + Duration::seconds(1),
    )
    .unwrap();
    let retry_id = store
        .automation_state(&key)
        .unwrap()
        .unwrap()
        .active
        .unwrap()
        .action_id
        .unwrap();
    assert_ne!(retry_id, minted_id);
    let retry = runtime.get_task(&retry_id).unwrap();
    let rows = runtime
        .run_tool_as_human("orbit.auto_task.list", json!({}))
        .unwrap();
    assert_eq!(
        named(&rows, CONSUMER)["last_fire"],
        json!({
            "kind": "fired", "at": retry.created_at, "task_id": retry.id,
            "task_status": retry.status,
        })
    );

    let last_fire = consumer["last_fire"].as_object().unwrap();
    assert_eq!(last_fire.len(), 4, "{last_fire:?}");
    for key in ["run_id", "logs", "step_output"] {
        assert!(consumer.get(key).is_none(), "{consumer}");
        assert!(last_fire.get(key).is_none(), "{last_fire:?}");
    }
}

/// [ORB-12867] An enabled delivery definition another machine owns never
/// fires here. The CLI already said so; MCP and its bounded view now report
/// the same reason and a non-live state instead of looking live.
#[test]
fn every_listing_reports_a_delivery_definition_owned_elsewhere_as_skipped() {
    const TEST: &str = "auto_task_listing_cli::every_listing_reports_a_delivery_definition_owned_elsewhere_as_skipped";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    commit(&fixture, "baseline\n");
    let mut elsewhere = trigger();
    elsewhere["owner_machine"] = json!("hm_elsewhere");
    retarget(&fixture, &elsewhere);
    toggle(&fixture, "on");

    let records = fixture.json(&["auto-task", "list", "--format", "json"]);
    let record = named(&records, CONSUMER);
    let reason = record["skipped_reason"]
        .as_str()
        .unwrap_or_else(|| panic!("an unadmittable definition names why: {record}"));
    assert!(reason.contains("hm_elsewhere"), "{reason}");
    assert_eq!(record["plugin_inactive"], false, "{record}");
    assert_eq!(record["effective_enabled"], true, "{record}");

    let table = fixture
        .command(&["auto-task", "list", "--format", "plain"])
        .assert()
        .success();
    let output = table.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let row = stdout
        .lines()
        .find(|line| line.starts_with(CONSUMER))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert_eq!(row.split('\t').nth(1), Some("skipped"), "{row}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains(&format!("skipped [{CONSUMER}]: {reason}")),
        "the skip notice keeps its wording on stderr: {output:?}"
    );

    let runtime = open_runtime(&fixture);
    let plain = runtime
        .run_tool_as_human("orbit.auto_task.list", json!({}))
        .unwrap();
    let listed = named(&plain, CONSUMER);
    assert_eq!(listed["skipped_reason"], reason, "{listed}");
    assert_eq!(listed["effective_enabled"], true, "{listed}");
    assert!(listed.get("plugin_inactive").is_none(), "{listed}");

    let page = bounded(&fixture);
    let item = named(&page["items"], CONSUMER);
    assert_eq!(item["state"], "unavailable", "{item}");
    assert_eq!(item["skip_reason"], reason, "{item}");
    assert!(item["next_due"].is_null(), "{item}");
}

/// The bounded view degrades per row as the dashboard does: an unreadable
/// host cursor or a schedule that cannot be projected marks only the rows it
/// affects unavailable, instead of failing the whole read.
#[test]
fn bounded_listing_marks_unprojectable_rows_unavailable_without_failing() {
    const TEST: &str = "auto_task_listing_cli::bounded_listing_marks_unprojectable_rows_unavailable_without_failing";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    retarget(&fixture, &trigger());
    toggle(&fixture, "on");
    let runtime = open_runtime(&fixture);
    for (name, schedule) in [
        ("hourly", AutoTaskSchedule::Interval { every_minutes: 60 }),
        (
            // Parses, yet no 30 February ever arrives.
            "never",
            AutoTaskSchedule::Cron {
                cron: "0 0 30 2 *".into(),
            },
        ),
    ] {
        runtime
            .auto_task_add(AutoTaskAddParams {
                name: name.into(),
                description: "listing fixture".into(),
                schedule,
                template: AutoTaskTemplate {
                    title: format!("Listing {name}"),
                    description: "Bounded listing fixture".into(),
                    acceptance_criteria: vec![],
                    task_type: TaskType::Chore,
                    tags: vec![],
                    required_tools: vec![],
                    context_files: vec![],
                    priority: TaskPriority::Medium,
                    complexity: None,
                    crew: None,
                    status: TaskStatus::Backlog,
                },
                dedupe: DedupePolicy::SkipIfOpen,
            })
            .unwrap();
    }
    let cursor = orbit_core::application::auto_tasks::cursor_state_path(&runtime.paths().state_dir);
    let baseline = (Utc::now() - Duration::minutes(10)).to_rfc3339();
    fs::create_dir_all(cursor.parent().unwrap()).unwrap();
    fs::write(
        &cursor,
        json!({"definitions":{
            "hourly":{"baseline_at":baseline},
            "never":{"baseline_at":baseline},
        }})
        .to_string(),
    )
    .unwrap();

    let page = bounded(&fixture);
    assert!(page["cursor_state_error"].is_null(), "{page}");
    let hourly = named(&page["items"], "hourly");
    assert_eq!(hourly["state"], "scheduled", "{hourly}");
    assert!(hourly["next_due"].is_string(), "{hourly}");
    let never = named(&page["items"], "never");
    assert_eq!(never["state"], "unavailable", "{never}");
    assert!(never["next_due"].is_null(), "{never}");

    fs::write(&cursor, "{broken").unwrap();
    let page = bounded(&fixture);
    assert!(page["cursor_state_error"].is_string(), "{page}");
    let delivery = named(&page["items"], CONSUMER);
    assert_eq!(delivery["state"], "waiting", "{delivery}");
    assert!(delivery["next_due"].is_null(), "{delivery}");
    assert!(delivery["skip_reason"].is_null(), "{delivery}");
    // Plain MCP and CLI records read the same effective state and skip decision.
    let plain = runtime
        .run_tool_as_human("orbit.auto_task.list", json!({}))
        .unwrap();
    let records = fixture.json(&["auto-task", "list", "--format", "json"]);
    for rows in [&plain, &records] {
        let delivery = named(rows, CONSUMER);
        assert_eq!(delivery["effective_enabled"], true, "{delivery}");
        assert!(delivery["skipped_reason"].is_null(), "{delivery}");
    }
    let hourly = named(&page["items"], "hourly");
    assert_eq!(
        hourly["state"], "unavailable",
        "a corrupt cursor is not silently never observed: {hourly}"
    );
    assert!(hourly["next_due"].is_null(), "{hourly}");
    assert_eq!(named(&page["items"], "never")["state"], "unavailable");
    let mut elsewhere = trigger();
    elsewhere["owner_machine"] = json!("hm_elsewhere");
    retarget(&fixture, &elsewhere);
    assert_eq!(
        named(&bounded(&fixture)["items"], CONSUMER)["state"],
        "unavailable"
    );
    toggle(&fixture, "off");
    assert_eq!(
        named(&bounded(&fixture)["items"], CONSUMER)["state"],
        "disabled"
    );
    assert_eq!(fs::read_to_string(&cursor).unwrap(), "{broken");
}

#[test]
fn bounded_listing_reports_malformed_definition() {
    const TEST: &str = "auto_task_listing_cli::bounded_listing_reports_malformed_definition";
    if !in_isolated_child(TEST) {
        return;
    }
    let fixture = Fixture::new();
    let runtime = open_runtime(&fixture);
    let dir = runtime.paths().orbit_dir.join("auto_tasks");
    fs::write(dir.join("malformed.yaml"), "name: [broken\n").unwrap();
    let page = bounded(&fixture);
    assert!(page["total"].as_u64().unwrap() > 0, "{page}");
    assert!(
        page["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "a valid sibling remains visible beside a malformed definition: {page}"
    );
    let errors = page["load_errors"].as_array().expect("load errors");
    assert_eq!(errors.len(), 1, "{page}");
    assert!(
        errors[0]["path"]
            .as_str()
            .unwrap()
            .contains("malformed.yaml")
    );
    assert!(!errors[0]["message"].as_str().unwrap().is_empty());
    assert_eq!(page["load_errors_total"], 1);
    assert_eq!(page["load_errors_truncated"], false);
    assert!(
        runtime
            .run_tool_as_human("orbit.auto_task.list", json!({}))
            .unwrap()
            .is_array()
    );

    // More failures than the response budget do not change the catalog page.
    let secret = format!("ghp_{}", "a".repeat(36));
    fs::write(dir.join(format!("00-{secret}.yaml")), "name: [broken\n").unwrap();
    for index in 0..21 {
        fs::write(
            dir.join(format!("broken-{index:02}.yaml")),
            "name: [broken\n",
        )
        .unwrap();
    }
    let cursor = orbit_core::application::auto_tasks::cursor_state_path(&runtime.paths().state_dir);
    fs::create_dir_all(cursor.parent().unwrap()).unwrap();
    fs::write(cursor, "{broken").unwrap();
    let input = json!({"view":"bounded","workspace":"audit-qa","offset":1,"limit":1}).to_string();
    let output = fixture
        .command(&["tool", "run", "orbit.auto_task.list", "--input", &input])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let paged: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(paged["total"], page["total"]);
    assert_eq!(paged["items"].as_array().unwrap().len(), 1);
    assert_eq!(paged["items"][0]["name"], page["items"][1]["name"]);
    assert_eq!(paged["pagination"]["offset"], 1);
    assert_eq!(paged["load_errors_total"], 23);
    assert_eq!(paged["load_errors_truncated"], true);
    assert_eq!(paged["load_errors"].as_array().unwrap().len(), 20);
    assert!(paged["cursor_state_error"].is_string());
    // A credential in a diagnostic path must not escape even when included.
    let full = bounded(&fixture);
    assert!(!full.to_string().contains(&secret));
    assert!(
        full["load_errors"]
            .to_string()
            .contains("[REDACTED_SECRET]")
    );
    let names = |value: &Value| {
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| (item["name"].clone(), item["state"] == "plugin_inactive"))
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&full), names(&page));

    // Remove every valid definition from this isolated fixture by corrupting it.
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "yaml") {
            fs::write(path, "name: [broken\n").unwrap();
        }
    }
    fixture
        .command(&["tool", "run", "orbit.auto_task.list", "--input", &input])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .failure();
}
