//! Code scanning alerts that share one repair become one bounded task.
//!
//! The fixtures are the incidents that motivated grouping: the three
//! `reconcile.rs` hash/salt alerts one commit covered (F2026-09-105), the
//! Antigravity and bootstrap alerts that both named the same
//! `validate_trusted_host_activity` flow (F2026-09-093, F2026-09-097,
//! F2026-09-101), and sibling scoreboard reads behind one path check.

use orbit_types::task::{Task, TaskPriority, TaskStatus};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, write_workspace_file,
};
use crate::application::task::TaskUpdateParams;

use super::dependabot_alert_tasks::{code_alert, expanded_snapshot, file};

const RECONCILE: &str = "crates/orbit-core/src/runtime/reconcile.rs";
const ANTIGRAVITY: &str = "crates/orbit-agent/src/providers/antigravity/antigravity_cli.rs";
const BOOTSTRAP: &str = "crates/orbit-core/src/bootstrap/activity.rs";
const SCOREBOARD: &str = "crates/orbit-core/src/runtime/scoreboard.rs";

const SALT: &str = "The hard-coded value is used as a salt for this hash.";
const TRUSTED_HOST: &str = "This expression logs sensitive data returned by validate_trusted_host_activity(...) as clear text.";
const SCOREBOARD_READ: &str =
    "This path depends on a user-provided value read from the scoreboard record.";

/// One Code scanning alert with the rule, message and location that decide
/// which repair it belongs to.
fn scan_alert(number: u64, rule: &str, message: &str, path: &str, line: u64) -> Value {
    severity_alert(number, rule, message, path, line, "high")
}

fn severity_alert(
    number: u64,
    rule: &str,
    message: &str,
    path: &str,
    line: u64,
    severity: &str,
) -> Value {
    let mut alert = code_alert(number, severity);
    alert["rule_id"] = json!(rule);
    alert["rule_name"] = json!(rule);
    alert["message"] = json!(message);
    alert["path"] = json!(path);
    alert["start_line"] = json!(line);
    alert["end_line"] = json!(line);
    alert
}

fn sweep(runtime: &OrbitRuntime, alerts: Vec<Value>) -> Value {
    file(
        runtime,
        expanded_snapshot(Vec::new(), alerts, Vec::new()),
        json!({}),
    )
}

fn filed_code_tasks(runtime: &OrbitRuntime, output: &Value) -> Vec<Task> {
    output["filed"]
        .as_array()
        .expect("filed")
        .iter()
        .filter(|entry| entry["family"] == "code_scanning")
        .map(|entry| {
            let task_id = entry["task_id"].as_str().expect("task id");
            runtime.get_task(task_id).expect("filed task")
        })
        .collect()
}

fn reconcile_hash_alerts() -> Vec<Value> {
    vec![
        scan_alert(
            101,
            "rust/hard-coded-cryptographic-value",
            SALT,
            RECONCILE,
            118,
        ),
        scan_alert(
            102,
            "rust/hard-coded-cryptographic-value",
            SALT,
            RECONCILE,
            246,
        ),
        scan_alert(
            103,
            "rust/hard-coded-cryptographic-value",
            SALT,
            RECONCILE,
            426,
        ),
    ]
}

#[test]
fn one_shared_cause_across_line_offsets_files_a_single_task_naming_every_alert() {
    let (_root, runtime, repo) = runtime_with_workspace_layout();
    write_workspace_file(&repo, RECONCILE);

    let output = sweep(&runtime, reconcile_hash_alerts());

    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(output["filed"][0]["alert_numbers"], json!([101, 102, 103]));
    assert_eq!(output["filed"][0]["alert_count"], json!(3));
    assert_eq!(output["filed"][0]["paths"], json!([RECONCILE]));

    let task = filed_code_tasks(&runtime, &output).remove(0);
    for alert in ["#101", "#102", "#103"] {
        assert!(
            task.description.contains(alert),
            "alert {alert} missing from the ledger: {}",
            task.description
        );
    }
    for line in ["line 118", "line 246", "line 426"] {
        assert!(task.description.contains(line), "location {line} missing");
    }
    // Coverage stays per alert: the group carries each member's generated key.
    assert_eq!(
        task.tags
            .iter()
            .filter(|tag| tag.starts_with("code-scanning:"))
            .count(),
        3
    );
    assert_eq!(task.context_files, vec![format!("file:{RECONCILE}")]);
}

