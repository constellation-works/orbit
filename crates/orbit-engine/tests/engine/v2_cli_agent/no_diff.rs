use std::sync::atomic::{AtomicBool, Ordering};

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskArtifact};
use orbit_types::workflow::activity_job::V2AuditEventKind;
use serde_json::json;

use super::*;
use crate::git_fixture;

struct NoDiffHost {
    cli: ScriptHost,
    task: Task,
    committed: AtomicBool,
}

impl RuntimeHost for NoDiffHost {
    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        self.cli.resolve_cli_executor(provider)
    }

    fn get_task(&self, _: &str) -> Result<Task, OrbitError> {
        Ok(self.task.clone())
    }

    fn get_task_artifacts(&self, _: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        Ok(Vec::new())
    }

    fn run_deterministic(
        &self,
        action: &str,
        _: &Value,
        _: &Value,
        _: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        assert_eq!(action, "delivery_sentinel");
        self.committed.store(true, Ordering::SeqCst);
        Ok(json!({}))
    }
}

/// A claimed unchanged base needs response artifacts at implement_one, while
/// an edited implementation keeps the best-effort default (ORB-15386).
#[test]
fn missing_no_diff_fields_fail_at_implementation_and_preserve_raw_response() {
    git_fixture::isolated(
        module_path!(),
        "missing_no_diff_fields_fail_at_implementation_and_preserve_raw_response",
        || {
            let evidence = json!({
                "execution_summary": "Validated the unchanged base.",
                "no_diff_artifacts": [{
                    "path":"no-diff.json", "source_path":".orbit/tmp/no-diff.json",
                }],
            });
            let cases = [
                ("prose-led envelope", evidence, true, "clean", true),
                (
                    "missing artifacts",
                    json!({"execution_summary":"done"}),
                    true,
                    "clean",
                    false,
                ),
                (
                    "empty artifacts",
                    json!({"no_diff_artifacts":[]}),
                    true,
                    "clean",
                    false,
                ),
                ("invalid response", json!([]), true, "clean", false),
                ("advisory edited response", json!([]), true, "tracked", true),
                ("advisory staged response", json!([]), true, "staged", true),
                ("advisory new file", json!([]), true, "untracked", true),
                ("already committed", json!([]), true, "committed", true),
                (
                    "owner artifacts are durable",
                    json!([]),
                    false,
                    "clean",
                    true,
                ),
                ("tagged no diff", json!({}), true, "tagged", true),
                (
                    "terminal task",
                    json!({"skipped_reason":"task_terminal"}),
                    true,
                    "terminal",
                    true,
                ),
            ];
            for (label, result, claimed, state, succeeds) in cases {
                let fixture = tempfile::tempdir().unwrap();
                let repo = fixture.path().join("repo");
                fs::create_dir(&repo).unwrap();
                git_fixture::init(&repo);
                git_fixture::run(&repo, &["config", "user.name", "Fixture"]);
                git_fixture::run(&repo, &["config", "user.email", "fixture@example.invalid"]);
                fs::write(repo.join("file.txt"), "base\n").unwrap();
                git_fixture::run(&repo, &["add", "file.txt"]);
                git_fixture::run(&repo, &["commit", "-qm", "base"]);
                let base = git_fixture::run(&repo, &["rev-parse", "HEAD"]);
                // Scratch is not ignored in this repository; it still isn't delivery.
                fs::create_dir_all(repo.join(".orbit/tmp")).unwrap();
                fs::write(repo.join(".orbit/tmp/log.json"), "{}").unwrap();
                match state {
                    "tracked" | "staged" | "committed" => {
                        fs::write(repo.join("file.txt"), "candidate\n").unwrap();
                        if state != "tracked" {
                            git_fixture::run(&repo, &["add", "file.txt"]);
                        }
                        if state == "committed" {
                            git_fixture::run(&repo, &["commit", "-qm", "candidate"]);
                        }
                    }
                    "untracked" => fs::write(repo.join("new.txt"), "candidate").unwrap(),
                    _ => {}
                }
                let before = git_fixture::run(&repo, &["status", "--porcelain"]);
                let stdout = json!({"text":format!(
                "Work complete. execution_summary: prose-only; no_diff_artifacts: fabricated.\n{}",
                json!({"schemaVersion":1,"status":"success","result":result,"error":null})
            ),"stopReason":"stop"})
                .to_string();
                let fake = fake_cli(
                    "grok",
                    &format!("#!/bin/sh\ncat > /dev/null\ncat <<'PAYLOAD'\n{stdout}\nPAYLOAD\n"),
                )
                .unwrap();
                let task: Task = serde_json::from_value(json!({
                    "id":"fixture-task", "title":"Fixture", "description":"Fixture",
                    "status":if state == "terminal" {"done"} else {"in-progress"},
                    "priority":"medium", "task_type":"bug", "context_files":[],
                    "tags":if state == "tagged" {vec!["no-diff-expected"]} else {vec![]},
                    "created_at":chrono::Utc::now(), "updated_at":chrono::Utc::now(),
                }))
                .unwrap();
                let host = NoDiffHost {
                    cli: ScriptHost::new(fake.cli_path()),
                    task,
                    committed: AtomicBool::new(false),
                };
                let (writer, _) =
                    build_writer(&fixture.path().join("audit"), "no-diff-fixture").unwrap();
                let mut spec = cli_agent_loop_spec(Some(Provider::Grok));
                spec.model = None;
                assert!(!spec.require_response_envelope);
                let mut job = load_job_asset(&json!({
                "schemaVersion":2,"kind":"Job","metadata":{"name":"no_diff_fixture"},
                "spec":{"state":"enabled","kind":"workflow","steps":[
                    {"id":"implement_one","spec":ActivityV2Spec::AgentLoop(spec)},
                    {"id":"git_commit","spec":{"type":"deterministic","action":"delivery_sentinel","config":{}}},
                ]},
            }).to_string()).unwrap().spec;
                // Exercise catalog identity too, with a different step id.
                let step_id = if state == "tagged" {
                    "custom_implement"
                } else {
                    "implement_one"
                };
                job.steps[0].id = step_id.to_string();
                if let JobV2StepBody::Target(target) = &mut job.steps[0].body {
                    target.activity_name = Some("agent_implement".into());
                }
                let outcome = execute_job_with_resume(&job, json!({
                "task_id":"fixture-task","workspace_path":repo,"base_sha":base,"claimed":claimed,
            }), "no-diff-fixture", writer.clone(), &host, None).unwrap();
                assert_eq!(outcome.success, succeeds, "{label}: {outcome:?}");
                assert_eq!(
                    host.committed.load(Ordering::SeqCst),
                    succeeds,
                    "{label}: delivery runs only after an accepted implementation"
                );
                let output = &outcome.pipeline[step_id];
                assert_eq!(output["completion_envelope_satisfied"], true, "{label}");
                assert_eq!(output["response_envelope_required"], false, "{label}");
                assert_eq!(
                    output["stdout_text"],
                    format!("{stdout}\n"),
                    "{label}: retain the raw provider capture"
                );
                let blobs = orbit_common::storage::blob_store::BlobStore::new(
                    fixture.path().join("audit/audit/blobs"),
                );
                assert_eq!(
                    blobs
                        .read(output["stdout_blob_ref"].as_str().unwrap())
                        .unwrap(),
                    format!("{stdout}\n").as_bytes(),
                    "{label}: retain raw stdout bytes in the audit blob"
                );
                if label == "prose-led envelope" {
                    assert_eq!(output["execution_summary"], result["execution_summary"]);
                    assert_eq!(output["no_diff_artifacts"], result["no_diff_artifacts"]);
                    assert_eq!(output["response_envelope_valid"], true);
                }
                if !succeeds {
                    let diagnostic = outcome.message.as_deref().unwrap();
                    assert!(
                        diagnostic.contains("response_envelope_error")
                            && diagnostic.contains("no_diff_artifacts"),
                        "{label}: {diagnostic}"
                    );
                    assert!(
                        output["response_envelope_error"]
                            .as_str()
                            .unwrap()
                            .contains("no_diff_artifacts")
                    );
                    assert!(writer.events_snapshot().unwrap().iter().any(|event| matches!(
                    &event.kind, V2AuditEventKind::StepFinished {step_id, outcome, error_message}
                    if step_id == "implement_one" && outcome == "failed"
                        && error_message.as_deref().is_some_and(|message| message.contains("response_envelope_error"))
                )), "{label}: attribute the failure to implement_one");
                }
                assert_eq!(
                    git_fixture::run(&repo, &["status", "--porcelain"]),
                    before,
                    "{label}: the guard never stages or edits the worktree"
                );
            }
        },
    );
}

