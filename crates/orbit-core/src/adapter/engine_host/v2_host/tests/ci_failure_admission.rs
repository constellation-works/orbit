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
    release_action_required: Value,
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
            release_action_required: Value::Null,
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
            release_action_required: Value::Null,
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
            release_action_required: Value::Null,
            warnings: vec!["duplicate task must be inspected before reconsideration"],
            authorized: true,
        }
    }

    fn unproven_no_diff() -> Self {
        Self {
            selectors: Vec::new(),
            disposition: "verified_no_diff",
            duplicate_of: Value::Null,
            already_landed: Value::Null,
            release_action_required: Value::Null,
            warnings: Vec::new(),
            authorized: true,
        }
    }

    /// The incident shape: a job resolves a version this repository records
    /// but nobody published, and the pilot reports the publication as the
    /// required action. Passing selectors reproduces the pilot that proposed
    /// editing the version-lockstep files anyway; passing none reproduces the
    /// contract the activity now asks for.
    fn pending_publication(selectors: Vec<&'a str>) -> Self {
        Self {
            disposition: if selectors.is_empty() {
                "verified_no_diff"
            } else {
                "selectors"
            },
            selectors,
            duplicate_of: Value::Null,
            already_landed: Value::Null,
            release_action_required: json!({
                "action": "publish the recorded release version, or withdraw the recorded bump, as a release operation",
                "evidence": "the smoke job resolves the version this repository already records, and the registry has no such published version",
            }),
            warnings: Vec::new(),
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
                        "release_action_required": assessment.release_action_required,
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
    let mut red_main = failure(
        10,
        "release",
        "homebrew",
        "on arm",
        "release\thomebrew\terror: old arm formula\n",
        CHECKOUT,
    );
    red_main["head_branch"] = json!("main");
    red_main["ref_kind"] = json!("release");
    let red_main_snapshot = vec![red_main];
    let old = file(&runtime, red_main_snapshot.clone());
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
        "release_promotion_or_hotfix_needed"
    );
    let disposition = &result["ci_sweep_admission"][0];
    assert_eq!(disposition["source"]["ref_kinds"], json!(["release"]));
    assert_eq!(disposition["source"]["head_branches"], json!(["main"]));
    assert_eq!(
        disposition["evidence"]["red_release"]["tested_commit"],
        CHECKOUT
    );
    assert!(
        disposition["evidence"]["covering_repair"]["evidence"]
            .as_str()
            .is_some_and(|evidence| evidence.contains(&prior.id) && evidence.contains("49740da"))
    );
    assert!(
        disposition["evidence"]["required_action"]
            .as_str()
            .is_some_and(|action| action.contains("release branch") && action.contains("hotfix"))
    );
    assert_eq!(
        runtime
            .get_task(old_filing["task_id"].as_str().expect("old id"))
            .expect("old task")
            .status,
        TaskStatus::Proposed
    );
    assert!(
        !backlog_task_ids(&runtime).contains(
            &old_filing["task_id"]
                .as_str()
                .expect("release task id")
                .to_string()
        )
    );

    let repeated = file(&runtime, red_main_snapshot);
    assert_eq!(repeated["filed_count"], json!(0));
    assert_eq!(repeated["pilot_candidate_count"], json!(1));
    assert_eq!(
        repeated["pilot_candidates"][0]["task_id"],
        old_filing["task_id"]
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
fn pending_release_publication_withholds_promotion_and_names_the_operator_action() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "release/version.json");
    write_workspace_file(&repo_root, "packaging/package.json");
    write_workspace_file(&repo_root, "Cargo.toml");
    let unpublished_version = concat!(
        "smoke\tnpm install\tnpm error code ETARGET\n",
        "smoke\tnpm install\tnpm error notarget No matching version found for @acme/cli@9.9.9.\n",
    );
    let filed = file(
        &runtime,
        vec![failure(
            20,
            "smoke-npm-install",
            "smoke",
            "npm install",
            unpublished_version,
            CHECKOUT,
        )],
    );
    let filing = &filed["filed"][0];
    let task_id = filing["task_id"].as_str().expect("task id");

    let proposed_version_edits = apply_pilot(
        &runtime,
        &repo_root,
        filing,
        Assessment::pending_publication(vec![
            "file:release/version.json",
            "file:packaging/package.json",
        ]),
    )
    .expect("apply a pilot that proposed release metadata edits");

    let admission = &proposed_version_edits["ci_sweep_admission"][0];
    assert_eq!(admission["decision"], "withhold");
    assert_eq!(
        admission["classification"],
        "release_publication_or_operator_action_needed"
    );
    assert_eq!(admission["evidence"]["automatic_action"], "none");
    assert!(
        admission["evidence"]["required_action"]
            .as_str()
            .is_some_and(|required| required.contains("publish")),
        "{admission}"
    );
    assert_eq!(
        admission["evidence"]["withheld_selectors"],
        json!(["file:release/version.json", "file:packaging/package.json"])
    );
    assert_eq!(
        admission["evidence"]["red_failure"]["tested_commit"],
        CHECKOUT
    );
    assert_eq!(admission["source"]["workflow"], "smoke-npm-install");
    assert_eq!(
        runtime.get_task(task_id).expect("task").status,
        TaskStatus::Proposed
    );
    assert!(!backlog_task_ids(&runtime).contains(&task_id.to_string()));

    // The contract shape the activity now asks for reaches the same owner
    // instead of the unproven-no-diff disposition.
    let reported_without_selectors = apply_pilot(
        &runtime,
        &repo_root,
        filing,
        Assessment::pending_publication(Vec::new()),
    )
    .expect("apply a pilot that returned no repository work");
    assert_eq!(
        reported_without_selectors["ci_sweep_admission"][0]["classification"],
        "release_publication_or_operator_action_needed"
    );

    // Repeated unchanged evidence keeps one owner with the preserved failure
    // and is re-piloted; it is never reported as a clean sweep.
    let repeated = file(
        &runtime,
        vec![failure(
            21,
            "smoke-npm-install",
            "smoke",
            "npm install",
            unpublished_version,
            NEXT_HEAD,
        )],
    );
    assert_eq!(repeated["outcome"], "current_failures");
    assert_eq!(repeated["filed_count"], json!(0));
    assert_eq!(repeated["pilot_candidate_count"], json!(1));
    assert_eq!(repeated["pilot_candidates"][0]["task_id"], task_id);

    // No filename denylist: a repository-owned dependency contract defect in
    // the same kind of manifest stays eligible once a pilot establishes it.
    let owned = file(
        &runtime,
        vec![failure(
            22,
            "ci",
            "build",
            "cargo build",
            "ci\tbuild\terror: failed to select a version for the requirement\n",
            CHECKOUT,
        )],
    );
    let owned_filing = &owned["filed"][0];
    let admitted = apply_pilot(
        &runtime,
        &repo_root,
        owned_filing,
        Assessment::actionable("file:Cargo.toml", true),
    )
    .expect("admit a repository-owned dependency repair");
    assert_eq!(
        admitted["ci_sweep_admission"][0]["classification"],
        "current_actionable_regression"
    );
    assert_eq!(
        runtime
            .get_task(owned_filing["task_id"].as_str().expect("owned task id"))
            .expect("owned task")
            .status,
        TaskStatus::Backlog
    );
}

#[test]
fn verified_no_diff_without_covering_proof_stays_uncertain_and_retryable() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
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

    let result = apply_pilot(&runtime, &repo_root, filing, Assessment::unproven_no_diff())
        .expect("retain an unproven no-diff assessment");

    assert_eq!(
        result["ci_sweep_admission"][0]["classification"],
        "covering_proof_missing"
    );
    assert!(
        result["ci_sweep_admission"][0]["evidence"]["required_action"]
            .as_str()
            .is_some_and(|action| action.contains("covering task and commit evidence"))
    );
    assert_eq!(
        runtime.get_task(task_id).expect("task").status,
        TaskStatus::Proposed
    );

    let repeated = file(
        &runtime,
        vec![failure(
            11,
            "ci",
            "build",
            "cargo build",
            "ci\tbuild\terror: current regression\n",
            NEXT_HEAD,
        )],
    );
    assert_eq!(repeated["filed_count"], json!(0));
    assert_eq!(repeated["pilot_candidate_count"], json!(1));
    assert_eq!(repeated["pilot_candidates"][0]["task_id"], task_id);
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
