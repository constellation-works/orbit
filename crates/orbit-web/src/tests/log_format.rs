use crate::log_format::{format_message_html, render_log_event_for_web};
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use serde_json::json;

#[test]
fn format_message_html_escapes_dynamic_field_values() {
    let html = format_message_html(
        "orbit.friction.reported",
        &json!({
            "task_id": "<script>alert(1)</script>",
            "agent": "codex",
            "model": TEST_CODEX_MODEL,
            "summary": "bad <b>markup</b>"
        }),
    );

    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(html.contains("bad &lt;b&gt;markup&lt;/b&gt;"));
    assert!(!html.contains("<script>"));
}

/// Secret-looking tokens in log fields are redacted before HTML escaping,
/// for the generic field renderer and the CLI-runner line renderer alike.
#[test]
fn rendered_log_redacts_secret_tokens_in_lines_and_fields() {
    let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
    let runner = render_log_event_for_web(&json!({
        "timestamp": "2026-04-27T01:00:03Z",
        "level": "INFO",
        "target": "orbit_engine::activity_job::cli_runner",
        "fields": {"stream": "stderr", "line": format!("using key {secret} now")}
    }));
    let generic = render_log_event_for_web(&json!({
        "timestamp": "2026-04-27T01:00:03Z",
        "level": "INFO",
        "target": "orbit.other",
        "fields": {"message": format!("token {secret}"), "detail": {"header": "Authorization: Bearer abc123def456"}}
    }));

    for (label, rendered) in [("cli_runner line", runner), ("generic fields", generic)] {
        assert!(
            !rendered.message_html.contains(secret)
                && !rendered.message_html.contains("abc123def456"),
            "{label} leaked a secret: {}",
            rendered.message_html
        );
        assert!(
            rendered.message_html.contains("REDACTED"),
            "{label} should show a redaction marker: {}",
            rendered.message_html
        );
    }
}
