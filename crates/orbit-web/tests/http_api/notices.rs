//! `GET /api/notices`: retained closed/expired history must not consume
//! the capacity that shows open notices (ORB-21551).

use chrono::{Duration, Utc};
use orbit_core::OrbitError;
use orbit_core::application::notice::{NoticeCaller, NoticePostRequest, NoticeSelector};
use orbit_types::record::NoticeKind;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};

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

fn post(fixture: &Fixture, kind: NoticeKind, title: &str, dedupe_key: &str) -> String {
    fixture
        .runtime
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
            fixture,
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
        fixture,
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