#[test]
fn one_cause_reported_in_two_files_is_still_one_repair() {
    let (_root, runtime, repo) = runtime_with_workspace_layout();
    write_workspace_file(&repo, ANTIGRAVITY);
    write_workspace_file(&repo, BOOTSTRAP);

    let output = sweep(
        &runtime,
        vec![
            scan_alert(15, "rust/cleartext-logging", TRUSTED_HOST, ANTIGRAVITY, 162),
            scan_alert(16, "rust/cleartext-logging", TRUSTED_HOST, BOOTSTRAP, 238),
        ],
    );

    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(output["filed"][0]["alert_numbers"], json!([15, 16]));

    let task = filed_code_tasks(&runtime, &output).remove(0);
    assert!(
        task.title.contains("2 locations across 2 files"),
        "{}",
        task.title
    );
    assert_eq!(
        task.context_files,
        vec![format!("file:{ANTIGRAVITY}"), format!("file:{BOOTSTRAP}")]
    );
}

#[test]
fn sibling_reads_behind_one_check_group_and_take_the_highest_severity() {
    let (_root, runtime, repo) = runtime_with_workspace_layout();
    write_workspace_file(&repo, SCOREBOARD);

    let output = sweep(
        &runtime,
        vec![
            severity_alert(
                20,
                "rust/path-injection",
                SCOREBOARD_READ,
                SCOREBOARD,
                44,
                "high",
            ),
            severity_alert(
                21,
                "rust/path-injection",
                SCOREBOARD_READ,
                SCOREBOARD,
                51,
                "critical",
            ),
        ],
    );

    assert_eq!(output["filed_count"], json!(1));
    let task = filed_code_tasks(&runtime, &output).remove(0);
    assert_eq!(task.priority, TaskPriority::Critical);
    assert!(
        task.description
            .contains("Highest open security severity: `critical`")
    );
}

#[test]
fn unrelated_findings_in_one_file_and_similar_messages_stay_separate() {
    let (_root, runtime, repo) = runtime_with_workspace_layout();
    write_workspace_file(&repo, RECONCILE);

    let output = sweep(
        &runtime,
        vec![
            // Same file, same rule, but a different construct to repair.
            scan_alert(
                30,
                "rust/hard-coded-cryptographic-value",
                SALT,
                RECONCILE,
                118,
            ),
            scan_alert(
                31,
                "rust/hard-coded-cryptographic-value",
                "The hard-coded value is used as a key for this cipher.",
                RECONCILE,
                140,
            ),
            // The same sentence under a different rule is a different repair.
            scan_alert(32, "rust/insecure-hash", SALT, RECONCILE, 118),
        ],
    );

    assert_eq!(output["filed_count"], json!(3));
    let numbers = output["filed"]
        .as_array()
        .expect("filed")
        .iter()
        .map(|entry| entry["alert_numbers"].clone())
        .collect::<Vec<_>>();
    assert_eq!(numbers, vec![json!([30]), json!([31]), json!([32])]);
}

#[test]
fn a_different_analysis_of_the_same_rule_and_message_is_not_grouped() {
    let (_root, runtime, repo) = runtime_with_workspace_layout();
    write_workspace_file(&repo, RECONCILE);

    let mut other_tool = scan_alert(
        41,
        "rust/hard-coded-cryptographic-value",
        SALT,
        RECONCILE,
        246,
    );
    other_tool["tool_guid"] = json!("a-different-analysis");

    let output = sweep(
        &runtime,
        vec![
            scan_alert(
                40,
                "rust/hard-coded-cryptographic-value",
                SALT,
                RECONCILE,
                118,
            ),
            other_tool,
        ],
    );

    assert_eq!(output["filed_count"], json!(2));
}

