//! Consent schema v2 at the `orbit application consent` boundary: version
//! selection by content, consent-time refusals and the host expiry cap.
use super::Fixture;
use super::application_authority::{app_command, consent};
use chrono::{Duration, SecondsFormat, Utc};
use serde_json::{Value, json};

fn expires_in(days: i64) -> String {
    (Utc::now() + Duration::days(days)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A built-in dismiss scope: reject a review-state approval task with a comment.
fn dismiss_scope() -> Value {
    json!({
        "name": "approval-dismiss",
        "operation": {
            "tool": "orbit.task.update",
            "plugin": null,
            "fields": {
                "id": {"task": {"tags_all": ["approval"], "status": ["review"]}},
                "status": {"const": "rejected"},
                "comment": {"text": {"max_chars": 512}}
            }
        },
        "callbacks": []
    })
}

fn request(scopes: Value, days: i64) -> Value {
    json!({"application_id": "office-app", "expires_at": expires_in(days), "scopes": scopes})
}

/// Run `orbit application consent` expecting a refusal; returns stderr.
fn refused(fixture: &Fixture, request: &Value, suffix: &str) -> String {
    let input = fixture.home.join(format!("refused-{suffix}.json"));
    std::fs::write(&input, request.to_string()).unwrap();
    let output = fixture
        .operator_command(["application", "consent", "--input-file"])
        .arg(&input)
        .arg("--credential-file")
        .arg(fixture.home.join(format!("refused-credential-{suffix}")))
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        !fixture
            .home
            .join(format!("refused-credential-{suffix}"))
            .exists(),
        "a refused consent must not provision a credential"
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn application_consent_writes_v2_for_constrained_scopes_and_v1_for_exact_ones() {
    let fixture = Fixture::new();
    fixture.install_shapes();
    fixture.success(["plugin", "enable", "shapes"]);
    let show = fixture.json(["plugin", "show", "shapes"]);
    let digest = show["manifest_digest"].as_str().unwrap();
    let plugin = json!({"name": "shapes", "manifest_digest": digest});
    let constrained = request(
        json!([
            dismiss_scope(),
            {
                "name": "maintain",
                "operation": {
                    "tool": "shapes.maintain",
                    "plugin": plugin,
                    "fields": {"force": {"optional": {"one_of": [true, false]}}}
                },
                "callbacks": [{
                    "tool": "orbit.task.update",
                    "plugin": null,
                    "fields": {
                        "id": {"task": {"tags_all": ["proposal"], "status": ["proposed"], "pin": true}},
                        "status": {"transition": {"from": ["review", "in-progress"], "to": "backlog"}},
                        "execution_summary": {"text": {"max_chars": 4096}}
                    }
                }],
                "rate_limit": {"admissions": 30, "window_seconds": 3600}
            }
        ]),
        20,
    );
    let (grant, credential) = consent(&fixture, &constrained, "v2", None);
    assert_eq!(grant["schema_version"], 2);
    assert_eq!(grant["scopes"][1]["rate_limit"]["admissions"], 30);
    // A scope without a rate limit records none: no limit applies.
    assert!(grant["scopes"][0].get("rate_limit").is_none());

    // A constrained scope is evaluated on current state: no such task here.
    let dismiss = json!({"id": "TST-1", "status": "rejected", "comment": "not needed"});
    let output = app_command(&fixture, &credential, "orbit.task.update", &dismiss)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("scope `approval-dismiss` field `id` failed `task`"),
        "a constrained scope must refuse a call its predicate does not admit: {stderr}"
    );

    // Every scope exact, unnamed and unlimited: still the v1 record a
    // rolled-back binary honors, and outside the constrained expiry cap.
    let exact = request(
        json!([{
            "operation": {"tool": "orbit.task.update", "plugin": null,
                          "input": {"id": "TST-1", "status": "rejected"}},
            "callbacks": []
        }]),
        400,
    );
    let (grant, _) = consent(&fixture, &exact, "v1", None);
    assert_eq!(grant["schema_version"], 1);
}

#[test]
fn application_consent_refuses_an_invalid_constraint() {
    let fixture = Fixture::new();
    let mut scope = dismiss_scope();
    scope["operation"]["fields"]["comment"] = json!({"text": {"max_chars": 0}});
    let stderr = refused(&fixture, &request(json!([scope]), 7), "invalid-text");
    assert!(stderr.contains("max_chars"), "{stderr}");
    // A predicate that would match every task is refused too.
    let mut scope = dismiss_scope();
    scope["operation"]["fields"]["id"] = json!({"task": {"status": ["review"]}});
    let stderr = refused(&fixture, &request(json!([scope]), 7), "match-all");
    assert!(stderr.contains("tags_all"), "{stderr}");
    // An identity field never binds to free text.
    let mut scope = dismiss_scope();
    scope["operation"]["fields"]["id"] = json!({"text": {"max_chars": 32}});
    let stderr = refused(&fixture, &request(json!([scope]), 7), "identity-text");
    assert!(stderr.contains("identity field"), "{stderr}");
}

#[test]
fn application_consent_refuses_fields_and_input_together() {
    let fixture = Fixture::new();
    let mut scope = dismiss_scope();
    scope["operation"]["input"] = json!({"id": "TST-1", "status": "rejected"});
    let stderr = refused(&fixture, &request(json!([scope]), 7), "both-forms");
    assert!(
        stderr.contains("exactly one of `input` or `fields`"),
        "{stderr}"
    );
}

#[test]
fn application_consent_refuses_a_ceiling_breach() {
    let fixture = Fixture::new();
    let mut scope = dismiss_scope();
    scope["operation"]["fields"]["crew"] = json!({"const": "opus"});
    let stderr = refused(&fixture, &request(json!([scope]), 7), "ceiling-field");
    assert!(
        stderr.contains("exceeds the application ceiling"),
        "{stderr}"
    );
    // Approving work for dispatch is never application authority.
    let mut scope = dismiss_scope();
    scope["operation"]["fields"]["status"] =
        json!({"transition": {"from": ["proposed"], "to": "backlog"}});
    let stderr = refused(&fixture, &request(json!([scope]), 7), "ceiling-backlog");
    assert!(stderr.contains("`backlog` is reachable only"), "{stderr}");
}

#[test]
fn application_consent_refuses_an_always_refused_tool() {
    let fixture = Fixture::new();
    let routine = json!({
        "name": "routine-toggle",
        "operation": {"tool": "orbit.routine.control", "plugin": null,
                      "fields": {"id": {"const": "nightly"}}},
        "callbacks": []
    });
    let stderr = refused(&fixture, &request(json!([routine]), 7), "routine");
    assert!(stderr.contains("never application authority"), "{stderr}");
    // An input-dependent Operator floor refuses an otherwise ceilinged tool.
    let grant = json!([{
        "operation": {"tool": "orbit.task.update", "plugin": null,
                      "input": {"id": "TST-1", "granted_tools": ["orbit.workflow.ship"]}},
        "callbacks": []
    }]);
    let stderr = refused(&fixture, &request(grant, 7), "granted-tools");
    assert!(stderr.contains("never application authority"), "{stderr}");
}

#[test]
fn application_consent_refuses_an_operation_over_8_kib() {
    let fixture = Fixture::new();
    let mut scope = dismiss_scope();
    scope["operation"]["fields"]["id"] = json!({"const": "x".repeat(9 * 1024)});
    let stderr = refused(&fixture, &request(json!([scope]), 7), "budget");
    assert!(stderr.contains("8 KiB per-operation budget"), "{stderr}");
}

#[test]
fn application_consent_expiry_cap_defaults_to_30_days_and_follows_host_config() {
    let fixture = Fixture::new();
    let beyond_default = request(json!([dismiss_scope()]), 45);
    let stderr = refused(&fixture, &beyond_default, "cap-default");
    assert!(stderr.contains("within 30 days"), "{stderr}");
    let (grant, _) = consent(
        &fixture,
        &request(json!([dismiss_scope()]), 29),
        "cap-ok",
        None,
    );
    assert_eq!(grant["schema_version"], 2);

    fixture
        .operator_command([
            "config",
            "set",
            "--global",
            "machine.application_consent_max_days",
            "60",
        ])
        .assert()
        .success();
    let (grant, _) = consent(&fixture, &beyond_default, "cap-raised", None);
    assert_eq!(grant["schema_version"], 2);
    let stderr = refused(
        &fixture,
        &request(json!([dismiss_scope()]), 90),
        "cap-raised-over",
    );
    assert!(stderr.contains("within 60 days"), "{stderr}");
}

#[test]
fn application_consent_validates_an_optional_rate_limit() {
    let fixture = Fixture::new();
    let mut scope = dismiss_scope();
    scope["rate_limit"] = json!({"admissions": 0, "window_seconds": 3600});
    let stderr = refused(&fixture, &request(json!([scope]), 7), "rate-zero");
    assert!(stderr.contains("rate limit"), "{stderr}");
    let mut scope = dismiss_scope();
    scope["rate_limit"] = json!({"admissions": 5, "window_seconds": 999_999});
    let stderr = refused(&fixture, &request(json!([scope]), 7), "rate-window");
    assert!(stderr.contains("rate limit"), "{stderr}");
}
