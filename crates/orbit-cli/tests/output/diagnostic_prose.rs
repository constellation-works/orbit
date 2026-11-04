//! Regression for diagnostic prose overflowing a real terminal.
use super::table_rendering::run_cli_on_terminal_at_width;
use crate::task_workspace::{TaskSeed, TestWorkspace};

#[test]
fn diagnostic_prose_wraps_on_terminals_and_preserves_pipe_and_json() {
    let workspace = TestWorkspace::new("diagnostic-prose", TaskSeed::new(""));
    let timestamp = regex::Regex::new(r"\d{4}-\d\d-\d\dT[0-9:.]+(?:Z|[+-]\d\d:\d\d)").unwrap();
    let normalize = |text: &str| {
        timestamp
            .replace_all(text, "TIMESTAMP")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let ansi = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    for args in [
        vec!["agents", "scorecard", "--role", "implementers"],
        vec!["run", "readiness"],
        vec!["crew", "ladder"],
        vec!["task", "flow"],
        vec!["codebase", "analytics"],
        vec!["run", "history", "--no-reconcile"],
    ] {
        let piped = workspace.run(&args, "piped diagnostics");
        let mut json_args = args.clone();
        json_args.push("--json");
        let piped_json = workspace.run(&json_args, "JSON diagnostics");
        let expected_json: serde_json::Value = serde_json::from_slice(&piped_json.stdout).unwrap();
        let mut plain_args = args.clone();
        plain_args.extend(["--format", "plain"]);
        for width in ["60", "80", "120"] {
            let terminal = run_cli_on_terminal_at_width(&workspace, &args, width);
            let stdout = String::from_utf8(terminal.stdout).unwrap();
            let stderr = String::from_utf8(terminal.stderr).unwrap();
            let stdout = ansi.replace_all(&stdout, "");
            let stderr = ansi.replace_all(&stderr, "");
            for line in stdout.lines().chain(stderr.lines()) {
                // Fixed table columns may overflow by contract; only prose wraps.
                let table_row = args[0] == "crew"
                    && ["TIER", "low", "medium", "hard", "xhard"]
                        .iter()
                        .any(|prefix| line.starts_with(&format!("{prefix} ")))
                    || args[0] == "task" && (line.starts_with("WINDOW ") || line.starts_with("20"));
                assert!(
                    table_row || line.chars().count() <= width.parse::<usize>().unwrap(),
                    "{args:?} at {width} columns overflows: {line:?}"
                );
            }
            let terminal_text = normalize(&format!("{stdout} {stderr}"));
            for line in String::from_utf8_lossy(&piped.stdout)
                .lines()
                .chain(String::from_utf8_lossy(&piped.stderr).lines())
            {
                if line.chars().count() > width.parse::<usize>().unwrap() && !line.contains('\t') {
                    assert!(
                        terminal_text.contains(&normalize(line)),
                        "{args:?} must preserve whole words: {line:?} in {terminal_text:?}"
                    );
                }
            }
            let plain = run_cli_on_terminal_at_width(&workspace, &plain_args, width);
            assert_eq!(
                String::from_utf8_lossy(&plain.stdout).lines().count(),
                String::from_utf8_lossy(&piped.stdout).lines().count()
            );
            assert_eq!(
                String::from_utf8_lossy(&plain.stderr).lines().count(),
                String::from_utf8_lossy(&piped.stderr).lines().count()
            );
            assert_eq!(
                normalize(&String::from_utf8_lossy(&plain.stdout)),
                normalize(&String::from_utf8_lossy(&piped.stdout))
            );
            assert_eq!(
                normalize(&String::from_utf8_lossy(&plain.stderr)),
                normalize(&String::from_utf8_lossy(&piped.stderr))
            );
            let json = run_cli_on_terminal_at_width(&workspace, &json_args, width);
            let actual_json: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
            assert_eq!(
                normalize(&actual_json.to_string()),
                normalize(&expected_json.to_string()),
                "{args:?} JSON at {width}"
            );
            assert_eq!(
                normalize(&String::from_utf8_lossy(&json.stderr)),
                normalize(&String::from_utf8_lossy(&piped_json.stderr))
            );
        }
    }
}
