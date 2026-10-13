use super::support::{Fixture, Server, isolated, json_ok};
use serde_json::{Value, json};
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
#[test]
fn rendered_agent_relays_put_kind_and_run_in_first_60_characters() {
    isolated(
        "log_render::rendered_agent_relays_put_kind_and_run_in_first_60_characters",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(false);

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
                let rendered = snapshot_render(
                    &fixture,
                    &server,
                    &json!({
                        "target": target,
                        "fields": {
                            "cwd": "/srv/project/.orbit/state/worktrees/orbit-jrun-20261007-0722-c12",
                            "job_run_id": "jrun-20261007-0722-c12",
                            "provider": "codex", "stream": stream, "line": line,
                        }
                    }),
                );
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
                assert_eq!(
                    rendered.source, "codex",
                    "agent output is labelled by its provider, not the module path"
                );
                assert_eq!(rendered.target, target);
            }
            for (target, line, expected) in [
                (
                    "orbit_engine::activity_job::cli_runner::supervisor",
                    Some("agent line"),
                    true,
                ),
                (
                    "orbit_engine::activity_job::cli_runner::supervisor",
                    None,
                    false,
                ),
                ("orbit.other", Some("orchestration detail"), false),
            ] {
                let rendered = snapshot_render(
                    &fixture,
                    &server,
                    &json!({
                        "target": target,
                        "fields": {"message": "orchestration event", "stream": "stdout", "line": line}
                    }),
                );
                assert_eq!(
                    rendered.agent_stdout, expected,
                    "only actual stdout relays may be hidden: {target}"
                );
            }
        },
    );
}

#[test]
fn rendered_context_shortens_paths_without_losing_full_values_or_html_safety() {
    isolated(
        "log_render::rendered_context_shortens_paths_without_losing_full_values_or_html_safety",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(false);

            let home = fixture
                .path("")
                .display()
                .to_string()
                .trim_end_matches('/')
                .to_string();
            let worktree =
                "/srv/project/.orbit/state/worktrees/orbit-jrun-20261007-0722-c12/src/lib.rs";
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
                let rendered = snapshot_render(
                    &fixture,
                    &server,
                    &json!({
                        "target": "orbit.other",
                        "fields": {"cwd": value, "job_run_id": "jrun-20261007-0722-c12", "message": "worker ready", "a": 1}
                    }),
                );
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
        },
    );
}

#[derive(serde::Deserialize)]
struct Rendered {
    message_html: String,
    agent_stdout: bool,
    source: String,
    target: String,
}
fn snapshot_render(fixture: &Fixture, server: &Server, event: &Value) -> Rendered {
    std::fs::write(fixture.path("process.log"), format!("{event}\n")).unwrap();
    let snapshot = json_ok(server.get("/api/log?limit=1"));
    let events = snapshot["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        1,
        "the existing rendering case must reach the snapshot: {snapshot}"
    );
    serde_json::from_value(events[0].clone()).unwrap()
}