#[test]
fn repeating_and_reordering_a_snapshot_files_nothing_further() {
    let (_root, runtime, repo) = runtime_with_workspace_layout();
    write_workspace_file(&repo, RECONCILE);

    let first = sweep(&runtime, reconcile_hash_alerts());
    assert_eq!(first["filed_count"], json!(1));
    let before = runtime.list_tasks().expect("tasks").len();

    let repeated = sweep(&runtime, reconcile_hash_alerts());
    assert_eq!(repeated["filed_count"], json!(0));

    let mut reordered = reconcile_hash_alerts();
    reordered.reverse();
    let out_of_order = sweep(&runtime, reordered);
    assert_eq!(out_of_order["filed_count"], json!(0));
    assert_eq!(
        out_of_order["skipped_existing"]
            .as_array()
            .expect("skipped")
            .len(),
        3
    );

    assert_eq!(runtime.list_tasks().expect("tasks").len(), before);
}

#[test]
fn a_new_alert_on_a_covered_cause_is_attributed_delta_work() {
    let (_root, runtime, repo) = runtime_with_workspace_layout();
    write_workspace_file(&repo, RECONCILE);

    let mut alerts = reconcile_hash_alerts();
    let late = alerts.pop().expect("third alert");
    let first = sweep(&runtime, alerts.clone());
    assert_eq!(first["filed_count"], json!(1));
    let covering_id = first["filed"][0]["task_id"]
        .as_str()
        .expect("covering task id")
        .to_string();

    // Somebody picked the covering task up before the next scan landed.
    runtime
        .update_task(
            &covering_id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..TaskUpdateParams::default()
            },
        )
        .expect("start the covering task");
    let running = runtime.get_task(&covering_id).expect("covering task");

    alerts.push(late);
    let delta = sweep(&runtime, alerts);

    assert_eq!(delta["filed_count"], json!(1));
    assert_eq!(delta["filed"][0]["alert_numbers"], json!([103]));
    assert_eq!(
        delta["filed"][0]["delta_covered_by"],
        json!([
            {"alert_number": 101, "task_id": covering_id},
            {"alert_number": 102, "task_id": covering_id},
        ])
    );

    // The running task is reported, not rewritten.
    let after = runtime.get_task(&covering_id).expect("covering task");
    assert_eq!(after.description, running.description);
    assert_eq!(after.tags, running.tags);
    assert_eq!(after.acceptance_criteria, running.acceptance_criteria);

    let delta_task = filed_code_tasks(&runtime, &delta).remove(0);
    assert!(delta_task.description.contains("Already covered elsewhere"));
    assert!(delta_task.description.contains(&covering_id));
    assert!(
        delta_task
            .tags
            .iter()
            .filter(|tag| tag.starts_with("code-scanning:"))
            .count()
            == 1,
        "a delta task must claim only the alerts it owns: {:?}",
        delta_task.tags
    );
}

#[test]
fn a_partial_fix_leaves_the_still_reported_alert_visible() {
    let (_root, runtime, repo) = runtime_with_workspace_layout();
    write_workspace_file(&repo, RECONCILE);

    let first = sweep(&runtime, reconcile_hash_alerts());
    let group_id = first["filed"][0]["task_id"]
        .as_str()
        .expect("group task id")
        .to_string();
    runtime
        .update_task(
            &group_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..TaskUpdateParams::default()
            },
        )
        .expect("close the group task");

    // Two locations are gone from the next scan; one is still real.
    let remaining = sweep(
        &runtime,
        vec![scan_alert(
            103,
            "rust/hard-coded-cryptographic-value",
            SALT,
            RECONCILE,
            426,
        )],
    );

    assert_eq!(
        remaining["filed_count"],
        json!(1),
        "a completed group must not suppress an alert the scanner still reports"
    );
    assert_eq!(remaining["filed"][0]["alert_numbers"], json!([103]));
}

