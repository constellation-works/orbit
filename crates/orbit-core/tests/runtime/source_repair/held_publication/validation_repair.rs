use orbit_engine::ReviewGate;
use orbit_types::task::DecisionRequestState;

use super::*;
use crate::task_delivery::source_repair::decision_answer;

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

crate::scenario_table!(
    validation_repair_source_repair,
    stopped_publication_binds_the_validated_repair,
    before_pr_review_starts_from_the_validated_repair,
    unauthorized_validation_repairs_refuse_without_writes,
);