struct SummaryDeliveryHost {
    cli: ScriptHost,
    task: std::sync::Mutex<Task>,
    history: std::sync::Mutex<Vec<orbit_types::task::TaskHistoryEntry>>,
    state: std::sync::Mutex<orbit_types::workflow::PipelineState>,
}

impl RuntimeHost for SummaryDeliveryHost {
    fn read_run_state(
        &self,
        _: &str,
    ) -> Result<Option<orbit_types::workflow::PipelineState>, OrbitError> {
        Ok(Some(self.state.lock().unwrap().clone()))
    }

    fn checkpoint_step(
        &self,
        _: &str,
        _: u32,
        step_id: &str,
        output: &Value,
        compound_outputs: &std::collections::BTreeMap<String, Value>,
    ) -> Result<(), DispatchError> {
        let mut state = self.state.lock().unwrap();
        state.record_pipeline_output(step_id, output.clone());
        for (key, value) in compound_outputs {
            state.record_pipeline_output(key, value.clone());
        }
        Ok(())
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        self.cli.resolve_cli_executor(provider)
    }

    fn get_task(&self, _: &str) -> Result<Task, OrbitError> {
        Ok(self.task.lock().unwrap().clone())
    }

    fn list_run_tasks(&self, _: &str) -> Result<Vec<Task>, OrbitError> {
        Ok(vec![self.task.lock().unwrap().clone()])
    }

