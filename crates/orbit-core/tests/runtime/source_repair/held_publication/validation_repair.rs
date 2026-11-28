use std::path::Path;

use orbit_engine::ReviewGate;
use orbit_types::task::DecisionRequestState;
use orbit_types::workflow::{REVIEW_CONTRACT_VERSION, REVIEW_REPORT_ARTIFACT};

use super::*;
use crate::task_delivery::source_repair::decision_answer;
use crate::task_delivery::source_repair::failed_publication::{
    configure_before_pr_review, escalate,
};
use crate::task_delivery::source_repair::published_owner;

fn record(fixture: &mut Fixture, index: u32, id: &str, output: Value) {
    fixture.state.record_pipeline_output(id, output.clone());
    fixture
        .state
        .record_step(index, JobRunState::Success, Some(output), None);
}

fn run_validation(fixture: &Fixture) -> Result<Value, orbit_common::OrbitError> {
    execute_deterministic_action(
        &orbit_core::test_support::EngineHost::new(&fixture.runtime),
        "candidate_validate",
        &json!({}),
        &json!({
            "job_run_id": fixture.source.run_id, "completed_task_ids": [fixture.task],
            "workspace_path": fixture.repo, "base_sha": fixture.base, "base_ref": "origin/main",
        }),
        false,
        &HashMap::new(),
        None,
    )
}

fn repair_trailers(task: &str, run: &str, step: &str) -> String {
    [
        format!("Orbit-Task: {task}"),
        format!("Orbit-Recovery-Run: {run}"),
        format!("Orbit-Recovery-Step: {step}"),
        "Orbit-Recovery-Activity: step_failure_recovery".into(),
    ]
    .join("\n")
}

fn repair_commit(fixture: &Fixture, parent: &str, task: &str, run: &str, step: &str) -> String {
    let tree = git(&fixture.repo, &["rev-parse", "HEAD^{tree}"]);
    git(
        &fixture.repo,
        &[
            "commit-tree",
            &tree,
            "-p",
            parent,
            "-m",
            "fix: repair failed validation",
            "-m",
            &repair_trailers(task, run, step),
        ],
    )
}

