//! Analytics › Experiments authoring routes against the real server: only an
//! operator session may create, edit or start an experiment. Each write runs
//! core's lifecycle function, so core's refusals come back with core's text
//! and every write records one event with actor `dashboard` and the reason.

use std::fs;

use serde_json::{Value, json};

use super::support::{Fixture, error_code, isolated, json_ok};

const LIST: &str = "/api/diagnostics/experiments?workspace=ws_http_fixture";

fn route(path: &str) -> String {
    format!("/api/diagnostics/experiments{path}?workspace=ws_http_fixture")
}

fn draft() -> Value {
    json!({
        "name": "trial",
        "hypothesis": "A second arm changes nothing measurable.",
        "arms": [
            { "name": "control", "control": true },
            { "name": "treatment", "weight": 2 },
        ],
        "population": { "complexity": ["medium"] },
        "reason": "first draft",
    })
}

fn refusal(response: reqwest::blocking::Response, status: u16) -> String {
    let actual = response.status().as_u16();
    let body: Value = response.json().expect("refusal JSON");
    assert_eq!(actual, status, "{body}");
    body["error"].as_str().expect("refusal text").to_string()
}

#[test]
fn experiment_authoring_routes_are_operator_only_and_record_core_events() {
    isolated(
        "experiments::experiment_authoring_routes_are_operator_only_and_record_core_events",
        || {
            let fixture = Fixture::new();
            {
                let agent = fixture.server(false);
                let list = json_ok(agent.get(LIST));
                assert_eq!(
                    list["authoring"]["create"]["authorized"],
                    json!(false),
                    "{list}"
                );
                for (method, path) in [
                    ("POST", route("")),
                    ("PATCH", route("/trial")),
                    ("POST", route("/trial/start")),
                ] {
                    let denied = error_code(
                        agent.send(method, &path, draft()),
                        403,
                        "authorization_denied",
                    );
                    assert!(
                        denied["operation"]
                            .as_str()
                            .is_some_and(|id| id.starts_with("experiment.")),
                        "{method} {path}: {denied}"
                    );
                }
                assert_eq!(json_ok(agent.get(LIST))["experiments"], json!([]));
            }

            let server = fixture.server(true);
            let list = json_ok(server.get(LIST));
            for action in ["create", "update", "start"] {
                assert_eq!(
                    list["authoring"][action]["authorized"],
                    json!(true),
                    "{list}"
                );
            }
            assert!(
                list["authoring"]["metrics"]
                    .as_array()
                    .is_some_and(|metrics| metrics.contains(&json!("first_pass_acceptance"))),
                "{list}"
            );

            let created = json_ok(server.send("POST", &route(""), draft()));
            assert_eq!(created["status"], json!("draft"), "{created}");
            assert_eq!(created["arms"][1]["weight"], json!(2), "{created}");
            let id = created["id"].as_str().expect("experiment id").to_string();
            let one = route(&format!("/{id}"));
            let start = route(&format!("/{id}/start"));

            let unknown = refusal(
                server.send("PATCH", &one, json!({ "primary_metric": "speed" })),
                400,
            );
            assert!(
                unknown
                    .starts_with("`primary_metric` names unknown metric `speed`; expected one of "),
                "core's refusal text: {unknown}"
            );
            let updated = json_ok(server.send(
                "PATCH",
                &one,
                json!({ "primary_metric": "first_pass_acceptance", "reason": "pick a metric" }),
            ));
            assert_eq!(updated["primary_metric"], json!("first_pass_acceptance"));

            let disabled = server.send("POST", &start, json!({ "reason": "go" }));
            assert_eq!(disabled.status().as_u16(), 409);
            let disabled: Value = disabled.json().unwrap();
            assert_eq!(
                disabled["error"],
                json!(
                    "experiment `trial` cannot start: experiments are disabled in this workspace (`experiments.enabled` is false)"
                ),
                "{disabled}"
            );

            let config = fixture.work.join("config.toml");
            let mut text = fs::read_to_string(&config).unwrap_or_default();
            text.push_str("\n[experiments]\nenabled = true\n");
            fs::write(&config, text).unwrap();
            let started = json_ok(server.send("POST", &start, json!({ "reason": "go" })));
            assert_eq!(started["status"], json!("running"), "{started}");

            let locked = refusal(
                server.send("PATCH", &one, json!({ "hypothesis": "x" })),
                400,
            );
            assert!(
                locked.starts_with("experiment `trial` is running: its arms, overlays, population, metrics and sample rules are editable only in draft"),
                "core's refusal text: {locked}"
            );

            let shown = json_ok(server.get(&one));
            let events = shown["events"].as_array().expect("events");
            let summary = events
                .iter()
                .map(|event| {
                    (
                        event["kind"].as_str().unwrap_or_default(),
                        event["actor"].as_str().unwrap_or_default(),
                        event["reason"].as_str().unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                summary,
                [
                    ("created", "dashboard", "first draft"),
                    ("updated", "dashboard", "pick a metric"),
                    ("started", "dashboard", "go"),
                ],
                "one event per accepted write, refusals record none: {shown}"
            );
        },
    );
}
