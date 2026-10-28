//! Regression for incident lists expanding every repeated raw event.

use orbit_core::{AuditEventStatus, FailureIncidentQuery};
use serde_json::{Value, json};

use super::audit;
use super::support::{Fixture, isolated, json_ok};

#[test]
fn incident_list_bounds_repeated_evidence_and_keeps_exact_raw_drilldown() {
    isolated(
        "incident_lists::incident_list_bounds_repeated_evidence_and_keeps_exact_raw_drilldown",
        || {
            let fixture = Fixture::new();
            let message = format!(
                "cargo test crashed: {} tail-marker",
                "diagnostic context ".repeat(30)
            );
            for run in ["burst-alpha", "burst-beta"] {
                for index in 0..41 {
                    let mut event = audit::row(
                        &format!("{run}-{index}"),
                        Some(if index == 40 {
                            "fixture.wait"
                        } else {
                            "fixture.validate"
                        }),
                        AuditEventStatus::Failure,
                    );
                    event.job_run_id = Some(run.into());
                    event.error_message = Some(if index == 40 {
                        "child run failed".into()
                    } else {
                        message.clone()
                    });
                    fixture.runtime.record_audit_event(&event).unwrap();
                }
            }
            let report = fixture
                .runtime
                .audit_failure_incidents(&FailureIncidentQuery::default())
                .unwrap();
            assert_eq!(report.incidents.len(), 2);
            let server = fixture.server(false);
            let payload = json_ok(server.get("/api/audit/incidents?since=24h&class=unexpected&limit=50&workspace=ws_http_fixture"));
            assert_eq!(payload["raw_failed_events"], 82);
            assert_eq!(payload["matching_incident_count"], 2);
            for (incident, expected) in payload["incidents"]
                .as_array()
                .unwrap()
                .iter()
                .zip(&report.incidents)
            {
                let preview = incident["events"].as_array().unwrap();
                assert!(
                    preview.len() <= 5,
                    "repeated evidence must not expand polled lists: {} references",
                    preview.len()
                );
                assert_eq!(
                    incident["events_omitted"],
                    expected.events.len() - preview.len()
                );
                let ids = expected
                    .events
                    .iter()
                    .map(|event| event.id)
                    .collect::<Vec<_>>();
                assert_eq!(incident["event_ids"], json!(ids));
                assert_eq!(
                    preview
                        .iter()
                        .map(|event| event["id"].as_i64().unwrap())
                        .collect::<Vec<_>>(),
                    ids[..preview.len()]
                );
                assert_eq!(incident["incident_id"], expected.incident_id);
                assert_eq!(incident["event_count"], expected.event_count);
                assert_eq!(incident["root_event_count"], expected.root_event_count);
                assert_eq!(incident["propagated_event_count"], 1);
                assert_eq!(incident["class"], "unexpected");
                assert_eq!(incident["first_ts"], expected.first_ts.to_rfc3339());
                assert_eq!(incident["last_ts"], expected.last_ts.to_rfc3339());
                assert_eq!(incident["run_ids"], json!(expected.run_ids));
                let propagation = incident["propagation"].as_array().unwrap();
                assert_eq!(propagation.len(), expected.propagation.len());
                assert_eq!(
                    propagation[0]["signature"],
                    expected.propagation[0].signature
                );
                assert_eq!(propagation[0]["event_count"], 1);

                let requested = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
                let raw = json_ok(server.get(&format!(
                    "/api/audit?workspace=ws_http_fixture&ids={requested}"
                )));
                let raw = raw.as_array().unwrap();
                assert_eq!(
                    raw.len(),
                    ids.len(),
                    "all evidence remains accessible on explicit drilldown"
                );
                assert_eq!(
                    raw.iter()
                        .map(|event| event["id"].as_i64().unwrap())
                        .collect::<Vec<_>>(),
                    ids
                );
                let omitted_root = expected.events.last().unwrap();
                let full: Value = json_ok(server.get(&format!(
                    "/api/audit/events/{}/message?workspace=ws_http_fixture",
                    omitted_root.id
                )));
                assert_eq!(full["message"], message);
            }
        },
    );
}
