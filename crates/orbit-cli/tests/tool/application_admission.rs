//! Constrained application admission at the `orbit tool run` boundary: a
//! direct dismiss or send-back is admitted on the consented task state and
//! refused, naming the failing constraint, on anything else.
use super::Fixture;
use super::application_authority::{app_command, consent};
use chrono::{Duration, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::path::Path;

fn expires_in(days: i64) -> String {
    (Utc::now() + Duration::days(days)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Reject a review-state task carrying `tag`, with a comment.
fn dismiss_scope(name: &str, tag: &str) -> Value {
    json!({
        "name": name,
        "operation": {
            "tool": "orbit.task.update",
            "plugin": null,
            "fields": {
                "id": {"task": {"tags_all": [tag], "status": ["review"]}},
                "status": {"const": "rejected"},
                "comment": {"text": {"max_chars": 512}}
            }
        },
        "callbacks": []
    })
}

/// Send an approval task back to `backlog`. The predicate admits `proposed`
/// too, so only the `transition` constraint stands between a proposed task
/// and dispatch.
fn send_back_scope() -> Value {
    json!({
        "name": "approval-send-back",
        "operation": {
            "tool": "orbit.task.update",
            "plugin": null,
            "fields": {
                "id": {"task": {"tags_all": ["approval"], "status": ["review", "proposed"]}},
                "status": {"transition": {"from": ["review"], "to": "backlog"}},
                "comment": {"text": {"max_chars": 512}}
            }
        },
        "callbacks": []
    })
}

fn grant(fixture: &Fixture, scopes: Value, suffix: &str) -> (Value, std::path::PathBuf) {
    let request = json!({
        "application_id": "office-app",
        "expires_at": expires_in(7),
        "scopes": scopes,
    });
    consent(fixture, &request, suffix, None)
}

/// A task tagged `tag` in `status`.
fn task(fixture: &Fixture, title: &str, tag: &str, status: &str) -> String {
    let task = fixture.json([
        "task",
        "add",
        "--title",
        title,
        "--complexity",
        "low",
        "--description",
        "Application admission target.",
        "--tag",
        tag,
    ]);
    let id = task["id"].as_str().unwrap().to_owned();
    if task["status"] != status {
        fixture
            .operator_command(["task", "update", &id, "--status", status, "--force"])
            .assert()
            .success();
    }
    id
}

fn status(fixture: &Fixture, id: &str) -> String {
    fixture.json(["task", "show", id])["status"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn admitted(fixture: &Fixture, credential: &Path, input: &Value) {
    app_command(fixture, credential, "orbit.task.update", input)
        .assert()
        .success();
}

/// Run a call expecting a refusal; returns stderr.
fn refused(fixture: &Fixture, credential: &Path, input: &Value) -> String {
    let output = app_command(fixture, credential, "orbit.task.update", input)
        .assert()
        .failure()
        .get_output()
        .clone();
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn application_rows(fixture: &Fixture) -> Vec<(String, Value)> {
    let conn = rusqlite::Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
    conn.prepare(
        "SELECT status, application_json FROM audit_events \
         WHERE tool_name = 'orbit.task.update' AND application_json IS NOT NULL",
    )
    .unwrap()
    .query_map([], |row| {
        let raw: String = row.get(1)?;
        Ok((row.get(0)?, serde_json::from_str(&raw).unwrap()))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn application_admission_admits_dismiss_and_send_back_on_consented_state() {
    let fixture = Fixture::new();
    let (_, credential) = grant(
        &fixture,
        json!([
            dismiss_scope("approval-dismiss", "approval"),
            send_back_scope()
        ]),
        "admit",
    );
    let dismissed = task(&fixture, "Dismiss me", "approval", "review");
    let comment = "Not needed after all";
    admitted(
        &fixture,
        &credential,
        &json!({"id": dismissed, "status": "rejected", "comment": comment}),
    );
    assert_eq!(status(&fixture, &dismissed), "rejected");

    let sent_back = task(&fixture, "Send me back", "approval", "review");
    admitted(
        &fixture,
        &credential,
        &json!({"id": sent_back, "status": "backlog", "comment": "Needs another pass"}),
    );
    assert_eq!(status(&fixture, &sent_back), "backlog");
    let shown = fixture.json(["task", "show", &sent_back]).to_string();
    assert!(shown.contains("Needs another pass"));
    assert!(shown.contains("application:office-app"));

    // The admitted invocation records the bound id and the comment's digest,
    // never the comment text.
    let rows = application_rows(&fixture);
    let dismiss_row = rows
        .iter()
        .find(|(status, app)| status == "success" && app["bindings"]["id"] == dismissed.as_str())
        .expect("admitted dismiss is audited with its bindings");
    assert_eq!(dismiss_row.1["scope_index"], 0);
    assert_eq!(
        dismiss_row.1["text_digests"]["comment"]["chars"],
        comment.len()
    );
    assert_eq!(dismiss_row.1["predicate"]["status_seen"], "review");
    assert!(!dismiss_row.1.to_string().contains(comment));
    assert!(
        rows.iter()
            .any(|(status, app)| status == "success" && app["scope_index"] == 1)
    );
}

#[test]
fn application_admission_refuses_changed_state_wrong_tag_proposed_backlog_and_revocation() {
    let fixture = Fixture::new();
    let (grant, credential) = grant(
        &fixture,
        json!([
            dismiss_scope("approval-dismiss", "approval"),
            send_back_scope()
        ]),
        "refuse",
    );
    let marker = "payload-marker-7f3a";

    // Changed state: the first dismiss moved the task, so a replay is refused.
    let id = task(&fixture, "Dismissed once", "approval", "review");
    let dismiss = json!({"id": id, "status": "rejected", "comment": marker});
    admitted(&fixture, &credential, &dismiss);
    let stderr = refused(&fixture, &credential, &dismiss);
    assert!(
        stderr.contains("scope `approval-dismiss` field `id` failed `task`"),
        "{stderr}"
    );
    assert!(
        !stderr.contains(marker) && !stderr.contains(&id),
        "a refusal must not echo call values: {stderr}"
    );

    // Wrong tag.
    let other = task(&fixture, "Not an approval", "unrelated", "review");
    let stderr = refused(
        &fixture,
        &credential,
        &json!({"id": other, "status": "rejected", "comment": marker}),
    );
    assert!(
        stderr.contains("scope `approval-dismiss` field `id` failed `task`"),
        "{stderr}"
    );
    assert!(!stderr.contains(marker) && !stderr.contains(&other));
    assert_eq!(status(&fixture, &other), "review");

    // `proposed` to `backlog` is approval for dispatch, never admitted.
    let proposed = task(&fixture, "Still proposed", "approval", "proposed");
    let stderr = refused(
        &fixture,
        &credential,
        &json!({"id": proposed, "status": "backlog", "comment": marker}),
    );
    assert!(
        stderr.contains("scope `approval-send-back` field `status` failed `transition`"),
        "{stderr}"
    );
    assert!(!stderr.contains(marker) && !stderr.contains(&proposed));
    assert_eq!(status(&fixture, &proposed), "proposed");

    // After revocation a call on matching state is refused.
    let pending = task(&fixture, "Pending send-back", "approval", "review");
    fixture
        .operator_command([
            "application",
            "revoke",
            grant["consent_id"].as_str().unwrap(),
        ])
        .assert()
        .success();
    refused(
        &fixture,
        &credential,
        &json!({"id": pending, "status": "backlog", "comment": "too late"}),
    );
    assert_eq!(status(&fixture, &pending), "review");

    // Every refusal is audited without the call's text.
    let denied: Vec<_> = application_rows(&fixture)
        .into_iter()
        .filter(|(status, _)| status == "denied")
        .collect();
    assert!(denied.len() >= 4, "{denied:?}");
}

#[test]
fn application_admission_enforces_a_scope_rate_limit_and_leaves_others_unlimited() {
    let fixture = Fixture::new();
    let mut limited = dismiss_scope("limited-dismiss", "limited");
    limited["rate_limit"] = json!({"admissions": 2, "window_seconds": 3600});
    let (_, credential) = grant(
        &fixture,
        json!([limited, dismiss_scope("open-dismiss", "open")]),
        "rate",
    );
    let dismiss = |id: &str| json!({"id": id, "status": "rejected", "comment": "dismissed"});

    for n in 0..2 {
        let id = task(&fixture, &format!("Limited {n}"), "limited", "review");
        admitted(&fixture, &credential, &dismiss(&id));
    }
    let over = task(&fixture, "Limited over", "limited", "review");
    let stderr = refused(&fixture, &credential, &dismiss(&over));
    assert!(
        stderr.contains("application scope `limited-dismiss` reached its rate limit"),
        "{stderr}"
    );
    assert_eq!(status(&fixture, &over), "review");

    // A scope without a rate limit is not limited.
    for n in 0..4 {
        let id = task(&fixture, &format!("Open {n}"), "open", "review");
        admitted(&fixture, &credential, &dismiss(&id));
        assert_eq!(status(&fixture, &id), "rejected");
    }

    // The refused admission is audited.
    assert!(
        application_rows(&fixture)
            .iter()
            .any(|(status, app)| status == "denied"
                && app["consent"]["scopes"][0]["name"] == "limited-dismiss")
    );
}
