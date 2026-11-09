//! `config show` text fitting at the output-rendering boundary: on a
//! terminal the renderer wraps descriptions to the sink width, and in plain
//! mode it keeps every line whole.

const DESCRIPTION: &str = "The crew dispatched when a task names no crew of its own, chosen from the configured crews and falling back only when that crew is empty or unset [allowed: any configured crew name]";

fn sample_text() -> crate::output::payload::ConfigShow {
    let mut text = crate::output::payload::ConfigShow::default();
    text.push_str("orbit config — effective\n\n");
    text.push_str(&format!(
        "{:<28} how tasks are shipped, reviewed and landed by the delivery pipeline on this machine\n",
        "Delivery (workflow.*)"
    ));
    text.push_str(&format!(
        "  {:<24}  {:<12}  {:<9}  ",
        "default_crew", "golden", "workspace"
    ));
    text.mark_trailing_column();
    text.push_str(&format!("{DESCRIPTION}\n"));
    text.push_str(&format!(
        "{:<28} 1 defined, all from workspace\n",
        "Crews (crews.*)"
    ));
    text.push_str(&format!(
        "  {:<10}  {:<7}  {:<9}  {:<8}  {:<12}  {:<6}  {:<4}  ",
        "golden", "true", "workspace", "codex", "global-model", "high", "cli"
    ));
    text.mark_trailing_column();
    text.push_str(&format!("{DESCRIPTION}\n"));
    text.push_str("\nPaths\n");
    text.push_str("  global root  ");
    text.mark_trailing_column();
    text.push_str("/home/operator/.orbit/workspaces/a-very-long-workspace-name/config.toml\n");
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
    assert_eq!(body, sample_text().text);
}

/// Multi-word cells must not be mistaken for extra columns in terminal output.
#[cfg(unix)]
#[test]
fn config_show_preserves_multi_word_columns() {
    use super::table_rendering::run_cli_on_terminal_at_width;
    use crate::task_workspace::{TaskSeed, TestWorkspace};

    let workspace = TestWorkspace::new("config-columns", TaskSeed::new(""));
    let path = workspace.work.join(".orbit/config.toml");
    let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    config["execution"]["env"]["pass"] = toml::Value::try_from(["cargo", "make"]).unwrap();
    config.as_table_mut().unwrap().insert("crews".into(), toml::toml! {
        [golden]
        provider = "codex"
        model = "global-model"
        effort = "high"
        tags = ["cli", "rust"]
        description = "A crew description long enough to wrap across several lines while keeping every tag in its cell and every continuation beneath the description column"
    }.into());
    std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();

    let plain = workspace.run(
        &["config", "show", "--all", "--format", "plain"],
        "plain config",
    );
    let plain = String::from_utf8(plain.stdout).unwrap();
    let ansi = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    let terminal = run_cli_on_terminal_at_width(&workspace, &["config", "show", "--all"], "120");
    let terminal = String::from_utf8(terminal.stdout).unwrap();
    let terminal = ansi.replace_all(&terminal, "");
    for (label, cell, state) in [
        ("env.pass", "cargo, make", "workspace"),
        ("golden", "cli, rust", ""),
    ] {
        let original = plain
            .lines()
            .find(|line| line.trim_start().starts_with(label))
            .unwrap();
        let cell_end = original.find(cell).unwrap() + cell.len();
        let prefix_end = if state.is_empty() {
            cell_end
        } else {
            original[cell_end..].find(state).unwrap() + cell_end + state.len()
        };
        let description_start = prefix_end
            + original[prefix_end..]
                .chars()
                .take_while(|c| *c == ' ')
                .count();
        let prefix = &original[..description_start];
        if state.is_empty() {
            let header = plain
                .lines()
                .find(|line| line.trim_start().starts_with("NAME "))
                .unwrap();
            assert_eq!(
                description_start,
                header.find("DESCRIPTION").unwrap(),
                "crew description follows the complete TAGS column"
            );
        }
        let lines = terminal.lines().collect::<Vec<_>>();
        let row = lines
            .iter()
            .position(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("{label} lost column padding: {terminal}"));
        assert!(original.chars().count() > 120, "fixture must wrap {label}");
        assert!(
            description_start + 24 <= 120,
            "fixture must leave room for aligned prose"
        );
        let continuation = lines[row + 1];
        assert_eq!(
            continuation.chars().take_while(|c| *c == ' ').count(),
            prefix.chars().count(),
            "{label} description alignment: {terminal}"
        );
        let mut rendered = lines[row][prefix.len()..].to_string();
        let mut next = row + 1;
        while next < lines.len() && lines[next].starts_with(&" ".repeat(prefix.chars().count())) {
            rendered.push(' ');
            rendered.push_str(lines[next].trim());
            next += 1;
        }
        assert_eq!(
            rendered,
            original[description_start..]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            "{label} retains its full description"
        );
    }
    assert!(terminal.lines().all(|line| line.chars().count() <= 120));
}
