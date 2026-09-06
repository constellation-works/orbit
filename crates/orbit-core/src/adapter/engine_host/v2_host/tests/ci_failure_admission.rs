//! CI-sweep quarantine and post-pilot admission regressions.

use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, write_workspace_file,
};
use crate::application::task::TaskAddParams;

use super::ci_failure_tasks::{failure, snapshot};

const CHECKOUT: &str = "3333333333333333333333333333333333333333";
const NEXT_HEAD: &str = "4444444444444444444444444444444444444444";

fn file(runtime: &OrbitRuntime, failures: Vec<Value>) -> Value {
    runtime
        .run_deterministic(
            "file_ci_failure_tasks",
            &json!({}),
            &json!({"ci_evidence": snapshot(failures)}),
            ToolContext::default(),
        )
        .expect("file CI task")
}

struct Assessment<'a> {
    selectors: Vec<&'a str>,
    disposition: &'a str,
    duplicate_of: Value,
    already_landed: Value,
    warnings: Vec<&'a str>,
    authorized: bool,
}

impl<'a> Assessment<'a> {
    fn actionable(selector: &'a str, authorized: bool) -> Self {
        Self {
            selectors: vec![selector],
            disposition: "selectors",
            duplicate_of: Value::Null,
            already_landed: Value::Null,
            warnings: Vec::new(),
            authorized,
        }
    }

    fn already_landed(evidence: String) -> Self {
        Self {
            selectors: Vec::new(),
            disposition: "verified_no_diff",
            duplicate_of: Value::Null,
            already_landed: json!({"evidence": evidence}),
            warnings: Vec::new(),
            authorized: true,
        }
    }

    fn duplicate() -> Self {
        Self {
            selectors: Vec::new(),
            disposition: "verified_no_diff",
            duplicate_of: json!({
                "task_id": "ORB-EXISTING",
                "evidence": "an open task already owns the same current repair",
            }),
            already_landed: Value::Null,
            warnings: vec!["duplicate task must be inspected before reconsideration"],
            authorized: true,
        }
    }
}

fn apply_pilot(
    runtime: &OrbitRuntime,
    repo_root: &std::path::Path,
    filing: &Value,
    assessment: Assessment<'_>,
) -> Result<Value, String> {
    let task_id = filing["task_id"].as_str().expect("filed task id");
    let prepared = runtime
        .run_deterministic(
            "prepare_task_pilot",
            &json!({}),
            &json!({
                "task_ids": [task_id],
                "workspace_path": repo_root,
                "max_tasks": 1,
                "max_partition_size": 1,
            }),
            ToolContext::default(),
        )
        .expect("prepare CI pilot");
    let before = prepared["tasks"][0]["context_files_before"].clone();
    runtime
        .run_deterministic(
            "apply_task_pilot_results",
            &json!({}),
            &json!({
                "prepared": prepared,
                "results": [{
                    "partition_index": 0,
                    "task_ids": [task_id],
                    "tasks": [{
                        "task_id": task_id,
                        "context_files_before": before,
                        "context_files_after": assessment.selectors,
                        "disposition": assessment.disposition,
                        "evidence": "compared runner-tested and current integration revisions",
                        "recommended_crew": "system",
                        "recommended_complexity": "medium",
                        "blocked_by": [],
                        "duplicate_of": assessment.duplicate_of,
                        "already_landed": assessment.already_landed,
                        "adr_conflicts": [],
                        "utility_warnings": assessment.warnings,
                        "surface_warnings": [],
                    }],
                }],
                "workspace_path": repo_root,
                "ci_sweep_filing": filing,
                "promotion_authorized": assessment.authorized,
            }),
            ToolContext::default(),
        )
        .map_err(|error| error.to_string())
}

fn backlog_task_ids(runtime: &OrbitRuntime) -> Vec<String> {
    runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({"max_tasks": 50}),
            ToolContext::default(),
        )
        .expect("list backlog")["task_ids"]
        .as_array()
        .expect("task ids")
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