    fn get_job_run(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::JobRun>, OrbitError> {
        Ok(Some(
            serde_json::from_value(json!({
                "run_id":run_id, "job_id":"summary-fixture", "attempt":1,
                "state":"running", "scheduled_at":chrono::Utc::now(),
                "created_at":chrono::Utc::now(), "resolved_crew":"fixture-crew",
            }))
            .unwrap(),
        ))
    }

    fn apply_task_automation_update(
        &self,
        _: &str,
        update: orbit_engine::TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        let mut task = self.task.lock().unwrap();
        if let Some(expected) = update.expected_status {
            assert_eq!(task.status, expected);
        }
        if let Some(summary) = update.execution_summary {
            task.execution_summary = summary;
        }
        if let Some(crew) = update.implemented_by {
            task.implemented_by = Some(crew);
        }
        if let Some(status) = update.status {
            task.status = status;
        }
        if let Some(event) = update.status_event {
            self.history
                .lock()
                .unwrap()
                .push(orbit_types::task::TaskHistoryEntry {
                    at: chrono::Utc::now(),
                    by: "system".into(),
                    event,
                    note: update.status_note,
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                });
        }
        Ok(())
    }

    fn update_task_from_activity(
        &self,
        _: &str,
        update: orbit_engine::TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        let mut task = self.task.lock().unwrap();
        assert_eq!(task.status, update.expected_status);
        task.status = update.status;
        Ok(task.clone())
    }

    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        _: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        orbit_engine::execute_deterministic_action(
            self,
            action,
            config,
            input,
            false,
            &std::collections::HashMap::new(),
            None,
        )
        .map_err(|error| DispatchError::JobExecution(error.to_string()))
    }
}

