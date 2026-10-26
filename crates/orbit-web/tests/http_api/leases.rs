use orbit_core::runtime::task::locks::lease;
use serde_json::{Value, json};

use super::support::{Fixture, error_code, isolated, json_ok};

#[test]
fn notice_poll_and_sse_resume_share_durable_sequence() {
    isolated(
        "leases::notice_poll_and_sse_resume_share_durable_sequence",
        || {
            use std::io::BufRead as _;
            let fixture = Fixture::new();
            std::fs::write(
                fixture.work.join("config.toml"),
                "[lease]\nenabled = true\n",
            )
            .unwrap();
            std::fs::write(fixture.work.parent().unwrap().join("held.txt"), "held\n").unwrap();
            let server = fixture.server(true);
            let first = json_ok(server.send(
                "POST",
                "/api/leases",
                json!({"principal":"alice", "selectors":["file:held.txt"]}),
            ));
            let id = first["lease"]["lease_id"].as_str().unwrap();
            let before = json_ok(server.get("/api/leases/notices?after=0"));
            assert_eq!(before["notices"][0]["kind"], "granted");
            assert_eq!(before["notices"][0]["lease_id"], id);
            assert_eq!(first["lease"]["notice_cursor"], 0);
            let cursor = before["cursor"].as_u64().unwrap();
            json_ok(server.send(
                "POST",
                "/api/leases",
                json!({"principal":"bob", "selectors":["file:held.txt"]}),
            ));
            json_ok(server.send("DELETE", &format!("/api/leases/{id}"), json!({})));
            let polled =
                json_ok(server.get(&format!("/api/leases/notices?after={cursor}&limit=1")));
            assert_eq!(polled["notices"].as_array().unwrap().len(), 1);
            let response = server
                .request("GET", "/api/leases/events?after=0&limit=1")
                .header("last-event-id", cursor.to_string())
                .send()
                .unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["content-type"], "text/event-stream");
            let mut stream = std::io::BufReader::new(response);
            let mut line = String::new();
            let mut event_id = None;
            loop {
                line.clear();
                assert!(stream.read_line(&mut line).unwrap() > 0);
                if let Some(id) = line.strip_prefix("id: ") {
                    event_id = Some(id.trim().parse::<u64>().unwrap());
                }
                if let Some(data) = line.strip_prefix("data: ") {
                    let streamed: Value = serde_json::from_str(data).unwrap();
                    assert_eq!(streamed, polled["notices"][0]);
                    assert_eq!(event_id, streamed["seq"].as_u64());
                    break;
                }
            }
            drop(stream);
            let filtered =
                json_ok(server.get(&format!("/api/leases/notices?after={cursor}&lease_id={id}")));
            let notices = filtered["notices"].as_array().unwrap();
            assert_eq!(
                notices
                    .iter()
                    .map(|n| n["kind"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["overlap", "released"]
            );
            assert!(
                notices
                    .windows(2)
                    .all(|rows| rows[0]["seq"].as_u64() < rows[1]["seq"].as_u64())
            );
            assert_eq!(server.get("/api/leases/notices?after=-1").status(), 400);
            assert_eq!(
                server
                    .request("GET", "/api/leases/events")
                    .header("last-event-id", "invalid")
                    .send()
                    .unwrap()
                    .status(),
                400
            );
            assert_eq!(server.get("/api/leases/notices?limit=0").status(), 400);
            let reopened = fixture.server(false);
            assert_eq!(
                json_ok(reopened.get(&format!("/api/leases/notices?after={cursor}&lease_id={id}"))),
                filtered
            );
        },
    );
}

#[test]
fn lease_routes_share_runtime_state_and_gate_writes() {
    isolated(
        "leases::lease_routes_share_runtime_state_and_gate_writes",
        || {
            let fixture = Fixture::new();
            std::fs::write(
                fixture.work.join("config.toml"),
                "[lease]\nenabled = true\n[lease.hard]\nagent = \"allow\"\n",
            )
            .unwrap();
            std::fs::write(fixture.work.parent().unwrap().join("held.txt"), "held\n").unwrap();
            let runtime =
                orbit_core::OrbitRuntime::from_roots(&fixture.global, &fixture.work).unwrap();
            let operator = fixture.server(true);
            let read_only = fixture.server(false);
            let request = json!({"principal": "alice", "selectors": ["file:held.txt"], "mode": "hard", "ttl_seconds": 600});
            let response = operator.send("POST", "/api/leases", request.clone());
            assert_eq!(
                response.status(),
                200,
                "lease request route must be mounted"
            );
            let granted = json_ok(response);
            assert_eq!(granted["granted"], true);
            assert_eq!(granted["lease"]["mode"], "hard");
            assert_eq!(granted["lease"]["principal"], "alice");
            let id = granted["lease"]["lease_id"].as_str().unwrap();
            assert_eq!(
                json_ok(operator.get(&format!("/api/leases/{id}"))),
                lease::show(&runtime, json!({"lease_id": id})).unwrap()
            );
            assert_eq!(
                json_ok(read_only.get("/api/leases")),
                lease::list(&runtime).unwrap()
            );
            let refused = json_ok(operator.send(
                "POST",
                "/api/leases",
                json!({"principal": "bob", "selectors": ["file:held.txt"], "mode": "hard"}),
            ));
            assert_eq!(refused["granted"], false);
            assert_eq!(
                refused,
                lease::request(
                    &runtime,
                    json!({"principal": "bob", "selectors": ["file:held.txt"], "mode": "hard"}),
                    None,
                    None
                )
                .unwrap()
            );
            assert!(
                refused["conflicts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|holder| holder["held_by_id"] == id)
            );
            let check = json!({"paths": ["file:held.txt"], "lease_id": id});
            assert_eq!(
                json_ok(read_only.send("POST", "/api/leases/check", check.clone())),
                lease::check(&runtime, check).unwrap()
            );
            for (method, path) in [
                ("POST", "/api/leases".to_string()),
                ("POST", format!("/api/leases/{id}/renew")),
                ("POST", format!("/api/leases/{id}/expand")),
                ("DELETE", format!("/api/leases/{id}")),
            ] {
                error_code(
                    read_only.send(method, &path, request.clone()),
                    403,
                    "authorization_denied",
                );
            }
            assert_eq!(
                operator
                    .request("POST", "/api/leases")
                    .json(&request)
                    .send()
                    .unwrap()
                    .status(),
                403
            );
            let renewed = json_ok(operator.send(
                "POST",
                &format!("/api/leases/{id}/renew"),
                json!({"ttl_seconds": 1200, "lease_id": "lease-wrong"}),
            ));
            assert_eq!(renewed["renewed"], true);
            assert_eq!(
                renewed["lease"],
                lease::show(&runtime, json!({"lease_id": id})).unwrap()["lease"]
            );
            error_code(
                operator.send("POST", &format!("/api/leases/{id}/expand"), json!({})),
                501,
                "not_available_in_this_phase",
            );
            error_code(
                operator
                    .request("POST", &format!("/api/leases/{id}/expand"))
                    .header("origin", &operator.origin)
                    .send()
                    .unwrap(),
                501,
                "not_available_in_this_phase",
            );
            let default_renewal = json_ok(
                operator
                    .request("POST", &format!("/api/leases/{id}/renew"))
                    .header("origin", &operator.origin)
                    .send()
                    .unwrap(),
            );
            assert_eq!(default_renewal["renewed"], true);
            let empty_json_renewal = json_ok(
                operator
                    .request("POST", &format!("/api/leases/{id}/renew"))
                    .header("origin", &operator.origin)
                    .header("content-type", "application/json")
                    .send()
                    .unwrap(),
            );
            assert_eq!(empty_json_renewal["renewed"], true);
            assert_eq!(empty_json_renewal["lease"]["lease_id"], id);
            error_code(
                operator
                    .request("POST", &format!("/api/leases/{id}/expand"))
                    .header("origin", &operator.origin)
                    .header("content-type", "application/json")
                    .send()
                    .unwrap(),
                501,
                "not_available_in_this_phase",
            );
            for path in [
                format!("/api/leases/{id}/renew"),
                format!("/api/leases/{id}/expand"),
            ] {
                let malformed = operator
                    .request("POST", &path)
                    .header("origin", &operator.origin)
                    .header("content-type", "application/json")
                    .body("{\"ttl_seconds\":")
                    .send()
                    .unwrap();
                assert_eq!(malformed.status(), 400, "{path}");
            }
            let released = json_ok(operator.send(
                "DELETE",
                &format!("/api/leases/{id}?principal=alice"),
                Value::Null,
            ));
            assert_eq!(released["released"], true);
            assert_eq!(
                released["lease"],
                lease::show(&runtime, json!({"lease_id": id})).unwrap()["lease"]
            );
            assert_eq!(
                json_ok(operator.send("DELETE", &format!("/api/leases/{id}"), Value::Null)),
                lease::release(&runtime, json!({"lease_id": id}), None, None).unwrap()
            );
            assert_eq!(json_ok(operator.get("/api/leases")), json!({"leases": []}));
            assert_eq!(operator.get("/api/leases?workspace=missing").status(), 404);
            assert_eq!(
                operator.send("POST", "/api/leases", Value::Null).status(),
                400
            );
            assert_eq!(
                operator
                    .send("POST", "/api/leases?principal=bob", request)
                    .status(),
                400
            );
            check_standalone_routes();
        },
    );
}

// A transport probe, not a second runtime adapter: checks the exported API a
// listener can use without depending on dashboard state or localhost headers.
struct ProbeBackend;

macro_rules! probe_lease_verb {
    ($verb:ident) => {
        fn $verb(
            &self,
            caller: &orbit_web::LeaseCaller,
            input: Value,
        ) -> orbit_web::LeaseBackendResult {
            Ok(json!({
                "verb": stringify!($verb),
                "principal": caller.principal,
                "workspace": caller.workspace,
                "input": input,
            }))
        }
    };
}

impl orbit_web::LeaseBackend for ProbeBackend {
    probe_lease_verb!(request);
    probe_lease_verb!(renew);
    probe_lease_verb!(expand);
    probe_lease_verb!(release);
    probe_lease_verb!(show);
    probe_lease_verb!(check);

    fn list(&self, caller: &orbit_web::LeaseCaller) -> orbit_web::LeaseBackendResult {
        Ok(json!({"verb": "list", "principal": caller.principal, "workspace": caller.workspace}))
    }
}

fn check_standalone_routes() {
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let router = axum::Router::new().nest("/api", orbit_web::lease_router(ProbeBackend));
            for (method, path, verb) in [
                ("POST", "/api/leases", "request"),
                ("GET", "/api/leases", "list"),
                ("GET", "/api/leases/lease-url", "show"),
                ("DELETE", "/api/leases/lease-url", "release"),
                ("POST", "/api/leases/lease-url/renew", "renew"),
                ("POST", "/api/leases/lease-url/expand", "expand"),
                ("POST", "/api/leases/check", "check"),
            ] {
                let response = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(format!(
                                "{path}?principal=alice&workspace=listener-workspace"
                            ))
                            .header("content-type", "application/json")
                            .body(Body::from(
                                json!({"principal": "alice", "lease_id": "lease-body"}).to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), 200, "{method} {path}");
                let body: Value =
                    serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap())
                        .unwrap();
                assert_eq!(body["verb"], verb);
                assert_eq!(body["principal"], "alice");
                assert_eq!(body["workspace"], "listener-workspace");
                if path.contains("lease-url") {
                    assert_eq!(body["input"]["lease_id"], "lease-url");
                }
            }
            for path in ["/api/tasks", "/api/config/effective", "/healthz"] {
                let response = router
                    .clone()
                    .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    404,
                    "lease-only router must exclude {path}"
                );
            }
        });
}
