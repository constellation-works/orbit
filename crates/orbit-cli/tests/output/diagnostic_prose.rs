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
        vec!["task", "flow"],
        vec!["codebase", "analytics"],
        vec!["run", "history", "--no-reconcile"],
        vec!["doctor", "admission-receipts"],
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
                let table_row =
                    args[0] == "task" && (line.starts_with("WINDOW ") || line.starts_with("20"));
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

#[test]
fn admission_receipt_summary_wraps_and_plain_bytes_stay_unchanged() {
    let workspace = TestWorkspace::new("admission-receipt-prose", TaskSeed::new(""));
    let args = ["doctor", "admission-receipts"];
    let piped = workspace.run(&args, "piped admission-receipt report");
    let plain = workspace.run(
        &["doctor", "admission-receipts", "--format", "plain"],
        "explicit plain admission-receipt report",
    );
    assert_eq!(plain.stdout, piped.stdout);
    assert_eq!(plain.stderr, piped.stderr);

    let piped_json = workspace.run(
        &["doctor", "admission-receipts", "--json"],
        "JSON admission-receipt report",
    );
    let expected_json: serde_json::Value = serde_json::from_slice(&piped_json.stdout).unwrap();
    let ansi = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    for width in ["60", "80", "120"] {
        let terminal = run_cli_on_terminal_at_width(&workspace, &args, width);
        let stdout = String::from_utf8_lossy(&terminal.stdout);
        let stdout = ansi.replace_all(&stdout, "");
        let width = width.parse::<usize>().unwrap();
        assert!(
            stdout.lines().all(|line| line.chars().count() <= width),
            "admission receipt prose at {width} columns:\n{stdout}"
        );
        let normalized = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains("Claim mutation receipts: 0 (0 logical payload bytes; pruned separately on settlement after 7 days)"),
            "the receipt label stays associated with its wrapped explanation:\n{stdout}"
        );
        assert!(
            stdout
                .lines()
                .any(|line| line.starts_with("Logical payload bytes exclude")),
            "the final explanation starts on its own section line:\n{stdout}"
        );

        let terminal_plain = run_cli_on_terminal_at_width(
            &workspace,
            &["doctor", "admission-receipts", "--format", "plain"],
            &width.to_string(),
        );
        let terminal_plain_stdout =
            String::from_utf8_lossy(&terminal_plain.stdout).replace("\r\n", "\n");
        assert_eq!(terminal_plain_stdout.as_bytes(), piped.stdout);
        assert_eq!(terminal_plain.stderr, piped.stderr);

        let terminal_json = run_cli_on_terminal_at_width(
            &workspace,
            &["doctor", "admission-receipts", "--json"],
            &width.to_string(),
        );
        let actual_json: serde_json::Value = serde_json::from_slice(&terminal_json.stdout).unwrap();
        assert_eq!(actual_json, expected_json);
        assert_eq!(terminal_json.stderr, piped_json.stderr);
    }
}

#[test]
fn failed_run_header_wraps_without_changing_plain_or_json_output() {
    let workspace = TestWorkspace::new("failed-run-prose", TaskSeed::new(""));
    let jobs = workspace.home.join(".orbit/resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let job = serde_json::json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": {"name": "long-failure"},
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "steps": [{
                "id": "script",
                "spec": {
                    "type": "deterministic",
                    "action": "local_shell",
                    "config": {"shell": "/bin/sh", "script": "exit 1"}
                }
            }]
        }
    });
    std::fs::write(jobs.join("long-failure.yaml"), job.to_string()).unwrap();
    let failed = workspace.run_raw(&["run", "job", "long-failure", "--wait", "--json"]);
    assert_eq!(
        failed.status.code(),
        Some(1),
        "failed fixture job: {failed:?}"
    );
    let failed_json: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    let run_id = failed_json["run_id"].as_str().unwrap();
    let long_message = "provider reported a sustained capacity limit while preparing the signed execution context; the failed run retained this explanation so the operator can identify the resource, retry boundary, and affected service";
    let database = rusqlite::Connection::open(workspace.home.join(".orbit/orbit.db")).unwrap();
    let events = {
        let mut statement = database
            .prepare("SELECT event_id, payload_json FROM v2_audit_events WHERE run_id = ?1")
            .unwrap();
        statement
            .query_map([run_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let mut changed = 0;
    for (event_id, payload_json) in events {
        let mut payload: serde_json::Value = serde_json::from_str(&payload_json).unwrap();
        if payload["step_id"] == "script" {
            payload["error_message"] = serde_json::json!(long_message);
            changed += database
                .execute(
                    "UPDATE v2_audit_events SET payload_json = ?1 WHERE event_id = ?2",
                    rusqlite::params![payload.to_string(), event_id],
                )
                .unwrap();
        }
    }
    assert!(changed > 0, "the disposable failed run has a recorded step");

    let args = ["run", "show", run_id, "--no-reconcile"];
    let piped = workspace.run(&args, "piped failed-run summary");
    let plain = workspace.run(
        &["run", "show", run_id, "--no-reconcile", "--format", "plain"],
        "explicit plain failed-run summary",
    );
    assert_eq!(plain.stdout, piped.stdout);
    assert_eq!(plain.stderr, piped.stderr);
    let piped_json = workspace.run(
        &["run", "show", run_id, "--no-reconcile", "--json"],
        "JSON failed-run summary",
    );
    let expected_json: serde_json::Value = serde_json::from_slice(&piped_json.stdout).unwrap();

    let ansi = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    for width in ["60", "80", "120"] {
        let terminal = run_cli_on_terminal_at_width(&workspace, &args, width);
        let raw = String::from_utf8_lossy(&terminal.stdout);
        let stdout = ansi.replace_all(&raw, "");
        let lines = stdout.lines().collect::<Vec<_>>();
        let start = lines
            .iter()
            .position(|line| line.starts_with("Root cause:"))
            .expect("root cause header");
        let end = lines
            .iter()
            .enumerate()
            .skip(start + 1)
            .find_map(|(index, line)| line.starts_with("Logs:").then_some(index))
            .expect("root cause log label");
        let cause_lines = &lines[start..end];
        let width = width.parse::<usize>().unwrap();
        assert!(cause_lines.len() > 1, "the long cause wraps:\n{stdout}");
        assert!(
            cause_lines.iter().all(|line| line.chars().count() <= width),
            "root cause exceeds {width} columns:\n{}",
            cause_lines.join("\n")
        );
        let cause = cause_lines.join(" ");
        assert!(
            cause.contains(long_message),
            "the wrapped lines remain associated with their label:\n{stdout}"
        );

        let terminal_plain = run_cli_on_terminal_at_width(
            &workspace,
            &["run", "show", run_id, "--no-reconcile", "--format", "plain"],
            &width.to_string(),
        );
        let terminal_plain_stdout =
            String::from_utf8_lossy(&terminal_plain.stdout).replace("\r\n", "\n");
        assert_eq!(terminal_plain_stdout.as_bytes(), piped.stdout);
        assert_eq!(terminal_plain.stderr, piped.stderr);
        let terminal_json = run_cli_on_terminal_at_width(
            &workspace,
            &["run", "show", run_id, "--no-reconcile", "--json"],
            &width.to_string(),
        );
        let actual_json: serde_json::Value = serde_json::from_slice(&terminal_json.stdout).unwrap();
        assert_eq!(actual_json, expected_json);
        assert_eq!(terminal_json.stderr, piped_json.stderr);
    }
}