fn repaired_publication(timing: &str) -> Fixture {
    let mut fixture = Fixture::build_with_command(
        timing,
        false,
        "17",
        Vec::new(),
        "test ! -f source.txt || test -f repaired.txt",
    );
    let parent = fixture.head.clone();
    let path = fixture
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let mut job: Value = serde_yaml::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    job["spec"]["steps"].as_array_mut().unwrap().insert(
        4,
        json!({"id":"validate","target":"activity:candidate_validate",
            "recovery_activity":"step_failure_recovery"}),
    );
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/activities/step_failure_recovery.yaml"),
        fixture
            ._root
            .path()
            .join("global/resources/activities/step_failure_recovery.yaml"),
    )
    .unwrap();
    std::fs::write(path, serde_yaml::to_string(&job).unwrap()).unwrap();

    assert!(run_validation(&fixture).is_err());
    std::fs::write(fixture.repo.join("repaired.txt"), "fixed\n").unwrap();
    git(&fixture.repo, &["add", "repaired.txt"]);
    // Reproduce the single-parent commit shape emitted by step recovery.
    let message = repair_trailers(&fixture.task, &fixture.source.run_id, "validate");
    git(
        &fixture.repo,
        &[
            "commit",
            "-qm",
            "fix: repair failed validation",
            "-m",
            &message,
        ],
    );
    fixture.head = git(&fixture.repo, &["rev-parse", "HEAD"]);
    assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD^"]), parent);
    let validation = run_validation(&fixture).unwrap();
    assert_eq!(validation["decision"], "passed");
    assert_eq!(validation["tested_head"], fixture.head);
    git(&fixture.repo, &["push", "-q", "origin", &fixture.branch]);
    let mut answer: Value =
        serde_json::from_slice(&std::fs::read(&fixture.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(fixture.head);
    std::fs::write(&fixture.answer, answer.to_string()).unwrap();

    let mut push = fixture.state.pipeline["push"].clone();
    push["local_sha"] = json!(fixture.head);
    let pr = fixture.state.pipeline["pr_open"].clone();
    record(&mut fixture, 4, "validate", validation);
    record(&mut fixture, 5, "push", push);
    record(&mut fixture, 6, "pr_open", pr);
    hold_completion(&mut fixture, DeliveryRefusal::CheckEvidenceInconclusive);
    fixture
        .runtime
        .write_run_state(&fixture.source.run_id, &fixture.state)
        .unwrap();
    fixture
}

fn stopped_publication_binds_the_validated_repair() {
    let fixture = repaired_publication("none");
    block_fixture_task(&fixture);
    let decision = decision_answer::raise_block_triage(&fixture);
    let task = fixture.runtime.get_task(&fixture.task).unwrap();
    let history = fixture.runtime.get_task_history(&fixture.task).unwrap();
    let binding = fixture.inspect();
    assert_eq!(binding["pull_request"], "17");
    assert_eq!(binding["head_sha"], fixture.head);
    assert_eq!(binding["task_id"], fixture.task);
    assert_eq!(binding["source_run_id"], fixture.source.run_id);
    assert_eq!(
        fixture
            .runtime
            .get_decision_request(&decision.id)
            .unwrap()
            .unwrap()
            .state,
        DecisionRequestState::Open
    );
    assert_eq!(fixture.runtime.get_task(&fixture.task).unwrap(), task);
    assert_eq!(
        fixture.runtime.get_task_history(&fixture.task).unwrap(),
        history
    );
    fixture.assert_original();
}

fn before_pr_review_starts_from_the_validated_repair() {
    let mut fixture = repaired_publication("before-pr");
    fixture.publish_review(false);
    // Real reviewer validation replaces the canonical receipt; the original
    // checkpoint still authenticates its own receipt through the archive.
    let reviewer_validation = run_validation(&fixture).unwrap();
    record(&mut fixture, 6, "review_validate", reviewer_validation);
    fixture
        .runtime
        .write_run_state(&fixture.source.run_id, &fixture.state)
        .unwrap();
    assert_eq!(fixture.inspect()["head_sha"], fixture.head);
}

fn unauthorized_validation_repairs_refuse_without_writes() {
    let mut fixture = repaired_publication("none");
    let binding = fixture.inspect();
    let original = fixture.state.clone();
    let parent = original.pipeline["sync_base"]["head_sha"].as_str().unwrap();
    for (case, reason) in [
        ("missing_checkpoint", "source lacks validate evidence"),
        (
            "wrong_head",
            "validation retry does not bind the repaired head",
        ),
        ("missing_log", "validation retry lacks host receipts"),
        (
            "wrong_digest",
            "validation retry receipt lacks host provenance",
        ),
        (
            "foreign_receipt",
            "validation retry references a foreign run",
        ),
        ("foreign_task", "initial publication lacks authenticated"),
        ("foreign_run", "initial publication lacks authenticated"),
        ("foreign_step", "initial publication lacks authenticated"),
        ("wrong_parent", "initial publication lacks authenticated"),
        (
            "no_repair_trailers",
            "initial publication lacks authenticated",
        ),
    ] {
        fixture.state = original.clone();
        let mut validation = fixture.state.pipeline["validate"].clone();
        let original_push = fixture.state.pipeline["push"].clone();
        match case {
            "missing_checkpoint" => {
                fixture
                    .state
                    .pipeline
                    .as_object_mut()
                    .unwrap()
                    .remove("validate");
            }
            "wrong_head" => validation["tested_head"] = json!(parent),
            "missing_log" => validation["validation"] = json!([]),
            "wrong_digest" => validation["validation"][0]["sha256"] = json!("0".repeat(64)),
            "foreign_receipt" => {
                validation["validation"][0]["path"] = json!("validation/foreign/0.json");
            }
            _ => {
                let head = if case == "no_repair_trailers" {
                    git(
                        &fixture.repo,
                        &[
                            "commit-tree",
                            "HEAD^{tree}",
                            "-p",
                            parent,
                            "-m",
                            "unrelated",
                        ],
                    )
                } else {
                    repair_commit(
                        &fixture,
                        if case == "wrong_parent" {
                            &fixture.base
                        } else {
                            parent
                        },
                        if case == "foreign_task" {
                            "foreign"
                        } else {
                            &fixture.task
                        },
                        if case == "foreign_run" {
                            "foreign"
                        } else {
                            &fixture.source.run_id
                        },
                        if case == "foreign_step" {
                            "push"
                        } else {
                            "validate"
                        },
                    )
                };
                let mut push = original_push;
                push["local_sha"] = json!(head);
                record(&mut fixture, 5, "push", push);
                validation["tested_head"] = json!(head);
            }
        }
        if case != "missing_checkpoint" {
            record(&mut fixture, 4, "validate", validation);
        }
        fixture
            .runtime
            .write_run_state(&fixture.source.run_id, &fixture.state)
            .unwrap();
        assert_refused(&fixture, &binding, case, reason);
    }

    fixture.state = original;
    fixture
        .runtime
        .write_run_state(&fixture.source.run_id, &fixture.state)
        .unwrap();
    let archive_prefix = format!("validation/{}/attempts/", fixture.source.run_id);
    let manifest = fixture
        .runtime
        .get_task_artifact_manifest(&fixture.task)
        .unwrap();
    let failure = manifest
        .iter()
        .find(|entry| entry.path.starts_with(&archive_prefix))
        .unwrap();
    orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .attach_task_validation_log(
            &fixture.task,
            &fixture.source.run_id,
            &failure.path,
            b"{}".to_vec(),
        )
        .unwrap();
    assert_refused(
        &fixture,
        &binding,
        "missing_parent_failure",
        "validation repair lacks a failed host attempt on its parent",
    );
}

fn assert_refused(fixture: &Fixture, binding: &Value, case: &str, reason: &str) {
    let task = fixture.runtime.get_task(&fixture.task).unwrap();
    let history = fixture.runtime.get_task_history(&fixture.task).unwrap();
    let remote = git(&fixture.repo, &["ls-remote", "origin", &fixture.branch]);
    for request in [fixture.request("inspect"), fixture.submit(binding.clone())] {
        let error = fixture
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", request)
            .unwrap_err();
        assert!(error.to_string().contains(reason), "{case}: {error}");
    }
    assert_eq!(
        fixture.runtime.get_task(&fixture.task).unwrap(),
        task,
        "{case}"
    );
    assert_eq!(
        fixture.runtime.get_task_history(&fixture.task).unwrap(),
        history
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", &fixture.branch]),
        fixture.head
    );
    assert_eq!(
        git(&fixture.repo, &["ls-remote", "origin", &fixture.branch]),
        remote
    );
    fixture.assert_original();
}

fn run_validation_in(
    fixture: &Fixture,
    run_id: &str,
    workspace: &Path,
    base_sha: &str,
) -> Result<Value, orbit_common::OrbitError> {
    execute_deterministic_action(
        &orbit_core::test_support::EngineHost::new(&fixture.runtime),
        "candidate_validate",
        &json!({}),
        &json!({
            "job_run_id": run_id, "completed_task_ids": [fixture.task],
            "workspace_path": workspace, "base_sha": base_sha, "base_ref": "origin/main",
        }),
        false,
        &HashMap::new(),
        None,
    )
}

fn handoff_after_validation_repair(with_review: bool, preservation: bool) -> (Fixture, String) {
    let mut f = Fixture::build_with_command(
        if with_review { "before-pr" } else { "none" },
        false,
        "17",
        Vec::new(),
        "test ! -f qa-inventory.txt || test -f repaired.txt",
    );
    std::fs::write(
        f._root.path().join("bin/gh"),
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n 'pr list') printf '['; cat '{}'; printf ']';;\n 'pr create') echo https://github.com/owner/repository/pull/17;;\n 'pr view') cat '{}';;\n *) exit 99;;\nesac\n",
            f.answer.display(),
            f.answer.display()
        ),
    )
    .unwrap();
    let definition_path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let mut job: Value =
        serde_yaml::from_str(&std::fs::read_to_string(&definition_path).unwrap()).unwrap();
    job["spec"]["failure_activity"] = json!("pr_failure_handoff");
    let steps = job["spec"]["steps"].as_array_mut().unwrap();
    steps.insert(
        4,
        json!({"id":"validate","target":"activity:candidate_validate",
            "recovery_activity":"step_failure_recovery"}),
    );
    if with_review {
        steps.insert(
            5,
            json!({"id":"review_gate_settle","target":"activity:review_gate_settle"}),
        );
        steps.insert(
            6,
            json!({
                "id": "review_validate",
                "target": "activity:candidate_validate",
                "default_input": {
                    "ownership_base_sha":
                        "{{ steps.review_gate_settle.output.implementation_head_sha }}"
                }
            }),
        );
    }
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/activities/step_failure_recovery.yaml"),
        f._root
            .path()
            .join("global/resources/activities/step_failure_recovery.yaml"),
    )
    .unwrap();
    std::fs::write(&definition_path, serde_yaml::to_string(&job).unwrap()).unwrap();

    let mut run = orbit_core::test_support::EngineHost::new(&f.runtime)
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({
                "task_ids": [f.task],
                "base_branch": "main",
                "completion": "review",
                "auto_fix": false
            })),
            None,
        )
        .unwrap();
    run.state = JobRunState::Running;
    run.pid = Some(std::process::id());
    published_owner::save_run(&f, &run);
    f.runtime
        .write_run_state(
            &run.run_id,
            &orbit_types::workflow::PipelineState::new(
                run.run_id.clone(),
                run.job_id.clone(),
                run.input.clone().unwrap(),
            ),
        )
        .unwrap();
    orbit_core::test_support::EngineHost::new(&f.runtime)
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":run.run_id,"run_id":run.run_id,
            "task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    published_owner::checkpoint(&f, &run.run_id, 0, "worktree", setup.clone());
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    f.branch = setup["head_ref"].as_str().unwrap().into();
    let base_sha = setup["base_sha"].as_str().unwrap().to_string();
    std::fs::write(checkout.join("qa-inventory.txt"), "needs repair\n").unwrap();
    orbit_core::test_support::EngineHost::new(&f.runtime)
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                execution_summary: Some(
                    "Outcome: success\nChanges: prepare candidate.\nAssessment: validate required."
                        .into(),
                ),
                ..Default::default()
            },
        )
        .unwrap();
    let commit = f.action(
        "git_commit",
        json!({"job_run_id":run.run_id,"scope":"all",
            "workspace_path":checkout,"base_sha":base_sha,"base_ref":setup["base_ref"]}),
    );
    published_owner::checkpoint(&f, &run.run_id, 1, "commit", commit);
    let prepared = f.action(
        "pr_prepare",
        json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],"workspace_path":checkout,
            "head":f.branch,"base":"main","base_ref":setup["base_ref"]}),
    );
    published_owner::checkpoint(&f, &run.run_id, 2, "prepare_branch", prepared.clone());
    let mut sync_input = prepared.clone();
    sync_input["job_run_id"] = json!(run.run_id);
    sync_input["workspace_path"] = json!(checkout);
    sync_input["completed_task_ids"] = json!([f.task]);
    sync_input["preparation"] = prepared;
    let sync = f.action("git_rebase", sync_input);
    published_owner::checkpoint(&f, &run.run_id, 3, "sync_base", sync.clone());
    let synced_head = sync["head_sha"].as_str().unwrap().to_string();

    assert!(run_validation_in(&f, &run.run_id, &checkout, &base_sha).is_err());
    std::fs::write(checkout.join("repaired.txt"), "fixed\n").unwrap();
    git(&checkout, &["add", "repaired.txt"]);
    let message = repair_trailers(&f.task, &run.run_id, "validate");
    git(
        &checkout,
        &[
            "commit",
            "-qm",
            "fix: repair failed validation",
            "-m",
            &message,
        ],
    );
    let repaired_head = git(&checkout, &["rev-parse", "HEAD"]);
    assert_eq!(git(&checkout, &["rev-parse", "HEAD^"]), synced_head);
    let validation = run_validation_in(&f, &run.run_id, &checkout, &base_sha).unwrap();
    assert_eq!(validation["decision"], "passed");
    assert_eq!(validation["tested_head"], repaired_head);
    published_owner::checkpoint(&f, &run.run_id, 4, "validate", validation);

    let (failed_index, parent) = if with_review {
        configure_before_pr_review(&f, &run.run_id);
        let mut gate = json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],
            "workspace_path":checkout,"base_sha":base_sha,"base_ref":setup["base_ref"],
            "base":"main","base_sync":"remote","mode":"pr"});
        let admission = f.action("review_gate_admit", gate.clone());
        assert_eq!(admission["applies"], true);
        std::fs::write(checkout.join("review.txt"), "reviewer fix\n").unwrap();
        let report = json!({
            "schema_version": REVIEW_CONTRACT_VERSION,
            "attempt_id": admission["attempt_id"],
            "verdict": "accept_with_fixes",
            "summary": "Fix review nit",
            "findings": [{
                "id": "F1",
                "severity": "medium",
                "summary": "Add review note",
                "paths": ["review.txt"],
                "disposition": "repaired",
                "change": "Added review note"
            }],
            "validation": [{
                "id": "V1",
                "command": "test ! -f qa-inventory.txt || test -f repaired.txt",
                "outcome": "passed",
                "role": "required"
            }],
            "escalation": null
        });
        let path = f.repo.join(".orbit/tmp/handoff-review-report.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, report.to_string()).unwrap();
        f.runtime
            .run_tool(
                "orbit.task.artifact.put",
                json!({
                    "id": f.task,
                    "model": "codex",
                    "path": REVIEW_REPORT_ARTIFACT,
                    "source_path": path
                }),
            )
            .unwrap();
        gate["admission"] = admission;
        let settled = f.action("review_gate_settle", gate);
        assert_eq!(settled["gate"], "passed");
        assert_eq!(settled["reviewer_fixed"], true);
        assert_eq!(settled["implementation_head_sha"], repaired_head);
        published_owner::checkpoint(&f, &run.run_id, 5, "review_gate_settle", settled.clone());
        let review_validated = execute_deterministic_action(
            &orbit_core::test_support::EngineHost::new(&f.runtime),
            "candidate_validate",
            &json!({}),
            &json!({
                "job_run_id": run.run_id, "completed_task_ids": [f.task],
                "workspace_path": checkout, "base_sha": base_sha, "base_ref": "origin/main",
                "ownership_base_sha": repaired_head,
            }),
            false,
            &HashMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            review_validated["tested_head"],
            settled["reviewed_head_sha"]
        );
        published_owner::checkpoint(&f, &run.run_id, 6, "review_validate", review_validated);
        let pushed = f.action(
            "git_push",
            json!({"job_run_id":run.run_id,"workspace_path":checkout,
                "branch":f.branch,"base":"main","base_ref":setup["base_ref"]}),
        );
        published_owner::checkpoint(&f, &run.run_id, 7, "push", pushed);
        (
            8u32,
            settled["reviewed_head_sha"].as_str().unwrap().to_string(),
        )
    } else {
        let pushed = f.action(
            "git_push",
            json!({"job_run_id":run.run_id,"workspace_path":checkout,
                "branch":f.branch,"base":"main","base_ref":setup["base_ref"]}),
        );
        published_owner::checkpoint(&f, &run.run_id, 5, "push", pushed);
        (6u32, repaired_head)
    };

    let gh = f._root.path().join("bin/gh");
    let healthy = std::fs::read_to_string(&gh).unwrap();
    std::fs::write(
        &gh,
        healthy.replace(
            "echo https://github.com/owner/repository/pull/17",
            "echo 'provider unavailable' >&2; exit 1",
        ),
    )
    .unwrap();
    let error = execute_deterministic_action(
        &orbit_core::test_support::EngineHost::new(&f.runtime),
        "pr_open",
        &json!({}),
        &json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],
            "workspace_path":checkout,"head":f.branch,"base":"main",
            "base_ref":setup["base_ref"],"base_sha":base_sha,
            "reviewed_head_sha":parent,"reviewed_base_sha":base_sha}),
        false,
        &HashMap::new(),
        None,
    )
    .unwrap_err();
    std::fs::write(&gh, healthy).unwrap();

    let mut state = f.runtime.read_run_state(&run.run_id).unwrap().unwrap();
    state.record_step(
        failed_index,
        JobRunState::Failed,
        None,
        Some(json!(error.to_string())),
    );
    f.runtime.write_run_state(&run.run_id, &state).unwrap();
    f.source = run.clone();
    escalate(&f, &run.run_id);
    if preservation {
        std::fs::write(checkout.join("preserved.txt"), "handoff repair\n").unwrap();
    }
    let handoff = f.action(
        "pr_failure_handoff",
        json!({"run_id":run.run_id,"failed_step_id":"pr_open",
            "activity_name":"pr_open","error_code":"execution",
            "error_message":error.to_string(),"job_input":run.input,"pipeline":state.pipeline}),
    );
    assert_eq!(handoff["decision"], "blocked_failure_pr");
    assert_eq!(handoff["preservation_commit_created"], preservation);
    if preservation {
        assert_eq!(git(&checkout, &["rev-parse", "HEAD^"]), parent);
    } else {
        assert_eq!(handoff["head_sha"], parent);
    }
    state.record_failure_activity(
        "pr_failure_handoff".into(),
        "pr_open".into(),
        handoff.clone(),
    );
    f.runtime.write_run_state(&run.run_id, &state).unwrap();
    f.head = handoff["head_sha"].as_str().unwrap().into();
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefName"] = json!(f.branch);
    answer["headRefOid"] = json!(f.head);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    run = f.runtime.show_job_run_observed(&run.run_id).unwrap();
    run.state = JobRunState::Failed;
    run.pid = None;
    run.finished_at = Some(Utc::now());
    published_owner::save_run(&f, &run);
    f.source = run;
    f.state = state;
    git(&f.repo, &["worktree", "remove", checkout.to_str().unwrap()]);
    git(&f.repo, &["branch", "-D", &f.branch]);
    (f, parent)
}

