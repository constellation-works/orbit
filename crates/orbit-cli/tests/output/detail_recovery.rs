//! Full human recovery of values truncated by terminal list tables.
use serde_json::Value;

use super::table_rendering::run_cli_on_terminal_at_width;
use crate::task_workspace::{TaskSeed, TestWorkspace};

fn workspace() -> TestWorkspace {
    TestWorkspace::new("detail-recovery-test", TaskSeed::new(""))
}

/// Long values in a detail table still need a human recovery path. Exercise
/// real terminal truncation and the single-record selector through the CLI.
#[cfg(unix)]
#[test]
fn truncated_parameter_has_full_single_record_detail() {
    let workspace = workspace();
    let description = format!("{} recovery-tail", "long parameter description ".repeat(12));
    let executable = workspace.work.join("detail-tool.sh");
    std::fs::write(&executable, "#!/bin/sh\n").unwrap();
    let manifest = workspace.work.join("detail-tool.orbit-tool.json");
    std::fs::write(
        &manifest,
        serde_json::json!({
            "name": "detail.fixture", "description": "Detail fixture",
            "parameters": [{"name": "value", "param_type": "string", "required": true,
                "description": description}]
        })
        .to_string(),
    )
    .unwrap();
    workspace.run(
        &["tool", "add", executable.to_str().unwrap()],
        "register detail fixture",
    );
    let listed = run_cli_on_terminal_at_width(
        &workspace,
        &["tool", "show", "detail.fixture", "--format", "table"],
        "65",
    );
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(
        !stdout.contains(&description),
        "fixture must truncate: {stdout}"
    );
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("value "))
            .count(),
        1,
        "one parameter row: {stdout}"
    );
    let args = ["tool", "show", "detail.fixture", "--parameter", "value"];
    let detail = run_cli_on_terminal_at_width(&workspace, &args, "65");
    assert!(String::from_utf8_lossy(&detail.stdout).contains(&description));
    let plain = workspace.run(
        &["tool", "show", "detail.fixture", "--format", "plain"],
        "full plain parameters",
    );
    assert!(String::from_utf8_lossy(&plain.stdout).contains(&description));
    let json = workspace.run(
        &["tool", "show", "detail.fixture", "--json"],
        "full JSON parameters",
    );
    let doc: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(doc["parameters"][0]["description"], description);
    let selected = workspace.run(
        &[
            "tool",
            "show",
            "detail.fixture",
            "--parameter",
            "value",
            "--json",
        ],
        "parameter JSON",
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&selected.stdout).unwrap()["description"],
        description
    );
    assert!(
        !workspace
            .run_raw(&["tool", "show", "detail.fixture", "--parameter", "absent"])
            .status
            .success()
    );
}

#[cfg(unix)]
#[test]
fn truncated_doctor_messages_have_full_single_record_details() {
    let workspace = workspace();
    let executable = workspace
        .work
        .join(format!("{}-missing.sh", "long-path-".repeat(15)));
    std::fs::write(&executable, "#!/bin/sh\n").unwrap();
    workspace.run(
        &[
            "tool",
            "add",
            executable.to_str().unwrap(),
            "--name",
            "detail.fixture",
            "--description",
            "Missing fixture",
        ],
        "register missing tool",
    );
    std::fs::rename(&executable, executable.with_extension("saved")).unwrap();
    for directory in [".agents/skills", ".claude/skills"] {
        let links = workspace.home.join(directory);
        std::fs::create_dir_all(&links).unwrap();
        std::os::unix::fs::symlink(
            workspace.home.join("missing-target"),
            links.join("detail-skill"),
        )
        .unwrap();
    }
    for (domain, key, name) in [
        ("tool", "tool_name", "detail.fixture"),
        ("skill", "skill_id", "detail-skill"),
    ] {
        let json = workspace.run(&[domain, "doctor", "--json"], "doctor JSON");
        let rows: Value = serde_json::from_slice(&json.stdout).unwrap();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row[key] == name)
            .unwrap();
        let message = row["message"].as_str().unwrap();
        assert!(message.len() > 65, "long diagnostic: {message}");
        let list = run_cli_on_terminal_at_width(
            &workspace,
            &[domain, "doctor", "--format", "table"],
            "65",
        );
        let stdout = String::from_utf8_lossy(&list.stdout);
        assert!(!stdout.contains(message), "fixture must truncate: {stdout}");
        assert_eq!(
            stdout.lines().filter(|line| line.starts_with(name)).count(),
            rows.as_array()
                .unwrap()
                .iter()
                .filter(|row| row[key] == name)
                .count(),
            "one line per diagnostic: {stdout}"
        );
        let detail = run_cli_on_terminal_at_width(&workspace, &[domain, "doctor", name], "65");
        assert!(String::from_utf8_lossy(&detail.stdout).contains(message));
        let plain = workspace.run(&[domain, "doctor", "--format", "plain"], "doctor plain");
        assert!(String::from_utf8_lossy(&plain.stdout).contains(message));
        let selected = workspace.run(&[domain, "doctor", name, "--json"], "single doctor JSON");
        let selected: Value = serde_json::from_slice(&selected.stdout).unwrap();
        if domain == "skill" {
            let findings = selected["findings"].as_array().unwrap();
            assert_eq!(findings.len(), 2, "both client link findings survive");
            for finding in findings {
                assert!(
                    String::from_utf8_lossy(&detail.stdout)
                        .contains(finding["message"].as_str().unwrap())
                );
            }
            assert!(findings.contains(row));
        } else {
            assert_eq!(&selected, row);
        }
        assert!(
            !workspace
                .run_raw(&[domain, "doctor", "absent-record"])
                .status
                .success()
        );
    }
}

#[cfg(unix)]
#[test]
fn workspace_doctor_info_has_full_single_check_detail() {
    let workspace = workspace();
    let json = workspace.run_raw(&["doctor", "--json"]);
    let rows: Value = serde_json::from_slice(&json.stdout).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["check"] == "resource-throttle")
        .unwrap();
    let detail = run_cli_on_terminal_at_width(
        &workspace,
        &["doctor", "--check", "resource-throttle"],
        "25",
    );
    assert!(String::from_utf8_lossy(&detail.stdout).contains(row["message"].as_str().unwrap()));
    let selected = workspace.run(
        &["doctor", "--check", "resource-throttle", "--json"],
        "single check JSON",
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&selected.stdout).unwrap()["message"],
        row["message"]
    );
    assert!(
        !workspace
            .run_raw(&["doctor", "--check", "absent-check"])
            .status
            .success()
    );
}

#[cfg(unix)]
#[test]
fn migration_components_stay_whole_on_narrow_terminal() {
    let workspace = workspace();
    let report = run_cli_on_terminal_at_width(&workspace, &["migrate", "--format", "table"], "25");
    let stdout = String::from_utf8_lossy(&report.stdout);
    assert!(stdout.contains("workspace layout"), "{stdout}");
    assert!(stdout.contains("store schema"), "{stdout}");
    assert_eq!(
        stdout.lines().count(),
        3,
        "header and two single-line records: {stdout}"
    );
}