/// The real delivery actions consume a stub CLI's envelope, persist its
/// summary, and finish a tagged no-diff task without inventing a commit.
#[test]
fn returned_summary_delivers_no_diff_task_and_missing_summaries_still_refuse() {
    git_fixture::isolated(
        module_path!(),
        "returned_summary_delivers_no_diff_task_and_missing_summaries_still_refuse",
        || {
            let returned =
                "Outcome: success\n\nValidated the requested behavior; no change required.";
            let persisted = "Outcome: success\n\nThe durable account takes precedence.";
            for (label, durable, result, succeeds, writes) in [
                // The returned execution_summary outranks the short summary.
                (
                    "envelope only",
                    "",
                    json!({"execution_summary":returned, "summary":"Short result."}),
                    true,
                    true,
                ),
                // F2026-10-267: the implementer returned only its short
                // `summary`, which is still its own account of the work.
                (
                    "short summary only",
                    "",
                    json!({"summary":returned}),
                    true,
                    true,
                ),
                (
                    "persisted wins",
                    persisted,
                    json!({"execution_summary":returned}),
                    true,
                    false,
                ),
                ("no summary", "", json!({}), false, false),
                (
                    "placeholder",
                    "TBD",
                    json!({"execution_summary":"TODO"}),
                    false,
                    false,
                ),
                (
                    "returned failure",
                    "",
                    json!({"execution_summary":"Outcome: failed\nValidation failed."}),
                    false,
                    true,
                ),
                (
                    "durable failure",
                    "Outcome: failed\nValidation failed.",
                    json!({"execution_summary":returned}),
                    false,
                    false,
                ),
            ] {
                let fixture = tempfile::tempdir().unwrap();
                let repo = fixture.path().join("repo");
                fs::create_dir(&repo).unwrap();
                git_fixture::init(&repo);
                git_fixture::run(&repo, &["config", "user.name", "Fixture"]);
                git_fixture::run(&repo, &["config", "user.email", "fixture@example.invalid"]);
                fs::write(repo.join("file.txt"), "base\n").unwrap();
                git_fixture::run(&repo, &["add", "file.txt"]);
                git_fixture::run(&repo, &["commit", "-qm", "base"]);
                let base = git_fixture::run(&repo, &["rev-parse", "HEAD"]);
                let stdout = json!({"text":json!({
                "schemaVersion":1,"status":"success","result":result,"error":null,
            }).to_string(),"stopReason":"stop"})
                .to_string();
                let fake = fake_cli(
                    "grok",
                    &format!("#!/bin/sh\ncat > /dev/null\ncat <<'PAYLOAD'\n{stdout}\nPAYLOAD\n"),
                )
                .unwrap();
                let task: Task = serde_json::from_value(json!({
                    "id":"fixture-task", "title":"Fixture", "description":"Fixture",
                    "status":"in-progress", "priority":"medium", "task_type":"chore",
                    "tags":["no-diff-expected"], "context_files":[],
                    "execution_summary":durable, "job_run_id":"summary-fixture",
                    "created_at":chrono::Utc::now(), "updated_at":chrono::Utc::now(),
                }))
                .unwrap();
                let host = SummaryDeliveryHost {
                    cli: ScriptHost::new(fake.cli_path()),
                    task: std::sync::Mutex::new(task),
                    history: std::sync::Mutex::new(Vec::new()),
                    state: std::sync::Mutex::new(orbit_types::workflow::PipelineState::new(
                        "summary-fixture".into(),
                        "summary_fixture".into(),
                        json!({}),
                    )),
                };
                let (writer, _) =
                    build_writer(&fixture.path().join("audit"), "summary-fixture").unwrap();
                let mut spec = cli_agent_loop_spec(Some(Provider::Grok));
                spec.model = None;
                // Resolve the commit inputs from the actual local and PR assets,
                // with the implement bundle checkpointed before commit, rather
                // than supplying the summary to the gate directly.
                for asset in ["task_local_pipeline", "task_pr_pipeline"] {
                    let source = fs::read_to_string(
                        Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join(format!("../orbit-core/assets/jobs/{asset}.yaml")),
                    )
                    .unwrap();
                    let asset: Value = serde_yaml::from_str(&source).unwrap();
                    let mut commit = asset["spec"]["steps"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|step| step["id"] == "commit")
                        .unwrap()["default_input"]
                        .clone();
                    commit["job_run_id"] = json!("summary-fixture");
                    commit["workspace_path"] = json!(repo);
                    commit["base_sha"] = json!(base);
                    commit.as_object_mut().unwrap().remove("base_ref");
                    let job = load_job_asset(&json!({
                    "schemaVersion":2,"kind":"Job","metadata":{"name":"summary_fixture"},
                    "spec":{"state":"enabled","kind":"workflow","steps":[
                        {"id":"implement_bundle","loop":{"items":"{{ input.task_ids }}","max_iterations":1,"steps":[
                            {"id":"implement_one","spec":ActivityV2Spec::AgentLoop(spec.clone())},
                        ]}},
                        {"id":"commit","spec":{"type":"deterministic","action":"git_commit","config":{}},"default_input":commit},
                        {"id":"promote","spec":{"type":"deterministic","action":"pr_promote","config":{}},"default_input":{
                            "job_run_id":"summary-fixture","completed_task_ids":["fixture-task"],
                            "workspace_path":repo,"no_diff_expected":"{{ steps.commit.output.skipped_no_diff_expected }}",
                        }},
                        {"id":"complete","spec":{"type":"deterministic","action":"pr_complete","config":{}},"default_input":{
                            "job_run_id":"summary-fixture","completed_task_ids":["fixture-task"],
                            "workspace_path":repo,"no_diff_expected":"{{ steps.commit.output.skipped_no_diff_expected }}",
                        }},
                    ]},
                }).to_string()).unwrap().spec;
                    let outcome = execute_job_with_resume(
                        &job,
                        json!({
                            "task_ids":["fixture-task"],"task_id":"fixture-task","workspace_path":repo,
                        }),
                        "summary-fixture",
                        writer.clone(),
                        &host,
                        None,
                    );
                    if succeeds {
                        let outcome = outcome.unwrap_or_else(|error| panic!("{label}: {error:?}"));
                        assert!(outcome.success, "{label}: {outcome:?}");
                        assert_eq!(outcome.pipeline["commit"]["committed"], false);
                        assert_eq!(outcome.pipeline["commit"]["skipped_no_diff_expected"], true);
                        let task = host.get_task("fixture-task").unwrap();
                        assert_eq!(task.status, orbit_types::task::TaskStatus::Done);
                        assert_eq!(
                            task.execution_summary,
                            if writes { returned } else { durable }
                        );
                    } else {
                        assert_eq!(
                            host.get_task("fixture-task").unwrap().status,
                            orbit_types::task::TaskStatus::InProgress
                        );
                        let message = outcome.unwrap_err().to_string();
                        assert!(
                            message.contains(if label.contains("failure") {
                                "cannot be delivered"
                            } else {
                                "requires a meaningful persisted execution_summary before delivery"
                            }),
                            "{label}: {message}"
                        );
                    }
                    assert_eq!(git_fixture::run(&repo, &["rev-parse", "HEAD"]), base);
                    assert!(git_fixture::run(&repo, &["status", "--porcelain"]).is_empty());
                    let history = host.history.lock().unwrap();
                    assert_eq!(
                        history.len(),
                        usize::from(writes),
                        "{label}: exactly one fallback write"
                    );
                    if writes {
                        assert_eq!(
                            host.get_task("fixture-task")
                                .unwrap()
                                .implemented_by
                                .as_deref(),
                            Some("fixture-crew")
                        );
                        assert_eq!(history[0].event, "execution_summary_persisted");
                        assert!(
                            history[0]
                                .note
                                .as_deref()
                                .unwrap()
                                .contains("summary-fixture")
                        );
                    }
                    drop(history);
                    // Restore only fixture state before exercising the other asset.
                    let mut task = host.task.lock().unwrap();
                    task.status = orbit_types::task::TaskStatus::InProgress;
                    task.execution_summary = durable.to_string();
                    task.implemented_by = None;
                    host.history.lock().unwrap().clear();
                    *host.state.lock().unwrap() = orbit_types::workflow::PipelineState::new(
                        "summary-fixture".into(),
                        "summary_fixture".into(),
                        json!({}),
                    );
                }
            }
        },
    );
}