#[test]
fn active_auto_drain_cannot_see_finding_before_pilot_apply_and_authorization() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/current_regression.rs");
    let filed = file(
        &runtime,
        vec![failure(
            10,
            "ci",
            "build",
            "cargo build",
            "ci\tbuild\terror: current regression\n",
            CHECKOUT,
        )],
    );
    let filing = &filed["filed"][0];
    let task_id = filing["task_id"].as_str().expect("task id");
    assert!(!backlog_task_ids(&runtime).contains(&task_id.to_string()));

    let unauthorized = apply_pilot(
        &runtime,
        &repo_root,
        filing,
        Assessment::actionable("file:src/current_regression.rs", false),
    )
    .expect("apply selectors without promotion authority");
    assert_eq!(
        unauthorized["ci_sweep_admission"][0]["classification"],
        "promotion_not_authorized"
    );
    assert_eq!(
        runtime.get_task(task_id).expect("task").status,
        TaskStatus::Proposed
    );
    assert!(!backlog_task_ids(&runtime).contains(&task_id.to_string()));

    let authorized = apply_pilot(
        &runtime,
        &repo_root,
        filing,
        Assessment::actionable("file:src/current_regression.rs", true),
    )
    .expect("authorize successfully applied pilot");
    assert_eq!(
        authorized["ci_sweep_admission"][0]["classification"],
        "current_actionable_regression"
    );
    assert!(backlog_task_ids(&runtime).contains(&task_id.to_string()));
}

#[test]
fn already_landed_release_stays_proposed_but_distinct_current_regression_advances() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/current_regression.rs");
    let prior = runtime
        .add_task(TaskAddParams {
            title: "Repair Homebrew on_arm release failure".to_string(),
            description: "Release/Homebrew/on_arm fixed by 49740da.".to_string(),
            acceptance_criteria: vec!["The Homebrew ARM formula works.".to_string()],
            status: Some(TaskStatus::Done),
            ..TaskAddParams::default()
        })
        .expect("seed completed repair");
    let old = file(
        &runtime,
        vec![failure(
            10,
            "release",
            "homebrew",
            "on arm",
            "release\thomebrew\terror: old arm formula\n",
            CHECKOUT,
        )],
    );
    let old_filing = &old["filed"][0];
    let result = apply_pilot(
        &runtime,
        &repo_root,
        old_filing,
        Assessment::already_landed(format!(
            "done repair {} landed as 49740da on current integration",
            prior.id
        )),
    )
    .expect("classify already-landed release failure");
    assert_eq!(
        result["ci_sweep_admission"][0]["classification"],
        "already_landed"
    );
    assert_eq!(
        runtime
            .get_task(old_filing["task_id"].as_str().expect("old id"))
            .expect("old task")
            .status,
        TaskStatus::Proposed
    );

    let current = file(
        &runtime,
        vec![failure(
            11,
            "ci",
            "build",
            "cargo build",
            "ci\tbuild\terror: distinct current type failure\n",
            NEXT_HEAD,
        )],
    );
    let current_filing = &current["filed"][0];
    apply_pilot(
        &runtime,
        &repo_root,
        current_filing,
        Assessment::actionable("file:src/current_regression.rs", true),
    )
    .expect("admit distinct current regression");
    assert_eq!(
        runtime
            .get_task(current_filing["task_id"].as_str().expect("current id"))
            .expect("current task")
            .status,
        TaskStatus::Backlog
    );
}

#[test]
fn failed_or_warned_pilot_does_not_block_an_independent_eligible_fix() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/eligible.rs");
    let filed = file(
        &runtime,
        vec![
            failure(
                10,
                "ci-a",
                "build",
                "cargo build",
                "a\terror: duplicate\n",
                CHECKOUT,
            ),
            failure(
                11,
                "ci-b",
                "test",
                "cargo test",
                "b\terror: current\n",
                CHECKOUT,
            ),
            failure(
                12,
                "ci-c",
                "lint",
                "cargo clippy",
                "c\terror: invalid\n",
                CHECKOUT,
            ),
        ],
    );
    let filings = filed["filed"].as_array().expect("three filings");
    let warning = apply_pilot(&runtime, &repo_root, &filings[0], Assessment::duplicate())
        .expect("withhold duplicate warning");
    assert_eq!(warning["ci_sweep_admission"][0]["decision"], "withhold");

    let invalid = apply_pilot(
        &runtime,
        &repo_root,
        &filings[2],
        Assessment::actionable("file:src/missing.rs", true),
    )
    .expect("invalid selector is a durable failed partition");
    assert_eq!(invalid["status"], "failed");
    assert!(
        invalid["partition_decisions"][0]["error"]
            .as_str()
            .unwrap_or("")
            .contains("does not resolve"),
        "{invalid}"
    );

    let eligible = apply_pilot(
        &runtime,
        &repo_root,
        &filings[1],
        Assessment::actionable("file:src/eligible.rs", true),
    )
    .expect("independent eligible fix advances");
    assert_eq!(eligible["ci_sweep_admission"][0]["decision"], "promote");
    for (index, status) in [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::Proposed,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            runtime
                .get_task(filings[index]["task_id"].as_str().expect("task id"))
                .expect("task")
                .status,
            status
        );
    }
}
