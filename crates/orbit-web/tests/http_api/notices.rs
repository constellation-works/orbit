//! `GET /api/notices`: complete aggregate reads and retained-history visibility.

use chrono::{Duration, Utc};
use orbit_core::application::notice::{NoticeCaller, NoticePostRequest, NoticeSelector};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::record::NoticeKind;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workspace::{WorkspaceCheckout, WorkspaceStatus};
use serde_json::{Value, json};
use std::path::PathBuf;

use super::support::{Fixture, isolated, json_ok};

fn operator_caller() -> NoticeCaller {
    let mut session = ToolSessionContext::default();
    session.effective_capabilities.insert(McpCapability::Agent);
    session
        .effective_capabilities
        .insert(McpCapability::Operator);
    NoticeCaller {
        session,
        scope_task_id: None,
        scope_run_id: None,
        orchestrator: None,
        model: None,
    }
}

fn post(runtime: &OrbitRuntime, kind: NoticeKind, title: &str, dedupe_key: &str) -> String {
    runtime
        .post_notice(
            &operator_caller(),
            NoticePostRequest {
                kind,
                title: title.to_string(),
                body: None,
                scope: None,
                dedupe_key: Some(dedupe_key.to_string()),
                linked_task: None,
                close_on_linked_done: None,
                evidence: Vec::new(),
                hosts: Vec::new(),
                expires_in_hours: None,
                expires_at: None,
            },
        )
        .unwrap()
        .notice
        .id
}

/// Seed at least 200 retained closed incidents, then one open maintenance.
fn seed_saturated_history(fixture: &Fixture) -> String {
    for index in 0..200 {
        let id = post(
            &fixture.runtime,
            NoticeKind::Incident,
            &format!("saturated incident {index}"),
            &format!("saturated-incident-{index}"),
        );
        fixture
            .runtime
            .close_notice(&operator_caller(), NoticeSelector::Id(id), "resolved")
            .unwrap();
    }
    post(
        &fixture.runtime,
        NoticeKind::Maintenance,
        "planned maintenance window",
        "open-maintenance",
    )
}

fn assert_maintenance_shown(payload: &Value, maintenance_id: &str) {
    let open = payload["open"].as_array().unwrap();
    assert!(
        open.iter().any(|notice| notice["id"] == maintenance_id),
        "open must include the maintenance notice: {payload}"
    );
    let banner = payload["banner"].as_array().unwrap();
    assert!(
        banner.iter().any(|notice| notice["id"] == maintenance_id),
        "banner must include the maintenance notice: {payload}"
    );
    assert_eq!(payload["degraded"], json!([]));
}

#[test]
fn open_maintenance_survives_200_retained_closed_incidents() {
    isolated(
        "notices::open_maintenance_survives_200_retained_closed_incidents",
        || {
            let fixture = Fixture::new();
            let maintenance_id = seed_saturated_history(&fixture);
            let server = fixture.server(false);
            let payload = json_ok(server.get("/api/notices"));
            assert_maintenance_shown(&payload, &maintenance_id);
            assert_eq!(
                payload["recent"].as_array().unwrap().len(),
                200,
                "closed incidents inside 24h stay in recent: {}",
                payload["recent"]
            );
        },
    );
}