#[test]
fn every_group_carries_bounded_scope_and_a_per_alert_disposition_contract() {
    let (_root, runtime, repo) = runtime_with_workspace_layout();
    write_workspace_file(&repo, RECONCILE);

    let output = sweep(&runtime, reconcile_hash_alerts());
    assert_eq!(
        output["code_scanning_group_bounds"],
        json!({"max_alerts": 12, "max_paths": 5})
    );

    let task = filed_code_tasks(&runtime, &output).remove(0);
    let disposition = task
        .acceptance_criteria
        .iter()
        .find(|criterion| criterion.contains("already-covered"))
        .expect("per-alert disposition criterion");
    assert!(disposition.contains("ancestor of the HEAD you validated"));
    assert!(disposition.contains("never present another task's commit as this task's own change"));

    let validation = task
        .acceptance_criteria
        .iter()
        .find(|criterion| criterion.contains("hosted rescan"))
        .expect("hosted closure criterion");
    assert!(validation.contains("Report source-side coverage and hosted alert closure separately"));

    assert!(
        task.acceptance_criteria
            .iter()
            .any(|criterion| criterion.contains("do not suppress"))
    );
}

#[test]
fn a_cause_larger_than_the_bounds_splits_into_executable_groups() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();

    let by_alert_count = (1..=14)
        .map(|index| {
            scan_alert(
                index,
                "rust/hard-coded-cryptographic-value",
                SALT,
                RECONCILE,
                100 + index,
            )
        })
        .collect::<Vec<_>>();
    let output = sweep(&runtime, by_alert_count);
    assert_eq!(output["filed_count"], json!(2));
    assert_eq!(output["filed"][0]["alert_count"], json!(12));
    assert_eq!(output["filed"][1]["alert_count"], json!(2));

    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let by_path_count = (1..=6)
        .map(|index| {
            scan_alert(
                index,
                "rust/cleartext-logging",
                TRUSTED_HOST,
                &format!("crates/orbit-core/src/file_{index}.rs"),
                10,
            )
        })
        .collect::<Vec<_>>();
    let output = sweep(&runtime, by_path_count);
    assert_eq!(output["filed_count"], json!(2));
    assert_eq!(
        output["filed"][0]["paths"].as_array().expect("paths").len(),
        5
    );
    assert_eq!(
        output["filed"][1]["paths"].as_array().expect("paths").len(),
        1
    );
}

/// The before/after count grouping exists to move: one representative
/// snapshot of seven alerts that today's per-alert sweep would file as seven
/// tasks.
#[test]
fn a_representative_snapshot_files_three_tasks_instead_of_seven() {
    let (_root, runtime, repo) = runtime_with_workspace_layout();
    for path in [RECONCILE, ANTIGRAVITY, BOOTSTRAP, SCOREBOARD] {
        write_workspace_file(&repo, path);
    }

    let mut alerts = reconcile_hash_alerts();
    alerts.push(scan_alert(
        15,
        "rust/cleartext-logging",
        TRUSTED_HOST,
        ANTIGRAVITY,
        162,
    ));
    alerts.push(scan_alert(
        16,
        "rust/cleartext-logging",
        TRUSTED_HOST,
        BOOTSTRAP,
        238,
    ));
    alerts.push(scan_alert(
        20,
        "rust/path-injection",
        SCOREBOARD_READ,
        SCOREBOARD,
        44,
    ));
    alerts.push(scan_alert(
        21,
        "rust/path-injection",
        SCOREBOARD_READ,
        SCOREBOARD,
        51,
    ));
    let alert_count = alerts.len();

    assert_eq!(runtime.list_tasks().expect("tasks").len(), 0);
    let output = sweep(&runtime, alerts);

    assert_eq!(alert_count, 7);
    assert_eq!(output["filed_count"], json!(3));
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 3);
    assert_eq!(
        filed_code_tasks(&runtime, &output)
            .iter()
            .map(|task| task
                .tags
                .iter()
                .filter(|tag| tag.starts_with("code-scanning:"))
                .count())
            .sum::<usize>(),
        alert_count,
        "every alert must remain covered by exactly one group"
    );
}
