//! Owner clean-base delivery must use verified artifacts, never claim-only selectors.

use super::*;

/// ORB-23194: drive the real implement boundary and commit together. Selectors
/// cannot bypass owner validation, and artifact-backed no-op delivery still ends
/// without a commit or a PR (the original owner empty-stage failure).
#[test]
fn owner_clean_base_requires_artifacts_through_implementation_and_commit() {
    git_fixture::isolated(
        module_path!(),
        "owner_clean_base_requires_artifacts_through_implementation_and_commit",
        || {
            for (declares_selectors, log_exit_code) in [(true, 0), (false, 0), (false, 1)] {
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
                let mut result = json!({
                    "execution_summary":"Outcome: success\n\nValidated the unchanged base; no change is required.",
                });
                if declares_selectors {
                    result["already_resolved"] = json!({
                        "selectors":["cargo test -p orbit-engine --test engine commit_verifier::"],
                    });
                }
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
                    "status":"in-progress", "priority":"medium", "task_type":"bug",
                    "tags":[], "context_files":[], "job_run_id":"owner-no-diff",
                    "created_at":chrono::Utc::now(), "updated_at":chrono::Utc::now(),
                }))
                .unwrap();
                let artifact =
                    |path, content: Value| TaskArtifact::from_text(path, content.to_string());
                let artifacts = if declares_selectors {
                    Vec::new()
                } else {
                    vec![
                        artifact(
                            "no-diff.json",
                            json!({
                                "schema_version":1, "task_id":"fixture-task", "run_id":"owner-no-diff",
                                "tested_head":base, "reason":"The base already resolves the task.",
                                "validation":[{"command":"make test", "exit_code":0, "log_artifact":"validation.json"}],
                            }),
                        ),
                        artifact(
                            "validation.json",
                            json!({
                                "run_id":"owner-no-diff", "tested_head":base, "command":"make test",
                                "exit_code":log_exit_code, "output":"captured validation output",
                            }),
                        ),
                    ]
                };
                let host = SummaryDeliveryHost {
                    cli: ScriptHost::new(fake.cli_path()),
                    artifacts,
                    task: std::sync::Mutex::new(task),
                    history: std::sync::Mutex::default(),
                    state: std::sync::Mutex::new(orbit_types::workflow::PipelineState::new(
                        "owner-no-diff".into(),
                        "owner_no_diff".into(),
                        json!({}),
                    )),
                };
                let mut spec = cli_agent_loop_spec(Some(Provider::Grok));
                spec.model = None;
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
                    commit["job_run_id"] = json!("owner-no-diff");
                    commit["workspace_path"] = json!(repo);
                    commit["base_sha"] = json!(base);
                    commit.as_object_mut().unwrap().remove("base_ref");
                    let job = load_job_asset(&json!({
                        "schemaVersion":2,"kind":"Job","metadata":{"name":"owner_no_diff"},
                        "spec":{"state":"enabled","kind":"workflow","steps":[
                            {"id":"implement_bundle","loop":{"items":"{{ input.task_ids }}","max_iterations":1,"steps":[
                                {"id":"implement_one","spec":ActivityV2Spec::AgentLoop(spec.clone())},
                            ]}},
                            {"id":"commit","spec":{"type":"deterministic","action":"git_commit","config":{}},"default_input":commit},
                            {"id":"promote","spec":{"type":"deterministic","action":"pr_promote","config":{}},"default_input":{
                                "job_run_id":"owner-no-diff","completed_task_ids":["fixture-task"],
                                "workspace_path":repo,"no_diff_expected":"{{ steps.commit.output.skipped_no_diff_expected }}",
                                "already_landed_checkpoint":"{{ steps.commit.output }}",
                            }},
                            {"id":"complete","spec":{"type":"deterministic","action":"pr_complete","config":{}},"default_input":{
                                "job_run_id":"owner-no-diff","completed_task_ids":["fixture-task"],
                                "workspace_path":repo,"no_diff_expected":"{{ steps.commit.output.skipped_no_diff_expected }}",
                                "already_landed_checkpoint":"{{ steps.commit.output }}",
                            }},
                        ]},
                    }).to_string()).unwrap().spec;
                    let (writer, _) =
                        build_writer(&fixture.path().join("audit"), "owner-no-diff").unwrap();
                    let outcome = execute_job_with_resume(
                        &job,
                        json!({
                            "task_ids":["fixture-task"], "task_id":"fixture-task", "workspace_path":repo,
                            "base_sha":base,
                        }),
                        "owner-no-diff",
                        writer,
                        &host,
                        None,
                    );
                    if declares_selectors {
                        let outcome = outcome.unwrap();
                        assert!(!outcome.success, "{outcome:?}");
                        let refusal = outcome.pipeline["implement_one"]["response_envelope_error"]
                            .as_str()
                            .unwrap();
                        assert!(refusal.contains("claimed"), "{refusal}");
                        let refusal = refusal.strip_prefix("response_envelope_error: ").unwrap();
                        assert!(outcome.pipeline.get("commit").is_none(), "{outcome:?}");
                        // Even a caller bypassing the implement step must receive
                        // the same refusal from commit, including checkpointed output.
                        for inline in [true, false] {
                            let mut input = commit.clone();
                            if inline {
                                input["implementation"] = result.clone();
                            } else {
                                host.state
                                    .lock()
                                    .unwrap()
                                    .record_pipeline_output("implement_one", result.clone());
                            }
                            let error = orbit_engine::execute_deterministic_action(
                                &host,
                                "git_commit",
                                &json!({}),
                                &input,
                                false,
                                &std::collections::HashMap::new(),
                                None,
                            )
                            .expect_err("owner selectors must not settle without replay")
                            .to_string();
                            assert!(
                                error.contains(refusal),
                                "commit and implement must agree: {error}; {refusal}"
                            );
                        }
                    } else if log_exit_code == 0 {
                        let outcome = outcome.unwrap();
                        assert!(outcome.success, "{outcome:?}");
                        assert_eq!(outcome.pipeline["commit"]["decision"], "verified_no_diff");
                        assert_eq!(outcome.pipeline["commit"]["committed"], false);
                        assert_eq!(
                            host.get_task("fixture-task").unwrap().status,
                            orbit_types::task::TaskStatus::Done
                        );
                    } else {
                        let error = outcome.unwrap_err().to_string();
                        assert!(error.contains("validation log must capture"), "{error}");
                    }
                    if declares_selectors || log_exit_code != 0 {
                        assert_eq!(
                            host.get_task("fixture-task").unwrap().status,
                            orbit_types::task::TaskStatus::InProgress
                        );
                    }
                    assert_eq!(git_fixture::run(&repo, &["rev-parse", "HEAD"]), base);
                    assert!(git_fixture::run(&repo, &["status", "--porcelain"]).is_empty());
                    host.task.lock().unwrap().status = orbit_types::task::TaskStatus::InProgress;
                    *host.state.lock().unwrap() = orbit_types::workflow::PipelineState::new(
                        "owner-no-diff".into(),
                        "owner_no_diff".into(),
                        json!({}),
                    );
                }
            }
        },
    );
}
