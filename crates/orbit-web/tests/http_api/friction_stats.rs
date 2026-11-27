//! One Frictions refresh (Open list, Triaged list, Stats) computes the
//! workspace friction rates once and serves every response the same rates.

use std::time::Duration;

use serde_json::{Value, json};

use super::support::{Fixture, Server, isolated, json_ok};

const WORKSPACE: &str = "workspace=ws_http_fixture";

/// Workspace-wide friction rate computations the server has run so far.
fn computations(server: &Server) -> usize {
    server
        .task_query_trace()
        .iter()
        .filter(|span| span["target"] == "orbit.core.friction_stats")
        .count()
}

/// One default active refresh: Open list, Triaged list and Stats, in the order
/// the dashboard issues them. Returns the three bodies.
fn refresh(server: &Server) -> [Value; 3] {
    let open = json_ok(server.get(&format!("/api/frictions?status=open&limit=100&{WORKSPACE}")));
    let triaged = json_ok(server.get(&format!(
        "/api/frictions?status=triaged&limit=100&{WORKSPACE}"
    )));
    let stats = json_ok(server.get(&format!("/api/frictions/stats?{WORKSPACE}")));
    [open, triaged, stats]
}

#[test]
fn one_frictions_refresh_computes_the_rates_once() {
    isolated(
        "friction_stats::one_frictions_refresh_computes_the_rates_once",
        || {
            let fixture = Fixture::new();
            let server = fixture.counted_task_server();

            // An empty workspace answers with zero counts and one computation.
            let [open, triaged, stats] = refresh(&server);
            assert_eq!(computations(&server), 1);
            assert_eq!(stats["total"], 0);
            assert_eq!(open["items"], json!([]));
            assert_eq!(triaged["items"], json!([]));
            assert_eq!(open["stats"], stats);
            assert_eq!(triaged["stats"], stats);

            // A dashboard write is visible to the very next read.
            let mut ids = Vec::new();
            for body in ["first", "second", "third"] {
                let created = json_ok(server.send(
                    "POST",
                    &format!("/api/frictions?{WORKSPACE}"),
                    json!({"body": body, "tags": ["tooling"]}),
                ));
                ids.push(created["id"].as_str().expect("friction id").to_string());
            }
            json_ok(server.send(
                "PATCH",
                &format!("/api/frictions/{}?{WORKSPACE}", ids[0]),
                json!({"status": "triaged"}),
            ));
            let before = computations(&server);
            let [open, triaged, stats] = refresh(&server);
            assert_eq!(computations(&server) - before, 1);
            assert_eq!(open["items"].as_array().expect("open items").len(), 2);
            assert_eq!(triaged["items"].as_array().expect("triaged items").len(), 1);
            assert_eq!(stats["total"], 3);
            assert_eq!(stats["open"], 2);
            assert_eq!(stats["triaged"], 1);
            assert_eq!(open["stats"], stats);
            assert_eq!(triaged["stats"], stats);
            assert!(
                open["tags"]
                    .as_array()
                    .expect("tags")
                    .contains(&json!("tooling"))
            );

            // The next ordinary refresh, once the memo has aged out, recomputes.
            std::thread::sleep(Duration::from_secs(3));
            let before = computations(&server);
            let [_, _, again] = refresh(&server);
            assert_eq!(computations(&server) - before, 1);
            assert_eq!(again, stats);

            json_ok(server.send(
                "POST",
                &format!("/api/frictions/{}/resolve?{WORKSPACE}", ids[1]),
                json!({}),
            ));
            let [_, _, resolved] = refresh(&server);
            assert_eq!(resolved["resolved"], 1);
            assert_eq!(resolved["open"], 1);
        },
    );
}
