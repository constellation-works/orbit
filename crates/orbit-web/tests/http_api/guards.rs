use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::support::{Fixture, isolated};

/// Ordinary writes intentionally outside the exceptional-operation registry.
/// New methods/paths default to operator-only, so a new unguarded route fails.
const ORDINARY_WRITES: &[(&str, &str)] = &[
    ("POST", "/tasks"),
    ("PATCH", "/tasks/:id"),
    ("POST", "/tasks/:id/comments"),
    ("POST", "/tasks/:id/approve"),
    ("POST", "/tasks/:id/reject"),
    ("POST", "/tasks/:id/archive"),
    ("POST", "/frictions"),
    ("PATCH", "/frictions/:id"),
    ("POST", "/frictions/:id/resolve"),
    ("POST", "/job-runs/:id/resume"),
    ("POST", "/workflows/ship"),
    ("POST", "/runs/:id/cancel"),
    ("POST", "/runs/:id/replay"),
    ("POST", "/metrics/invocations"),
];

/// Discover paths from registrations, then ask the real HTTP router for methods.
/// This source input is only discovery for a security guard, never a text assertion.
/// A nonliteral registration fails closed instead of silently dropping coverage.
fn registered_paths() -> impl Iterator<Item = &'static str> {
    include_str!("../../src/api/routes.rs")
        .split(".route(")
        .skip(1)
        .map(|registration| {
            registration
                .trim_start()
                .strip_prefix('"')
                .and_then(|literal| literal.split('"').next())
                .expect(
                    "router-wide origin/operator safety coverage requires discoverable route paths",
                )
        })
}

fn body(path: &str) -> Value {
    match path {
        "/config/keys/:key" => json!({"value":"agent-main","init":"fresh"}),
        "/config/crews/:name" => json!({"fields":{}}),
        "/routines/clock" => json!({
            "action":"disable", "machine_name":"fixture", "expected_enabled":true,
            "expected_cadence_seconds":300,
        }),
        "/routines/toggle" => json!({
            "name":"missing", "source":"fixture", "target":"job:missing",
            "machine_name":"fixture", "expected_enabled":true,"enabled":false,
        }),
        "/auto-tasks/toggle" => json!({"name":"missing","expected_enabled":true,"enabled":false}),
        "/auto-tasks/mint" => json!({"name":"missing","acknowledge_unconditional":true}),
        "/workflows/auto" => json!({"for_duration":"1m","complete":true}),
        _ => json!({
            "title":"guard fixture", "description":"HTTP guard check", "scope":"host",
            "expected_candidate_commit":"a", "expected_base_commit":"b",
            "expected_phase":"claimed", "request_id":"guard-fixture", "status":"blocked",
            "reason":"HTTP guard check", "comment":"HTTP guard check",
            "task_ids":["missing"], "mode":"local",
        }),
    }
}

