//! Evidence-driven completed-owner regression tests. Git and task/run/artifact
//! storage are real disposable fixtures; no ancestry or delivery mock certifies coverage.

use super::*;
use crate::adapter::engine_host::v2_host::ci_failure_tasks::{
    cluster_failures, file_ci_failure_tasks,
};
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;
use crate::application::task::TaskAddParams;
use chrono::Utc;
use orbit_types::workflow::PipelineState;
use std::path::Path;

const DETAIL: &str = "assertion failed: tool_list.plain.txt differs: github.run.logs description";

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("fixture Git");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git UTF8")
        .trim()
        .to_string()
}

struct Fixture {
    _root: tempfile::TempDir,
    runtime: OrbitRuntime,
    repo: std::path::PathBuf,
    owner: Task,
    finding: Value,
    assessment: Value,
    before: Value,
    after: Value,
    delivery: String,
    landed: String,
    pre_fix: String,
}

impl Fixture {
    fn new() -> Self {
        let (root, runtime, repo) = runtime_with_workspace_layout();
        git(&repo, &["init", "-b", "agent-main"]);
        git(&repo, &["config", "user.name", "Fixture"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        std::fs::write(repo.join("golden.txt"), "old\n").expect("fixture");
        git(&repo, &["add", "golden.txt"]);
        git(&repo, &["commit", "-m", "before repair"]);
        let pre_fix = git(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("golden.txt"), "repaired\n").expect("fixture");
        git(&repo, &["commit", "-am", "repair"]);
        let landed = git(&repo, &["rev-parse", "HEAD"]);
        let finding = json!({"run_id":34167152200_u64,"job_id":101880348482_u64,
            "log_job_id":101880348482_u64,"workflow":"CI","head_branch":"agent-main",
            "ref_kind":"integration","actual_checkout_shas":[pre_fix],"current_ref_head_sha":landed,
            "checkout_identity":{"state":"observed","provenance":{"job_id":101880348482_u64,"complete":true}},
            "investigated":true,"log_truncated":false,"log_excerpt":format!("test plain_and_json_forms_match_their_goldens ... FAILED\n{DETAIL}\n"),
            "failed_jobs":[{"job_id":101880348482_u64,"name":"Coverage (informational)",
                "failed_steps":[{"name":"Collect workspace coverage","conclusion":"failure"}]}]});
        let cluster = cluster_failures(std::slice::from_ref(&finding)).remove(0);
        let owner = runtime
            .add_task(TaskAddParams {
                title: "Historical golden repair replay owner".into(),
                description: "Completed repair; original historical evidence contained only prose."
                    .into(),
                tags: vec![format!("ci-failure:{}", cluster.failure_key)],
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .expect("completed owner");
        let run = runtime
            .stores()
            .jobs()
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({"task_ids":[owner.id]})),
                None,
            )
            .expect("delivery");
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .expect("running");
        runtime
            .stores()
            .jobs()
            .finalize_job_run(&run.run_id, JobRunState::Success, Utc::now(), Some(1))
            .expect("success");
        let mut state = PipelineState::new(
            run.run_id.clone(),
            run.job_id.clone(),
            json!({"task_ids":[owner.id]}),
        );
        state.record_step(
            12,
            JobRunState::Success,
            Some(json!({"phase":"complete",
            "merge":{"merged":true,"landed_commit":landed},"completed_task_ids":[owner.id]})),
            None,
        );
        runtime
            .write_run_state(&run.run_id, &state)
            .expect("delivery state");
        let owner = runtime
            .update_task(
                &owner.id,
                TaskUpdateParams {
                    job_run_id: Some(Some(run.run_id.clone())),
                    ..Default::default()
                },
            )
            .expect("delivery owner");
        let command = json!([
            "cargo",
            "test",
            "-p",
            "orbit-cli",
            "--test",
            "output_goldens",
            "plain_and_json_forms_match_their_goldens",
            "--",
            "--exact"
        ]);
        let before = json!({"schema_version":1,"task_id":owner.id,"revision":pre_fix,"command":command,
            "exit_code":101,"outcome":"failed","origin":"retrospective","recorded_at":"2026-09-08T00:00:00Z",
            "output":format!("running 1 test\n{DETAIL}\ntest result: FAILED. 0 passed; 1 failed")});
        let after = json!({"schema_version":1,"task_id":owner.id,"revision":landed,"command":command,
            "exit_code":0,"outcome":"passed","origin":"retrospective","recorded_at":"2026-09-08T00:00:01Z",
            "output":"running 1 test\nplain_and_json_forms_match_their_goldens ... ok\ntest result: ok. 1 passed; 0 failed"});
        let assessment = json!({"schema_version":1,"task_id":owner.id,"failure_key":cluster.failure_key,
            "delivery_run_id":run.run_id,"delivery_step_index":12,"landed_revision":landed,"observations":observations(&cluster).expect("observations"),
            "before":{"path":"before.json","sha256":sha256(&serde_json::to_vec(&before).expect("JSON"))},
            "after":{"path":"after.json","sha256":sha256(&serde_json::to_vec(&after).expect("JSON"))},
            "command":command,"diagnostic_details":[DETAIL],
            "coverage_reason":"The golden changed with the public tool description; the exact targeted assertion fails before and passes on the landed repair."});
        Self {
            _root: root,
            runtime,
            repo,
            owner,
            finding,
            assessment,
            before,
            after,
            delivery: run.run_id,
            landed,
            pre_fix,
        }
    }

    fn enrich(&mut self) {
        for (key, value) in [("before", &self.before), ("after", &self.after)] {
            self.assessment[key]["sha256"] =
                json!(sha256(&serde_json::to_vec(value).expect("JSON")));
        }
        self.runtime
            .update_task(
                &self.owner.id,
                TaskUpdateParams {
                    upsert_artifacts: vec![
                        TaskArtifact::from_text(ASSESSMENT_PATH, self.assessment.to_string()),
                        TaskArtifact::from_text("before.json", self.before.to_string()),
                        TaskArtifact::from_text("after.json", self.after.to_string()),
                    ],
                    ..Default::default()
                },
            )
            .expect("retrospective records");
    }

    fn assess(&self) -> Assessment {
        let cluster = cluster_failures(std::slice::from_ref(&self.finding)).remove(0);
        Assessor::new(&self.runtime).assess(&cluster)
    }

    fn file(&self, findings: Vec<Value>) -> Value {
        file_ci_failure_tasks(
            &self.runtime,
            &json!({"ci_evidence":{"schema_version":2,"collected":true,
            "current_failures":findings}}),
        )
        .expect("file")
    }
}

#[test]
fn insufficient_historical_state_stays_unresolved_until_reassessment_then_files_no_owner() {
    let mut fixture = Fixture::new();
    let initial = fixture.assess();
    assert!(initial.owner.is_none());
    assert_eq!(initial.evidence["outcome"], "unresolved");
    assert!(
        initial.evidence["reason"]
            .as_str()
            .expect("reason")
            .contains("artifact_missing")
    );
    fixture.enrich();
    let output = fixture.file(vec![fixture.finding.clone()]);
    assert_eq!(output["filed_count"], 0, "{output}");
    assert_eq!(output["pilot_candidate_count"], 0);
    assert_eq!(output["skipped_existing"][0]["task_id"], fixture.owner.id);
    assert_eq!(
        output["repair_assessments"][0]["outcome"],
        "covered_by_repair"
    );
    assert_eq!(
        output["repair_assessments"][0]["observations"][0]["job_id"],
        "101880348482"
    );
    let artifacts = fixture
        .runtime
        .get_task_artifacts(&fixture.owner.id)
        .expect("artifacts");
    assert_eq!(
        artifacts
            .iter()
            .filter(|artifact| artifact.path.starts_with("ci-repair-observations/"))
            .count(),
        1
    );
    let repeated = fixture.file(vec![fixture.finding.clone()]);
    assert_eq!(output, repeated);
    assert_eq!(
        fixture
            .runtime
            .get_task_artifacts(&fixture.owner.id)
            .expect("artifacts")
            .len(),
        artifacts.len()
    );
    assert_eq!(fixture.runtime.list_tasks().expect("tasks").len(), 1);
}

#[test]
fn mismatched_assessment_and_validation_references_never_cover() {
    for fault in [
        "task",
        "diagnostic",
        "run",
        "job",
        "branch",
        "ref_kind",
        "command",
        "details",
        "prose",
        "landed",
        "validated",
        "before_revision",
        "result",
        "exit",
        "origin",
        "timestamp",
        "output",
        "digest",
        "missing",
    ] {
        let mut fixture = Fixture::new();
        match fault {
            "task" => fixture.assessment["task_id"] = json!("ORB-OTHER"),
            "diagnostic" => {
                fixture.assessment["observations"][0]["diagnostic_sha256"] = json!("wrong")
            }
            "run" => fixture.assessment["observations"][0]["run_id"] = json!("other"),
            "job" => fixture.assessment["observations"][0]["job_id"] = json!("other"),
            "branch" => fixture.assessment["observations"][0]["branch"] = json!("main"),
            "ref_kind" => fixture.assessment["observations"][0]["ref_kind"] = json!("release"),
            "command" => fixture.after["command"] = json!(["true"]),
            "details" => {
                fixture.assessment["diagnostic_details"] =
                    json!(["plain_and_json_forms_match_their_goldens"])
            }
            "prose" => {
                fixture.assessment =
                    json!({"covered":true,"evidence":"Task done and ancestry prove this is fixed"})
            }
            "landed" => fixture.assessment["landed_revision"] = json!(fixture.pre_fix),
            "validated" => fixture.after["revision"] = json!(fixture.pre_fix),
            "before_revision" => fixture.before["revision"] = json!(fixture.landed),
            "result" => fixture.after["outcome"] = json!("failed"),
            "exit" => fixture.after["exit_code"] = json!(101),
            "origin" => fixture.after["origin"] = json!("historical_prose"),
            "timestamp" => fixture.after["recorded_at"] = json!("unknown"),
            "output" => fixture.before["output"] = json!("a different assertion failed"),
            "missing" => fixture.assessment["before"]["path"] = json!("missing.json"),
            _ => {}
        }
        fixture.enrich();
        if fault == "digest" {
            fixture
                .runtime
                .update_task(
                    &fixture.owner.id,
                    TaskUpdateParams {
                        upsert_artifacts: vec![TaskArtifact::from_text("after.json", "{}")],
                        ..Default::default()
                    },
                )
                .expect("replace referenced artifact");
        }
        let result = fixture.assess();
        assert!(result.owner.is_none(), "{fault}: {}", result.evidence);
        assert_eq!(result.evidence["outcome"], "unresolved", "{fault}");
    }
}

#[test]
fn delivery_must_be_authoritative_and_bound_to_owner_and_revision() {
    for fault in ["run", "owner", "merge", "landed", "completed"] {
        let mut fixture = Fixture::new();
        fixture.enrich();
        let mut state = fixture
            .runtime
            .read_run_state(&fixture.delivery)
            .expect("state")
            .expect("present");
        match fault {
            "run" => state.run_id = "jrun-other".into(),
            "owner" => {
                fixture
                    .runtime
                    .update_task(
                        &fixture.owner.id,
                        TaskUpdateParams {
                            job_run_id: Some(Some("jrun-other".into())),
                            ..Default::default()
                        },
                    )
                    .expect("different delivery");
            }
            "merge" => {
                state.step_outputs.get_mut(&12).expect("completion")["merge"]["merged"] =
                    json!(false)
            }
            "landed" => {
                state.step_outputs.get_mut(&12).expect("completion")["merge"]["landed_commit"] =
                    json!(fixture.pre_fix)
            }
            _ => {
                state.step_outputs.get_mut(&12).expect("completion")["completed_task_ids"] =
                    json!([])
            }
        }
        fixture
            .runtime
            .write_run_state(&fixture.delivery, &state)
            .expect("state");
        assert!(fixture.assess().owner.is_none(), "{fault}");
    }
}

#[test]
fn post_fix_other_assertions_and_branches_without_repair_remain_actionable() {
    for fault in [
        "post_fix",
        "different_assertion",
        "unrepaired_branch",
        "release",
        "missing_git",
    ] {
        let mut fixture = Fixture::new();
        fixture.enrich();
        match fault {
            "post_fix" => fixture.finding["actual_checkout_shas"] = json!([fixture.landed]),
            "different_assertion" => {
                fixture.finding["log_excerpt"] = json!(format!(
                    "test plain_and_json_forms_match_their_goldens ... FAILED\n{DETAIL}; different operand"
                ))
            }
            "unrepaired_branch" => {
                git(&fixture.repo, &["branch", "-f", "main", &fixture.pre_fix]);
                fixture.finding["head_branch"] = json!("main");
                fixture.finding["ref_kind"] = json!("release");
                fixture.finding["current_ref_head_sha"] = json!(fixture.pre_fix);
            }
            "release" => {
                git(&fixture.repo, &["branch", "-f", "main", &fixture.pre_fix]);
                fixture.finding["head_branch"] = json!("main");
                fixture.finding["ref_kind"] = json!("release");
                let cluster = cluster_failures(std::slice::from_ref(&fixture.finding)).remove(0);
                fixture.assessment["observations"] =
                    json!(observations(&cluster).expect("sources"));
                fixture.enrich();
            }
            _ => {
                fixture.assessment["landed_revision"] = json!("a".repeat(40));
                fixture.after["revision"] = json!("a".repeat(40));
                fixture.enrich();
            }
        }
        let output = fixture.file(vec![fixture.finding.clone()]);
        assert_eq!(output["filed_count"], 1, "{fault}: {output}");
        assert_eq!(output["pilot_candidate_count"], 1);
        assert_eq!(fixture.runtime.list_tasks().expect("tasks").len(), 2);
    }
}

#[test]
fn reversed_observations_retain_one_receipt_and_never_admit_implementation() {
    let mut fixture = Fixture::new();
    let mut second = fixture.finding.clone();
    second["run_id"] = json!(34167152201_u64);
    let findings = vec![fixture.finding.clone(), second.clone()];
    let cluster = cluster_failures(&findings).remove(0);
    fixture.assessment["observations"] = json!(observations(&cluster).expect("sources"));
    fixture.enrich();
    let first = fixture.file(findings);
    let count = fixture
        .runtime
        .get_task_artifacts(&fixture.owner.id)
        .expect("artifacts")
        .len();
    let reversed = fixture.file(vec![second, fixture.finding.clone()]);
    assert_eq!(first["repair_assessments"], reversed["repair_assessments"]);
    assert_eq!(reversed["filed_count"], 0);
    assert_eq!(reversed["pilot_candidate_count"], 0);
    assert_eq!(
        fixture
            .runtime
            .get_task_artifacts(&fixture.owner.id)
            .expect("artifacts")
            .len(),
        count
    );
}

#[test]
fn owner_and_assessment_budgets_are_explicit() {
    let fixture = Fixture::new();
    let cluster = cluster_failures(std::slice::from_ref(&fixture.finding)).remove(0);
    let mut assessor = Assessor::new(&fixture.runtime);
    assessor.remaining = 0;
    assert!(
        assessor.assess(&cluster).evidence["reason"]
            .as_str()
            .expect("reason")
            .contains("budget")
    );
    for _ in 0..MAX_OWNERS {
        fixture
            .runtime
            .add_task(TaskAddParams {
                title: "Another historical owner".into(),
                tags: fixture.owner.tags.clone(),
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .expect("owner");
    }
    assert!(
        fixture.assess().evidence["reason"]
            .as_str()
            .expect("reason")
            .contains("lookup_budget")
    );
}

fn historical_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    let historical: Value =
        serde_json::from_str(include_str!("fixtures/ci_repair_historical.json"))
            .expect("preserved historical source");
    assert_eq!(historical["owner_task_id"], "ORB-11739");
    assert_eq!(historical["duplicate_task_id"], "ORB-11747");
    assert!(historical["original_assessment"].is_null());
    fixture.finding = historical["source"].clone();
    fixture.before = serde_json::from_str(include_str!("fixtures/ci_repair_before.json"))
        .expect("retrospective reproduction");
    fixture.after = serde_json::from_str(include_str!("fixtures/ci_repair_after.json"))
        .expect("retrospective validation");
    fixture.pre_fix = fixture.before["revision"].as_str().expect("before").into();
    fixture.landed = fixture.after["revision"].as_str().expect("after").into();
    let cluster = cluster_failures(std::slice::from_ref(&fixture.finding)).remove(0);
    assert_eq!(cluster.failure_key, historical["failure_key"]);
    // Only store-allocated task/run IDs are remapped for the disposable runtime.
    // Diagnostic bytes, source IDs, revisions and command outputs are unchanged.
    fixture.owner = fixture
        .runtime
        .update_task(
            &fixture.owner.id,
            TaskUpdateParams {
                tags: Some(vec![format!("ci-failure:{}", cluster.failure_key)]),
                ..Default::default()
            },
        )
        .expect("historical key");
    fixture.before["task_id"] = json!(fixture.owner.id);
    fixture.after["task_id"] = json!(fixture.owner.id);
    fixture.assessment["failure_key"] = json!(cluster.failure_key);
    fixture.assessment["landed_revision"] = json!(fixture.landed);
    fixture.assessment["observations"] = json!(observations(&cluster).expect("historical sources"));
    fixture.assessment["diagnostic_details"] = json!([
        "assertion `left == right` failed: ",
        "tool_list.plain.txt drifted from its golden.",
        "The source stream is drained incrementally; checkout extraction stops after 8 MiB"
    ]);
    let mut completion = historical["delivery"]["completion"].clone();
    completion["completed_task_ids"] = json!([fixture.owner.id]);
    let mut state = PipelineState::new(
        fixture.delivery.clone(),
        "task_pr_pipeline".into(),
        json!({"task_ids":[fixture.owner.id]}),
    );
    state.record_step(12, JobRunState::Success, Some(completion), None);
    fixture
        .runtime
        .write_run_state(&fixture.delivery, &state)
        .expect("historical completion shape");
    fixture
}

#[test]
fn preserved_historical_records_need_validation_and_available_revision_evidence() {
    let mut fixture = historical_fixture();
    assert!(
        fixture.assess().evidence["reason"]
            .as_str()
            .expect("reason")
            .contains("artifact_missing")
    );
    fixture.enrich();
    let cluster = cluster_failures(std::slice::from_ref(&fixture.finding)).remove(0);
    let assessment: RepairAssessment =
        serde_json::from_value(fixture.assessment.clone()).expect("assessment");
    let sources = observations(&cluster).expect("sources");
    validate_binding(&fixture.owner, &cluster, &sources, &assessment)
        .expect("exact historical diagnostic binding");
    validate_results(
        &assessment,
        &sources,
        &serde_json::from_value(fixture.before.clone()).expect("before"),
        &serde_json::from_value(fixture.after.clone()).expect("after"),
    )
    .expect("actual retrospective results");
    Assessor::new(&fixture.runtime)
        .verify_delivery(&fixture.owner, &assessment)
        .expect("actual completion contract");
    let unavailable = fixture.assess();
    assert!(unavailable.owner.is_none());
    assert!(
        unavailable.evidence["reason"]
            .as_str()
            .expect("reason")
            .contains("revision_unavailable")
    );
}

/// Explicit history dependency: ordinary CI clones need not retain these old
/// commits. The production verifier still uses real Git in the disposable repo.
#[test]
#[ignore = "requires ORBIT_CI_REPAIR_HISTORY pointing to a local checkout with historical commits"]
fn replay_historical_repair_through_filing() {
    let mut fixture = historical_fixture();
    let history = std::env::var("ORBIT_CI_REPAIR_HISTORY").expect("local history checkout");
    let history = std::fs::canonicalize(history).expect("canonical local checkout");
    assert!(history.is_dir());
    let head = fixture.finding["current_ref_head_sha"]
        .as_str()
        .expect("historical branch head");
    git(
        &fixture.repo,
        &[
            "fetch",
            "--no-tags",
            "--depth=100",
            history.to_str().expect("path"),
            head,
        ],
    );
    git(
        &fixture.repo,
        &["update-ref", "refs/heads/agent-main", head],
    );
    assert!(
        fixture.assess().owner.is_none(),
        "original insufficient records"
    );
    fixture.enrich();
    let output = fixture.file(vec![fixture.finding.clone()]);
    assert_eq!(output["filed_count"], 0, "{output}");
    assert_eq!(output["pilot_candidate_count"], 0, "{output}");
    assert_eq!(
        output["repair_assessments"][0]["outcome"], "covered_by_repair",
        "{output}"
    );
    assert_eq!(
        output["repair_assessments"][0]["validated_revision"],
        fixture.landed
    );
    assert_eq!(fixture.runtime.list_tasks().expect("tasks").len(), 1);
    assert_eq!(output, fixture.file(vec![fixture.finding.clone()]));
    let receipts = fixture
        .runtime
        .get_task_artifacts(&fixture.owner.id)
        .expect("receipt");
    assert_eq!(
        receipts
            .iter()
            .filter(|artifact| artifact.path.starts_with("ci-repair-observations/"))
            .count(),
        1
    );
}

#[test]
fn contradictory_observations_and_foreign_validation_owners_are_rejected() {
    for fault in [
        "contradictory_source",
        "before_owner",
        "after_owner",
        "delivery_step",
        "assignment",
        "failed_step",
        "oversize",
    ] {
        let mut fixture = Fixture::new();
        match fault {
            "contradictory_source" => {
                let mut duplicate = fixture.assessment["observations"][0].clone();
                duplicate["diagnostic_sha256"] = json!("a".repeat(64));
                fixture.assessment["observations"]
                    .as_array_mut()
                    .expect("sources")
                    .push(duplicate);
            }
            "before_owner" => fixture.before["task_id"] = json!("ORB-OTHER"),
            "after_owner" => fixture.after["task_id"] = json!("ORB-OTHER"),
            "delivery_step" => fixture.assessment["delivery_step_index"] = json!(9),
            "oversize" => fixture.before["output"] = json!("x".repeat(MAX_ARTIFACT_BYTES + 1)),
            _ => {
                let mut state = fixture
                    .runtime
                    .read_run_state(&fixture.delivery)
                    .expect("state")
                    .expect("present");
                if fault == "assignment" {
                    state.initial_input["task_ids"] = json!([]);
                } else {
                    state.step_states.insert(12, JobRunState::Failed);
                }
                fixture
                    .runtime
                    .write_run_state(&fixture.delivery, &state)
                    .expect("state");
            }
        }
        fixture.enrich();
        assert!(fixture.assess().owner.is_none(), "{fault}");
    }
}

#[test]
fn release_coverage_requires_the_observed_release_branch_to_contain_repair() {
    let mut fixture = Fixture::new();
    git(&fixture.repo, &["branch", "main", &fixture.landed]);
    fixture.finding["head_branch"] = json!("main");
    fixture.finding["ref_kind"] = json!("release");
    let cluster = cluster_failures(std::slice::from_ref(&fixture.finding)).remove(0);
    fixture.assessment["observations"] = json!(observations(&cluster).expect("sources"));
    fixture.enrich();
    let output = fixture.file(vec![fixture.finding.clone()]);
    assert_eq!(output["filed_count"], 0, "{output}");
    assert_eq!(output["pilot_candidate_count"], 0);
}