#[test]
fn open_maintenance_survives_closed_incidents_older_than_24_hours() {
    isolated(
        "notices::open_maintenance_survives_closed_incidents_older_than_24_hours",
        || {
            let fixture = Fixture::new();
            let maintenance_id = seed_saturated_history(&fixture);
            // Age every closed incident past the 24h display window while
            // staying inside the 30-day retention window.
            let ended = Utc::now() - Duration::hours(25);
            let ended_stamp = ended.to_rfc3339();
            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .with_transaction(|tx| {
                    let conn = tx.connection();
                    let mut statement = conn
                        .prepare("SELECT id, record_json FROM notices WHERE kind = 'incident'")
                        .map_err(|error| OrbitError::Store(error.to_string()))?;
                    let rows = statement
                        .query_map([], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })
                        .map_err(|error| OrbitError::Store(error.to_string()))?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|error| OrbitError::Store(error.to_string()))?;
                    drop(statement);
                    assert_eq!(rows.len(), 200);
                    for (id, record_json) in rows {
                        let mut document: Value = serde_json::from_str(&record_json)
                            .map_err(|error| OrbitError::Store(error.to_string()))?;
                        document["closed"]["at"] = json!(ended_stamp);
                        document["updated_at"] = json!(ended_stamp);
                        conn.execute(
                            "UPDATE notices SET closed_at = ?1, updated_at = ?1, record_json = ?2 WHERE id = ?3",
                            rusqlite::params![
                                ended_stamp,
                                serde_json::to_string(&document).unwrap(),
                                id
                            ],
                        )
                        .map_err(|error| OrbitError::Store(error.to_string()))?;
                    }
                    Ok(())
                })
                .unwrap();
            let server = fixture.server(false);
            let payload = json_ok(server.get("/api/notices"));
            assert_maintenance_shown(&payload, &maintenance_id);
            assert_eq!(
                payload["recent"],
                json!([]),
                "notices ended over 24h ago stay out of recent, without hiding open ones"
            );
        },
    );
}

/// Add another owner-local workspace without making any HTTP request.
fn seed_second_workspace(fixture: &Fixture) -> (PathBuf, String) {
    let repo = fixture.path("second");
    let work = repo.join(".orbit");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        work.join("config.yaml"),
        "schema_version: 1\nworkspace_id: ws_second\n",
    )
    .unwrap();
    std::fs::write(work.join("config.toml"), "").unwrap();
    let registry_path = orbit_registry::workspace_registry::registry_path_for(&fixture.global);
    let mut registry =
        orbit_registry::workspace_registry::load_registry_from(&registry_path).unwrap();
    let mut workspace = registry.workspaces[0].clone();
    workspace.id = "ws_second".into();
    workspace.name = "second".into();
    workspace.status = WorkspaceStatus::Active;
    let checkout = WorkspaceCheckout::owner(workspace.id.clone(), repo, work.clone());
    let runtime = orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
        &fixture.global,
        &workspace,
        &checkout,
    )
    .unwrap();
    let notice_id = post(&runtime, NoticeKind::Incident, "second incident", "second");
    registry.workspaces.push(workspace);
    registry.checkouts.push(checkout);
    orbit_registry::workspace_registry::save_registry_to(&registry, &registry_path).unwrap();
    (work, notice_id)
}

fn assert_notice_ids(payload: &Value, key: &str, ids: &[&str]) {
    let notices = payload[key].as_array().unwrap();
    assert_eq!(notices.len(), ids.len(), "{payload}");
    for id in ids {
        assert!(
            notices.iter().any(|notice| notice["id"] == *id),
            "{key} must include {id}: {payload}"
        );
    }
}

#[test]
fn post_notice_refusals_carry_the_core_invalid_field_code() {
    isolated(
        "notices::post_notice_refusals_carry_the_core_invalid_field_code",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let future = (Utc::now() + Duration::hours(2)).to_rfc3339();
            let cases: Vec<(Value, &str)> = vec![
                (
                    json!({"title": "no kind"}),
                    "`kind` must be incident, maintenance or info",
                ),
                (
                    json!({"kind": "bogus", "title": "bad kind"}),
                    "`kind` must be incident, maintenance or info",
                ),
                (
                    json!({"kind": "info", "title": "bad scope", "scope": "bogus"}),
                    "`scope` must be workspace or global",
                ),
                (
                    json!({"kind": "info", "title": "bad expiry", "expires_at": "not-a-time"}),
                    "`expires_at` must be an RFC 3339 timestamp",
                ),
                (
                    json!({"kind": "info", "title": "both expiries", "expires_at": future, "expires_in_hours": 1}),
                    "pass `expires_in_hours` or `expires_at`, not both",
                ),
                (
                    json!({"kind": "info"}),
                    "`title` must be non-empty single-line text",
                ),
            ];
            for (body, message) in cases {
                let response = server.send("POST", "/api/notices", body);
                assert_eq!(response.status().as_u16(), 400);
                let payload: Value = response.json().unwrap();
                assert_eq!(payload["code"], "notice_invalid_field", "{payload}");
                assert_eq!(payload["error"], message, "{payload}");
            }
            // The HTTP body remains typed; numeric strings are accepted only
            // by the tool input parser, not by this API.
            let response = server.send(
                "POST",
                "/api/notices",
                json!({"kind": "info", "title": "String expiry", "expires_in_hours": "1"}),
            );
            assert_eq!(response.status().as_u16(), 422);

            // Blank form fields mean absent, not a refusal.
            let payload = json_ok(server.send(
                "POST",
                "/api/notices",
                json!({
                    "kind": "info",
                    "title": "blank form fields",
                    "scope": "",
                    "dedupe_key": "",
                    "linked_task": "",
                    "expires_at": "",
                    "evidence": [{"type": "run", "value": "ignored"}],
                }),
            ));
            assert_eq!(payload["outcome"], "created");
            assert!(payload["notice"].is_object());
            assert!(payload["notice"].get("ended_label").is_some());
            assert!(payload["notice"].get("ended_at").is_some());
            assert_eq!(payload["notice"]["scope"], "workspace");
            assert_eq!(payload["notice"]["evidence"], json!([]));
        },
    );
}

