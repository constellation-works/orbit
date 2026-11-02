//! The Automation › Scripts view (script steps phase 4): every session reads
//! each script's grant state and open request; only an operator session
//! grants, bound to the digest it reviewed, or answers a `script_grant`
//! request.

use std::fs;

use serde_json::json;

use super::support::{Fixture, isolated, json_ok};

const WS: &str = "?workspace=ws_http_fixture";

fn script_fixture(fixture: &Fixture) {
    let repo = fixture.work.parent().unwrap();
    fs::create_dir_all(fixture.work.join("resources/scripts")).unwrap();
    fs::create_dir_all(repo.join("tools")).unwrap();
    fs::write(repo.join("tools/report.sh"), "#!/bin/sh\necho '{}'\n").unwrap();
    fs::write(
        fixture.work.join("resources/scripts/report.yaml"),
        "schemaVersion: 1\nkind: Script\nmetadata:\n  name: report\nspec:\n  description: Fixture report.\n  entry: tools/report.sh\n",
    )
    .unwrap();
    fs::write(
        repo.join("sweep.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: sweep\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: run\n      target: script:report\n",
    )
    .unwrap();
    // The refused step raises the open grant request the view shows.
    let _ = fixture
        .runtime
        .run_job_v2_from_yaml(&repo.join("sweep.yaml"), json!({}));
}

#[test]
fn script_grants_need_the_operator_session_and_the_reviewed_digest() {
    isolated(
        "scripts::script_grants_need_the_operator_session_and_the_reviewed_digest",
        || {
            let fixture = Fixture::new();
            script_fixture(&fixture);
            let grant_path = format!("/api/scripts/report/grant{WS}");

            let anon = fixture.server(false);
            let listed = json_ok(anon.get(&format!("/api/scripts{WS}")));
            let script = &listed["scripts"][0];
            assert_eq!(script["name"], "report", "{listed}");
            assert_eq!(script["state"], "ungranted", "{listed}");
            assert_eq!(
                listed["capabilities"]["grant"]["authorized"], false,
                "{listed}"
            );
            let digest = script["current_digest"].as_str().unwrap().to_string();
            let request = script["open_request"]["id"]
                .as_str()
                .expect("the refusal raised an open request")
                .to_string();
            assert!(
                script["diff"].as_str().unwrap().contains("+echo '{}'"),
                "{listed}"
            );

            // Without the operator session neither path writes a grant.
            let denied = anon.send("POST", &grant_path, json!({"digest": digest}));
            assert_eq!(denied.status().as_u16(), 403);
            let answered = anon.send(
                "POST",
                &format!("/api/decisions/{request}/answer{WS}"),
                json!({"answer": "grant"}),
            );
            assert_eq!(answered.status().as_u16(), 403);
            assert_eq!(
                answered.json::<serde_json::Value>().unwrap()["code"],
                "authorization_denied"
            );
            assert!(
                fixture
                    .runtime
                    .verified_script_grant("report")
                    .unwrap()
                    .is_none()
            );

            // The operator session grants only the digest it reviewed.
            let operator = fixture.server(true);
            let stale = operator.send("POST", &grant_path, json!({"digest": "0".repeat(64)}));
            assert!(
                stale.status().is_client_error(),
                "a stale review is refused"
            );
            assert!(
                fixture
                    .runtime
                    .verified_script_grant("report")
                    .unwrap()
                    .is_none()
            );
            let granted = json_ok(operator.send("POST", &grant_path, json!({"digest": digest})));
            assert_eq!(granted["grant"]["digest"], json!(digest), "{granted}");
            assert_eq!(granted["grant"]["provenance"], "dashboard", "{granted}");

            let listed = json_ok(operator.get(&format!("/api/scripts{WS}")));
            assert_eq!(listed["scripts"][0]["state"], "granted", "{listed}");
            assert!(listed["scripts"][0]["open_request"].is_null(), "{listed}");
        },
    );
}
