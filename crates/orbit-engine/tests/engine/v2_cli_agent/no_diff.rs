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