#[test]
fn aggregate_notices_resolve_cold_registered_workspaces() {
    isolated(
        "notices::aggregate_notices_resolve_cold_registered_workspaces",
        || {
            let fixture = Fixture::new();
            let first = post(
                &fixture.runtime,
                NoticeKind::Maintenance,
                "first maintenance",
                "first",
            );
            let closed = post(
                &fixture.runtime,
                NoticeKind::Info,
                "resolved info",
                "closed",
            );
            fixture
                .runtime
                .close_notice(
                    &operator_caller(),
                    NoticeSelector::Id(closed.clone()),
                    "resolved",
                )
                .unwrap();
            let (_, second) = seed_second_workspace(&fixture);
            let server = fixture.server(false);
            // /healthz is the harness readiness probe; no workspace route
            // has populated this server's runtime cache.
            let payload = json_ok(server.get("/api/notices?workspace=all"));
            assert_notice_ids(&payload, "open", &[&first, &second]);
            assert_notice_ids(&payload, "banner", &[&first, &second]);
            assert_notice_ids(&payload, "recent", &[&closed]);
            assert_eq!(payload["degraded"], json!([]));
        },
    );
}

#[test]
fn aggregate_notices_report_broken_config_and_recover_without_warming() {
    isolated(
        "notices::aggregate_notices_report_broken_config_and_recover_without_warming",
        || {
            let fixture = Fixture::new();
            let first = post(
                &fixture.runtime,
                NoticeKind::Maintenance,
                "healthy maintenance",
                "first",
            );
            let (second_work, second) = seed_second_workspace(&fixture);
            let server = fixture.server(false);
            // Warm both runtimes so this case isolates config invalidation
            // from the cold-cache regression above.
            json_ok(server.get("/api/workspaces"));
            let read = || json_ok(server.get("/api/notices?workspace=all"));
            assert_notice_ids(&read(), "open", &[&first, &second]);

            std::fs::write(second_work.join("config.toml"), "# valid config edit\n").unwrap();
            let payload = read();
            assert_notice_ids(&payload, "open", &[&first, &second]);
            assert_eq!(payload["degraded"], json!([]));

            std::fs::write(second_work.join("config.toml"), "[broken\n").unwrap();
            for _ in 0..2 {
                let payload = read();
                assert_notice_ids(&payload, "open", &[&first]);
                assert_notice_ids(&payload, "banner", &[&first]);
                let degraded = payload["degraded"].as_array().unwrap();
                assert_eq!(degraded.len(), 1, "{payload}");
                assert!(
                    degraded[0].as_str().unwrap().contains("ws_second"),
                    "{payload}"
                );
            }
            std::fs::write(second_work.join("config.toml"), "# repaired config\n").unwrap();
            let payload = read();
            assert_notice_ids(&payload, "open", &[&first, &second]);
            assert_eq!(payload["degraded"], json!([]));
        },
    );
}
