use crate::log_format::{format_message_html, render_log_event_for_web};
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use serde_json::json;

// Inspect rendered output, not source text. The renderer's escaped attribute
// values cannot contain literal angle brackets.
fn visible_text(html: &str) -> String {
    let mut in_tag = false;
    html.chars()
        .filter(|ch| match ch {
            '<' => {
                in_tag = true;
                false
            }
            '>' => {
                in_tag = false;
                false
            }
            _ => !in_tag,
        })
        .collect()
}

/// Combinatorial provider-line parsing: structured, plain, malformed and
/// non-string event kinds must all leave meaning ahead of bulky context.
#[test]
fn rendered_agent_relays_put_kind_and_run_in_first_60_characters() {
    for (line, stream, kind) in [
        (
            r#"{"type":"item.started","item":{"type":"command_execution"}}"#,
            "stdout",
            "item.started",
        ),
        (
            r#"{"type":"assistant","message":{"content":[]}}"#,
            "stdout",
            "assistant",
        ),
        ("plain agent diagnostic", "stderr", "agent stderr"),
        ("{incomplete", "stdout", "agent stdout"),
        (r#"{"type":false}"#, "stdout", "agent stdout"),
    ] {
        let target = "orbit_engine::activity_job::cli_runner::supervisor";
        let rendered = render_log_event_for_web(&json!({
            "target": target,
            "fields": {
                "cwd": "/srv/project/.orbit/state/worktrees/orbit-jrun-20261007-0722-c12",
                "job_run_id": "jrun-20261007-0722-c12",
                "provider": "codex", "stream": stream, "line": line,
            }
        }));
        let first: String = visible_text(&rendered.message_html)
            .chars()
            .take(60)
            .collect();
        assert!(
            first.starts_with(kind) && first.contains("jrun-…-c12"),
            "relay meaning must lead the dock and status bar: {first}"
        );
        assert!(
            !first.starts_with("cwd="),
            "relay context must follow its meaning: {first}"
        );
        let text = visible_text(&rendered.message_html);
        assert_eq!(
            text.contains("command_execution"),
            line.contains("command_execution"),
            "the item kind is the only payload detail a structured relay keeps: {text}"
        );
        assert_eq!(
            text.contains("line="),
            kind.starts_with("agent "),
            "a structured relay must not echo its provider JSON, and a line without a kind is its own summary: {text}"
        );
        assert_eq!(rendered.agent_stdout, stream == "stdout");
        assert_eq!(rendered.source, "supervisor");
        assert_eq!(rendered.target, target);
    }
    for (target, line, expected) in [
        (
            "orbit_engine::activity_job::cli_runner",
            Some("legacy relay"),
            true,
        ),
        (
            "orbit_engine::activity_job::cli_runner::supervisor",
            None,
            false,
        ),
        ("orbit.other", Some("orchestration detail"), false),
    ] {
        let rendered = render_log_event_for_web(&json!({
            "target": target,
            "fields": {"message": "orchestration event", "stream": "stdout", "line": line}
        }));
        assert_eq!(
            rendered.agent_stdout, expected,
            "only actual stdout relays may be hidden: {target}"
        );
    }
}

/// Context shortening must respect path boundaries, retain full escaped
/// tooltips, and never mistake a JSON relay containing a path for a path.
#[test]
fn rendered_context_shortens_paths_without_losing_full_values_or_html_safety() {
    let _env = orbit_common::test_env::scoped([]);
    let home = std::env::var("HOME").expect("test home");
    let worktree = "/srv/project/.orbit/state/worktrees/orbit-jrun-20261007-0722-c12/src/lib.rs";
    for (value, short) in [
        (
            format!("{home}/workspace/<file>\".rs"),
            "~/workspace/<file>\".rs".to_string(),
        ),
        (home.clone(), "~".to_string()),
        (
            worktree.to_string(),
            "jrun-20261007-0722-c12/src/lib.rs".to_string(),
        ),
        (
            format!(
                "/srv/project/.orbit/state/worktrees/orbit-jrun-outer/.orbit/tmp/fixture{worktree}"
            ),
            "jrun-20261007-0722-c12/src/lib.rs".to_string(),
        ),
        (
            format!("{home}-other/file.rs"),
            format!("{home}-other/file.rs"),
        ),
        (
            "/srv/other/file.rs".to_string(),
            "/srv/other/file.rs".to_string(),
        ),
        (
            format!(r#"{{"path":"{worktree}"}}"#),
            format!(r#"{{"path":"{worktree}"}}"#),
        ),
    ] {
        let rendered = render_log_event_for_web(&json!({
            "target": "orbit.other",
            "fields": {"cwd": value, "job_run_id": "jrun-20261007-0722-c12", "message": "worker ready", "a": 1}
        }));
        let html = &rendered.message_html;
        assert!(
            visible_text(html).starts_with("worker ready a=1 cwd="),
            "human message must precede structured context: {html}"
        );
        let escape = |s: &str| {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
        };
        assert!(
            html.contains(&format!(">{}</code>", escape(&short))),
            "visible path should shorten only at a path boundary: {html}"
        );
        if value != short {
            assert!(
                html.contains(&format!("title=\"{}\"", escape(&value))),
                "shortened paths must retain a safely escaped full-value title: {html}"
            );
        }
        assert!(
            !html.contains("<file>"),
            "log path markup must be escaped: {html}"
        );
    }
}

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