fn failure_handoff_authenticates_validation_repair_and_review() {
    for (with_review, preservation) in [(false, false), (false, true), (true, false), (true, true)]
    {
        let (fixture, parent) = handoff_after_validation_repair(with_review, preservation);
        let task = fixture.runtime.get_task(&fixture.task).unwrap();
        let history = fixture.runtime.get_task_history(&fixture.task).unwrap();
        let binding = fixture.inspect();
        assert_eq!(binding["pull_request"], "17");
        assert_eq!(binding["head_sha"], fixture.head);
        assert_eq!(binding["task_id"], fixture.task);
        assert_eq!(binding["source_run_id"], fixture.source.run_id);
        if preservation {
            assert_ne!(fixture.head, parent);
        } else {
            assert_eq!(fixture.head, parent);
        }
        assert_eq!(fixture.runtime.get_task(&fixture.task).unwrap(), task);
        assert_eq!(
            fixture.runtime.get_task_history(&fixture.task).unwrap(),
            history
        );
        fixture.assert_original();
    }
}

fn failure_handoff_refuses_unauthenticated_validation_repair_without_writes() {
    let (mut fixture, _) = handoff_after_validation_repair(false, false);
    let binding = fixture.inspect();
    let original = fixture.state.clone();
    let remote = git(&fixture.repo, &["ls-remote", "origin", &fixture.branch]);

    for (case, reason) in [
        (
            "missing_validate_checkpoint",
            "failure-handoff candidate lacks an authenticated preservation parent",
        ),
        (
            "wrong_digest",
            "validation retry receipt lacks host provenance",
        ),
        (
            "foreign_receipt",
            "validation retry references a foreign run",
        ),
    ] {
        fixture.state = original.clone();
        match case {
            "missing_validate_checkpoint" => {
                fixture
                    .state
                    .pipeline
                    .as_object_mut()
                    .unwrap()
                    .remove("validate");
            }
            "wrong_digest" => {
                let mut validation = fixture.state.pipeline["validate"].clone();
                validation["validation"][0]["sha256"] = json!("0".repeat(64));
                record(&mut fixture, 4, "validate", validation);
            }
            "foreign_receipt" => {
                let mut validation = fixture.state.pipeline["validate"].clone();
                validation["validation"][0]["path"] = json!("validation/foreign/0.json");
                record(&mut fixture, 4, "validate", validation);
            }
            _ => unreachable!(),
        }
        fixture
            .runtime
            .write_run_state(&fixture.source.run_id, &fixture.state)
            .unwrap();
        let task = fixture.runtime.get_task(&fixture.task).unwrap();
        let history = fixture.runtime.get_task_history(&fixture.task).unwrap();
        for request in [fixture.request("inspect"), fixture.submit(binding.clone())] {
            let error = fixture
                .runtime
                .run_tool_as_human("orbit.workflow.run.resume", request)
                .unwrap_err();
            assert!(error.to_string().contains(reason), "{case}: {error}");
        }
        assert_eq!(
            fixture.runtime.get_task(&fixture.task).unwrap(),
            task,
            "{case}"
        );
        assert_eq!(
            fixture.runtime.get_task_history(&fixture.task).unwrap(),
            history
        );
        assert_eq!(
            git(&fixture.repo, &["ls-remote", "origin", &fixture.branch]),
            remote
        );
        fixture.assert_original();
    }

    fixture.state = original;
    fixture
        .runtime
        .write_run_state(&fixture.source.run_id, &fixture.state)
        .unwrap();
    let archive_prefix = format!("validation/{}/attempts/", fixture.source.run_id);
    let manifest = fixture
        .runtime
        .get_task_artifact_manifest(&fixture.task)
        .unwrap();
    let failure = manifest
        .iter()
        .find(|entry| entry.path.starts_with(&archive_prefix))
        .unwrap();
    orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .attach_task_validation_log(
            &fixture.task,
            &fixture.source.run_id,
            &failure.path,
            b"{}".to_vec(),
        )
        .unwrap();
    let error = fixture
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", fixture.request("inspect"))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("validation repair lacks a failed host attempt on its parent"),
        "{error}"
    );
}

crate::scenario_table!(
    validation_repair_source_repair,
    stopped_publication_binds_the_validated_repair,
    before_pr_review_starts_from_the_validated_repair,
    unauthorized_validation_repairs_refuse_without_writes,
    failure_handoff_authenticates_validation_repair_and_review,
    failure_handoff_refuses_unauthenticated_validation_repair_without_writes,
);