#[test]
fn every_router_mutation_enforces_origin_and_operator_policy() {
    isolated(
        "guards::every_router_mutation_enforces_origin_and_operator_policy",
        || {
            let fixture = Fixture::new();
            let operator = fixture.server(true);
            let agent = fixture.server(false);
            let mut mutations = BTreeSet::new();
            for path in registered_paths() {
                let concrete = path
                    .replace(":id", "missing")
                    .replace(":key", "workflow.base_branch")
                    .replace(":name", "missing")
                    .replace(":namespace", "missing")
                    .replace(":panel", "missing")
                    .replace(":kind", "routine")
                    .replace(":batch", "missing")
                    .replace("*path", "missing");
                let uri = format!("/api{concrete}?workspace=ws_http_fixture");
                let discovery = operator.request("OPTIONS", &uri).send().unwrap();
                assert_eq!(discovery.status().as_u16(), 405, "route discovery: {path}");
                let allowed = discovery
                    .headers()
                    .get("allow")
                    .expect("router's HTTP Allow header")
                    .to_str()
                    .unwrap()
                    .to_string();
                for method in allowed
                    .split(',')
                    .map(str::trim)
                    .filter(|method| matches!(*method, "POST" | "PUT" | "PATCH" | "DELETE"))
                {
                    mutations.insert((method.to_string(), path.to_string()));
                    // Cover CSRF, DNS rebinding, missing/malformed host, and authority mismatch.
                    for (host, origin) in [
                        (None, None),
                        (None, Some("https://attacker.example")),
                        (Some("attacker.example"), Some(operator.origin.as_str())),
                        (Some(""), Some(operator.origin.as_str())),
                        (None, Some("http://localhost:1")),
                        (None, Some("null")),
                    ] {
                        let mut request = operator.request(method, &uri).json(&body(path));
                        if let Some(host) = host {
                            request = request.header("host", host);
                        }
                        if let Some(origin) = origin {
                            request = request.header("origin", origin);
                        }
                        let response = request.send().unwrap();
                        assert_eq!(
                            response.status().as_u16(),
                            403,
                            "origin guard must protect {method} {path}, host={host:?}, origin={origin:?}"
                        );
                        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                        let payload: Value = response.json().unwrap();
                        assert!(payload["error"].is_string(), "{payload}");
                    }

                    let response = agent.send(method, &uri, body(path));
                    let status = response.status().as_u16();
                    let payload: Value = response.json().unwrap();
                    if ORDINARY_WRITES.contains(&(method, path)) {
                        assert_ne!(
                            payload["code"], "authorization_denied",
                            "ordinary write must preserve agent admission: {method} {path}: {status} {payload}"
                        );
                    } else {
                        assert_eq!(
                            status, 403,
                            "operator guard must protect {method} {path}: {payload}"
                        );
                        assert_eq!(
                            payload["code"], "authorization_denied",
                            "{method} {path}: {payload}"
                        );
                    }
                }
            }
            for &(method, path) in ORDINARY_WRITES {
                assert!(
                    mutations.contains(&(method.into(), path.into())),
                    "stale authorization exception has no HTTP route: {method} {path}"
                );
            }
            assert!(
                !mutations.is_empty(),
                "router-wide mutation security table must execute HTTP checks"
            );
        },
    );
}

#[test]
fn config_key_admission_refuses_invalid_writes_and_preserves_types() {
    isolated(
        "guards::config_key_admission_refuses_invalid_writes_and_preserves_types",
        || {
            let fixture = Fixture::new();
            let config = fixture.work.join("config.toml");
            std::fs::write(&config, "# fixture\n").unwrap();
            let server = fixture.server(true);
            for (key, value) in [
                ("workflow.base_brunch", json!("main")),
                ("execution.codex.sandbox", json!("wide-open")),
                ("scoring.enabled", json!("false")),
            ] {
                let before = std::fs::read(&config).unwrap();
                let response = server.send(
                    "PUT",
                    &format!("/api/config/keys/{key}?workspace=ws_http_fixture"),
                    json!({"value":value}),
                );
                assert_eq!(
                    response.status().as_u16(),
                    400,
                    "admission must refuse {key}"
                );
                assert!(response.json::<Value>().unwrap()["error"].is_string());
                assert_eq!(
                    std::fs::read(&config).unwrap(),
                    before,
                    "refused config write must be byte-identical"
                );
            }
            let written = super::support::json_ok(server.send(
                "PUT",
                "/api/config/keys/scoring.enabled?workspace=ws_http_fixture",
                json!({"value":false}),
            ));
            assert_eq!(written["new_value"], false);
            let read = super::support::json_ok(
                server.get("/api/config/effective?workspace=ws_http_fixture"),
            );
            let row = read["sections"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|section| section["keys"].as_array().unwrap())
                .find(|row| row["key"] == "scoring.enabled")
                .unwrap();
            assert_eq!(
                row["value"], false,
                "a boolean config write stays a boolean on HTTP readback"
            );
            assert_eq!(row["source"]["layer"], "workspace");
        },
    );
}
