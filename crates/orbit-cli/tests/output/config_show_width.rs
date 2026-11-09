//! `config show` text fitting at the output-rendering boundary: on a
//! terminal the renderer wraps descriptions to the sink width, and in plain
//! mode it keeps every line whole.

const DESCRIPTION: &str = "The crew dispatched when a task names no crew of its own, chosen from the configured crews and falling back only when that crew is empty or unset [allowed: any configured crew name]";

fn sample_text() -> String {
    let mut text = String::new();
    text.push_str("orbit config — effective\n\n");
    text.push_str(&format!(
        "{:<28} how tasks are shipped, reviewed and landed by the delivery pipeline on this machine\n",
        "Delivery (workflow.*)"
    ));
    text.push_str(&format!(
        "  {:<24}  {:<12}  {:<9}  {DESCRIPTION}\n",
        "default_crew", "golden", "workspace"
    ));
    text.push_str(&format!(
        "{:<28} 1 defined, all from workspace\n",
        "Crews (crews.*)"
    ));
    text.push_str(&format!(
        "  {:<10}  {:<7}  {:<9}  {:<8}  {:<12}  {:<6}  {:<4}  {DESCRIPTION}\n",
        "golden", "true", "workspace", "codex", "global-model", "high", "cli"
    ));
    text.push_str("\nPaths\n");
    text.push_str(
        "  global root  /home/operator/.orbit/workspaces/a-very-long-workspace-name/config.toml\n",
    );
    text
}

pub(super) fn render_fixture(case: &str) -> String {
    let logs =
        tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir()).expect("child logs");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    command
        .args([
            "--exact",
            "config_show_width::render_fixture_child",
            "--nocapture",
        ])
        .env("ORBIT_CONFIG_SHOW_RENDER_CASE", case);
    let output = orbit_common::test_env::run_child_test(
        &mut command,
        "config_show_width::render_fixture_child",
        logs.path(),
    );
    orbit_common::test_env::assert_child_test_passed(
        "config_show_width::render_fixture_child",
        output.status,
        &output.stdout,
        &output.stderr,
    );
    let stdout = String::from_utf8(output.stdout).expect("child stdout");
    let after = stdout
        .split_once("BEGIN_CONFIG_SHOW\n")
        .expect("start marker")
        .1;
    let (body, _) = after.split_once("END_CONFIG_SHOW").expect("end marker");
    body.to_string()
}

#[test]
fn render_fixture_child() {
    use crate::output::{
        payload::{Block, Payload},
        sink::{FormatArg, OutputSink, SinkEnv},
    };
    use std::io::Write as _;
    let Ok(case) = std::env::var("ORBIT_CONFIG_SHOW_RENDER_CASE") else {
        return;
    };
    let (is_tty, mode, columns) = if case == "plain" {
        (false, FormatArg::Plain, "80")
    } else {
        (true, FormatArg::Table, case.as_str())
    };
    let sink = OutputSink::resolve(
        is_tty,
        &SinkEnv {
            columns: Some(columns.into()),
            no_color: Some("1".into()),
            ..SinkEnv::default()
        },
        None,
        Some(mode),
        false,
    );
    writeln!(std::io::stdout(), "BEGIN_CONFIG_SHOW").expect("start marker");
    crate::output::render::emit(
        Payload::blocks(
            serde_json::json!({}),
            vec![Block::config_show(sample_text())],
        )
        .into(),
        &sink,
    )
    .expect("emit");
    writeln!(std::io::stdout(), "END_CONFIG_SHOW").expect("end marker");
}

#[test]
fn config_show_text_fits_terminal_widths() {
    for width in [60usize, 80, 120] {
        let body = render_fixture(&width.to_string());
        for line in body.lines() {
            assert!(
                line.chars().count() <= width,
                "width {width}: line exceeds terminal: {line:?}"
            );
        }
        // No information is lost by wrapping: every description word survives.
        let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
        for word in DESCRIPTION.split_whitespace() {
            assert!(collapsed.contains(word), "width {width}: lost {word:?}");
        }
        // The key row stays associated: its label, value and state lead a line.
        assert!(
            body.lines().any(|line| {
                line.starts_with("  default_crew")
                    && line.contains("golden")
                    && line.contains("workspace")
            }),
            "width {width}: row lost its columns:\n{body}"
        );
    }
}

#[test]
fn config_show_text_plain_keeps_every_line_whole() {
    let body = render_fixture("plain");
    assert_eq!(body, sample_text());
}
