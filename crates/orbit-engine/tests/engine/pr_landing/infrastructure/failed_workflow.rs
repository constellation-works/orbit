//! A malformed candidate workflow fails before the forge creates checks.
use super::*;

#[test]
fn failed_zero_job_workflow_routes_by_candidate_ownership() {
    isolated(
        "infrastructure::failed_workflow::failed_zero_job_workflow_routes_by_candidate_ownership",
        |sandbox| {
            for case in [
                "repair",
                "review",
                "recovery",
                "infrastructure",
                "infrastructure_review",
                "queued",
                "in_progress",
            ] {
                in_fresh_row(sandbox, |row| {
                    let mut fx = fixture_with_workflow(row);
                    let candidate_owned = matches!(case, "repair" | "review" | "recovery");
                    if candidate_owned {
                        fx.candidate = fx.commit(".github/workflows/guard.yml", "on: [broken\n");
                        git(&fx.repo, &["push", "origin", BRANCH]);
                    }
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    open_for_completion(&fx, &host);
                    setup(
                        &fx,
                        if case.starts_with("infrastructure") {
                            "second_failure"
                        } else {
                            "positive"
                        },
                    );
                    update(&fx, "policy", |value| {
                        value["data"]["repository"]["pullRequest"]["baseRef"]["branchProtectionRule"] = json!({"id":"classic-rule","requiredStatusChecks":[{"context":"Protected (rust)","app":{"databaseId":900,"slug":"github-actions"}}]})
                    });
                    put(&fx, "rules", json!([]));
                    for file in ["run-before", "runs-before", "run-after", "runs-after"] {
                        update(&fx, file, |value| {
                            let run = if file.starts_with("runs-") {
                                &mut value["workflow_runs"][0]
                            } else {
                                value
                            };
                            run["pull_requests"] = json!([]);
                        });
                    }
                    fs::write(
                        fx.forge.join("infra-log"),
                        "This run likely failed because of a workflow file issue.\n",
                    )
                    .unwrap();
                    put(&fx, "jobs", json!({"total_count":0,"jobs":[]}));
                    put(
                        &fx,
                        "checks-before",
                        json!({"total_count":0,"check_runs":[]}),
                    );
                    put(
                        &fx,
                        "rollup-policy",
                        json!({"data":{"repository":{"pullRequest":{"headRefOid":fx.candidate,"commits":{"nodes":[{"commit":{"oid":fx.candidate,"statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},"nodes":[]}}}}]}}}}}),
                    );
                    if matches!(case, "queued" | "in_progress") {
                        for file in ["run-before", "runs-before"] {
                            update(&fx, file, |value| {
                                let run = if file == "run-before" {
                                    value
                                } else {
                                    &mut value["workflow_runs"][0]
                                };
                                run["status"] = json!(case);
                                run["conclusion"] = Value::Null;
                            });
                        }
                    }
                    fx.script_checks(&["unreported"]);
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(!matches!(
                        case,
                        "review" | "recovery" | "infrastructure_review"
                    ));
                    input["max_wait_seconds"] = json!(60);
                    let error = if case == "recovery" {
                        host.script("test_checkpoint", json!({"workspace_path":fx.repo,"base_ref":BASE,"base_sha":fx.base_sha}));
                        host.script("test_final_decision", json!({"decision":"escalate","diagnosis":"candidate workflow failed","human_action":"repair workflow"}));
                        let mut job = job_asset(json!([
                            {"id":"setup","target":"activity:checkpoint"},
                            {"id":"complete_pr","target":"activity:pr_complete","default_input":input}
                        ]));
                        job.final_recovery_activity = Some("decide".into());
                        let mut catalog = V2ActivityCatalog::new();
                        for (name, action) in [
                            ("checkpoint", "test_checkpoint"),
                            ("pr_complete", "pr_complete"),
                            ("decide", "test_final_decision"),
                        ] {
                            catalog
                                .insert(name, auto_fix::pr_landing_fixture_activity(name, action));
                        }
                        resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
                        let (writer, _, _) = build_writer_and_sinks(&row.join("audit"), RUN_ID);
                        let result = with_completion_clock(|| {
                            execute_job_with_resume(
                                &job,
                                json!({"task_ids":[TASK_ID]}),
                                RUN_ID,
                                writer,
                                &host,
                                None,
                            )
                        });
                        assert!(!matches!(result, Ok(outcome) if outcome.success));
                        let mut packet = locked(&host.scripted_inputs)
                            .get("test_final_decision")
                            .cloned()
                            .expect("final recovery receives the workflow failure");
                        assert_eq!(
                            packet["workflow_run_failure"]["classification"],
                            "candidate_owned"
                        );
                        assert!(
                            !packet["decisions"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|kind| kind == "rerun_ci")
                        );
                        packet.as_object_mut().unwrap().remove("step_id");
                        let activity =
                            load_activity_asset(
                                &fs::read_to_string(workspace_root().join(
                                    "crates/orbit-core/assets/activities/final_recovery.yaml",
                                ))
                                .unwrap(),
                            )
                            .unwrap();
                        let schema =
                            jsonschema::JSONSchema::compile(&activity.spec.input_schema_json)
                                .unwrap();
                        if let Err(errors) = schema.validate(&packet) {
                            panic!(
                                "recovery packet: {:?}",
                                errors.map(|error| error.to_string()).collect::<Vec<_>>()
                            );
                        }
                        packet["error_message"].as_str().unwrap().to_string()
                    } else {
                        complete_with_logical_clock(&host, &input)
                            .expect_err(case)
                            .to_string()
                    };
                    let attempts = pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER);
                    assert_eq!(
                        attempts.len(),
                        usize::from(case == "repair"),
                        "{case}: {error}"
                    );
                    assert_eq!(
                        fx.forge_calls()
                            .iter()
                            .filter(|call| call.starts_with("run rerun"))
                            .count(),
                        usize::from(case.starts_with("infrastructure")),
                        "{case}: {error}"
                    );
                    if candidate_owned {
                        assert!(
                            error.contains(".github/workflows/guard.yml"),
                            "{case}: {error}"
                        );
                        assert!(error.contains("700"), "{case}: {error}");
                        assert!(
                            error.contains(
                                "This run likely failed because of a workflow file issue."
                            ),
                            "{case}: {error}"
                        );
                        assert_eq!(fx.status_reads(), 1, "stop on the first failed-run poll");
                        if case == "repair" {
                            assert!(
                                orbit_types::workflow::is_pr_auto_fix_check_repair(&error),
                                "{error}"
                            );
                        }
                    } else if !case.starts_with("infrastructure") {
                        assert!(error.contains("timed out"), "{case}: {error}");
                        assert!(fx.status_reads() > 1, "{case} stays pending");
                    } else {
                        assert!(
                            error.contains("infrastructure rerun budget is spent"),
                            "{case}: {error}"
                        );
                    }
                    assert!(fx.merge_requests().is_empty(), "{case}");
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    if case == "repair" {
                        let again = complete_with_logical_clock(&host, &input)
                            .expect_err("the same head cannot spend another repair");
                        assert!(
                            matches!(
                                again,
                                OrbitError::DeliveryRefused {
                                    refusal:
                                        orbit_types::workflow::DeliveryRefusal::RepairDispatched,
                                    ..
                                }
                            ),
                            "{again}"
                        );
                        assert_eq!(
                            pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).len(),
                            1
                        );
                    }
                });
            }
        },
    );
}
