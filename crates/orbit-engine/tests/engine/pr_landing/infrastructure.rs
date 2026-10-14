//! Missing-required prerequisite recovery through the real-Git PR executor.
use super::*;
use orbit_types::workflow::{PR_INFRASTRUCTURE_RESERVED_EVENT, pr_infrastructure_reservations};

const SOURCE: &str = "name: Guarded CI\non: [pull_request]\npermissions: {}\njobs:\n  state:\n    name: Read state\n    runs-on: ubuntu-latest\n    steps:\n      - name: Read PR\n        run: gh api repos/example/pulls/42\n  protected:\n    needs: state\n    name: Protected (${{ matrix.language }})\n    strategy:\n      matrix:\n        language: [rust]\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo protected\n";

fn fixture_with_workflow(row: &Path) -> Fixture {
    fixture_with_source(row, SOURCE)
}

fn fixture_with_source(row: &Path, source: &str) -> Fixture {
    let initial = Fixture::new(row);
    initial.commit_on_base(".github/workflows/guard.yml", source);
    git(&initial.repo, &["rebase", BASE]);
    git(
        &initial.repo,
        &["push", "--force-with-lease", "origin", BRANCH],
    );
    Fixture {
        candidate: initial.head(),
        base_sha: git(&initial.repo, &["rev-parse", BASE]),
        ..initial
    }
}

fn put(fx: &Fixture, name: &str, value: Value) {
    fs::write(
        fx.forge.join(format!("infra-{name}")),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
}
fn setup(fx: &Fixture, mode: &str) {
    let head = &fx.candidate;
    let repo = json!({"id":500,"full_name":"orbit/test"});
    put(fx, "repo", repo.clone());
    put(
        fx,
        "pr",
        json!({"number":42,"state":"open","merged":false,"head":{"sha":head,"ref":BRANCH,"repo":repo},"base":{"sha":fx.base_sha,"ref":BASE,"repo":repo}}),
    );
    put(
        fx,
        "policy",
        json!({"data":{"repository":{"pullRequest":{"headRefOid":head,"baseRefName":BASE,"reviewDecision":null,"baseRef":{"branchProtectionRule":{"requiredStatusChecks":[]}}}}}}),
    );
    put(
        fx,
        "rules",
        json!([{"type":"required_status_checks","ruleset_id":55,"ruleset_source":"orbit/test","ruleset_source_type":"Repository","parameters":{"required_status_checks":[{"context":"Protected (rust)","integration_id":900}]}}]),
    );
    put(
        fx,
        "rollup-policy",
        json!({"data":{"repository":{"pullRequest":{"headRefOid":head,"commits":{"nodes":[{"commit":{"oid":head,"statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},"nodes":[{"name":"Read state","isRequired":false}]}}}}]}}}}}),
    );
    let run = json!({"id":700,"run_attempt":1,"workflow_id":600,"check_suite_id":800,"path":".github/workflows/guard.yml","head_sha":head,"head_branch":BRANCH,"repository":repo,"head_repository":repo,"event":"pull_request","actor":{"id":123,"login":"owner"},"status":"completed","conclusion":"failure","pull_requests":[{"number":42,"head":{"sha":head},"base":{"sha":fx.base_sha,"ref":BASE,"repo":repo}}]});
    put(fx, "run-before", run.clone());
    put(
        fx,
        "runs-before",
        json!({"total_count":1,"workflow_runs":[run]}),
    );
    let mut after = run.clone();
    after["run_attempt"] = json!(2);
    after["conclusion"] = json!("success");
    if mode == "second_failure" {
        after["conclusion"] = json!("failure");
    }
    if mode == "running" {
        after["status"] = json!("in_progress");
        after["conclusion"] = Value::Null;
    }
    put(fx, "run-after", after.clone());
    put(
        fx,
        "runs-after",
        json!({"total_count":1,"workflow_runs":[after]}),
    );
    let app = json!({"id":900,"slug":"github-actions"});
    let prerequisite = json!({"id":1100,"name":"Read state","head_sha":head,"app":app,"check_suite":{"id":800},"status":"completed","conclusion":"failure"});
    put(
        fx,
        "checks-before",
        json!({"total_count":1,"check_runs":[prerequisite]}),
    );
    put(
        fx,
        "checks-after",
        json!({"total_count":1,"check_runs":[{"id":1200,"name":"Protected (rust)","head_sha":head,"app":app,"check_suite":{"id":800},"status":"completed","conclusion":"success"}]}),
    );
    put(
        fx,
        "suite",
        json!({"id":800,"head_sha":head,"app":app,"repository":repo}),
    );
    put(
        fx,
        "jobs",
        json!({"total_count":1,"jobs":[{"id":1000,"run_id":700,"run_attempt":1,"head_sha":head,"name":"Read state","status":"completed","conclusion":"failure","check_run_url":"https://api.github.com/repos/orbit/test/check-runs/1100","steps":[{"name":"Read PR","number":1,"status":"completed","conclusion":"failure"}]}]}),
    );
    // Provider coverage is complete, including zero-step skipped matrices.
    let source: Value = serde_yaml::from_str(&git(
        &fx.repo,
        &["show", &format!("{head}:.github/workflows/guard.yml")],
    ))
    .unwrap_or_else(|_| serde_yaml::from_str(SOURCE).unwrap());
    let jobs = source["jobs"].as_object().unwrap();
    let root = if jobs.contains_key("pr-state") {
        "pr-state"
    } else {
        "state"
    };
    update(fx, "jobs", |value| {
        let failed = &mut value["jobs"][0];
        failed["name"] = source["jobs"][root]["name"].clone();
        if let Some(steps) = source["jobs"][root]["steps"].as_array() {
            failed["steps"] = json!(steps.iter().enumerate().map(|(i, step)| json!({"name":step["name"], "number":i+1,"status":"completed","conclusion":if step["name"] == "Read PR" || step["name"] == "Read live PR state" {"failure"} else {"success"}})).collect::<Vec<_>>());
        }
        let mut rows = vec![failed.clone()];
        for (index, (id, job)) in jobs
            .iter()
            .filter(|(id, _)| id.as_str() != root)
            .enumerate()
        {
            rows.push(json!({"id":1001+index,"run_id":700,"run_attempt":1,"head_sha":head,"name":job.get("name").unwrap_or(&json!(id)),"status":"completed","conclusion":"skipped","steps":[]}));
        }
        *value = json!({"total_count":rows.len(),"jobs":rows});
    });
    update(fx, "checks-before", |value| {
        value["check_runs"][0]["name"] = source["jobs"][root]["name"].clone()
    });
    put(
        fx,
        "workflow",
        json!({"id":600,"path":".github/workflows/guard.yml"}),
    );
    put(
        fx,
        "contents",
        json!({"type":"file","path":".github/workflows/guard.yml","size":git(&fx.repo,&["cat-file","-s",&format!("{head}:.github/workflows/guard.yml")]).parse::<usize>().unwrap(),"sha":git(&fx.repo,&["rev-parse",&format!("{head}:.github/workflows/guard.yml")])}),
    );
    fs::write(
        fx.forge.join("infra-log"),
        "Read state\tRead PR\tgh: Service Unavailable (HTTP 503)\n",
    )
    .unwrap();
    fs::write(fx.forge.join("infrastructure"), mode).unwrap();
    fx.script_checks(&["prerequisite"]);
}
fn update(fx: &Fixture, file: &str, change: impl FnOnce(&mut Value)) {
    let path = fx.forge.join(format!("infra-{file}"));
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    change(&mut value);
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[test]
fn missing_required_prerequisite_requires_a_literal_api_read() {
    isolated(
        "infrastructure::missing_required_prerequisite_requires_a_literal_api_read",
        |sandbox| {
            for (command, shell, allowed) in [
                "gh api --method POST repos/example/issues --raw-field title=example",
                "gh api -X PATCH repos/example/issues/42",
                "gh api --method=DELETE repos/example/issues/42",
                "gh api -XPOST repos/example/issues",
                "gh api --method GET --method POST repos/example/issues",
                "gh api repos/example/issues --field title=example",
                "gh api repos/example/issues -Ftitle=example",
                "gh api repos/example/issues --raw-field=title=example",
                "gh api repos/example/issues -f title=example",
                "gh api repos/example/issues -ftitle=example",
                "gh api repos/example/issues --field=title=example",
                "gh api repos/example/issues --input payload.json",
                "gh api repos/example/issues --input=payload.json",
                "gh api graphql",
                "gh api repos/{1..1000000}/pulls/42",
                "gh api --method GET 'repos/example/pulls/42' --jq 'env.GH_TOKEN' >> \"$GITHUB_OUTPUT\"",
                "# gh api repos/example/pulls/42",
                "echo gh api repos/example/pulls/42",
                "gh api repos/example/pulls/42 # a comment",
                "gh api repos/example/pulls/42; echo done",
                "gh api repos/example/pulls/42 && echo done",
                "gh api repos/example/pulls/42 | cat",
                "gh api repos/example/pulls/42\ngh api -X POST repos/example/issues",
                "gh api repos/example/$(echo pulls)/42",
                "gh api repos/example/`echo pulls`/42",
                "gh api repos/example/pulls/42 > response.json",
                "gh api\nrepos/example/pulls/42",
                "gh api --method POST 'repos/example/issues' --jq '.state' >> \"$GITHUB_OUTPUT\"",
                "gh api --method GET 'repos/example/pulls/42' --field title=x --jq '.state' >> \"$GITHUB_OUTPUT\"",
                "gh api --method GET 'repos/${{ github.event.pull_request.title }}' --jq '.state' >> \"$GITHUB_OUTPUT\"",
                "gh api --method GET 'repos/example/pulls/42' --jq '.state' >> \"$GITHUB_ENV\"",
                "gh api --method GET 'repos/example/pulls/42' --jq '.state'; gh api -X POST repos/example/issues --jq '.state' >> \"$GITHUB_OUTPUT\"",
            ]
            .into_iter()
            .map(|command| (command, None, false))
            .chain([
                ("gh api repos/example/pulls/42", None, true),
                (
                    "gh api --method GET repos/example/pulls/42",
                    Some("bash"),
                    true,
                ),
                (
                    "gh api repos/{owner}/{repo}/pulls/42 -X GET",
                    Some("sh"),
                    true,
                ),
                ("gh api --method=GET repos/example/pulls/42", None, true),
                ("gh api -XGET repos/example/pulls/42", None, true),
                ("gh api repos/example/pulls/42", Some("python"), false),
                ("gh api repos/example/pulls/42", Some("job:python"), false),
                (
                    "gh api repos/example/pulls/42",
                    Some("workflow:python"),
                    false,
                ),
            ]) {
                in_fresh_row(sandbox, |row| {
                    tracing::info!(command, ?shell, "API read source counterfactual");
                    let mut source: Value = serde_yaml::from_str(SOURCE).unwrap();
                    source["jobs"]["state"]["steps"][0]["run"] = json!(command);
                    if let Some(shell) = shell {
                        if let Some(shell) = shell.strip_prefix("job:") {
                            source["jobs"]["state"]["defaults"] = json!({"run":{"shell":shell}});
                        } else if let Some(shell) = shell.strip_prefix("workflow:") {
                            source["defaults"] = json!({"run":{"shell":shell}});
                        } else {
                            source["jobs"]["state"]["steps"][0]["shell"] = json!(shell);
                        }
                    }
                    let source = serde_yaml::to_string(&source).unwrap();
                    let fx = fixture_with_source(row, &source);
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    action(
                        &host,
                        "pr_open",
                        &fx.open_input(&fx.candidate, &fx.base_sha),
                    )
                    .unwrap();
                    setup(&fx, "positive");
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    let result = action(&host, "pr_complete", &input);
                    assert_eq!(
                        result.is_ok(),
                        allowed,
                        "source admission: {command}: {result:?}"
                    );
                    if let Err(error) = result {
                        assert!(
                            error.to_string().contains("source-bound API read"),
                            "{error}"
                        );
                    }
                    assert_eq!(
                        pr_infrastructure_reservations(&host.history(TASK_ID))
                            .unwrap()
                            .len(),
                        usize::from(allowed),
                        "only supported reads spend infrastructure budget: {command}"
                    );
                    assert_eq!(
                        fs::read_to_string(fx.forge.join("calls"))
                            .unwrap()
                            .lines()
                            .filter(|line| *line == "run rerun")
                            .count(),
                        usize::from(allowed),
                        "only supported reads authorize one rerun: {command}"
                    );
                    assert!(fx.merge_requests().is_empty());
                    assert!(pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty());
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                });
            }
        },
    );
}

#[test]
fn missing_required_prerequisite_recovers_once_without_source_repair() {
    isolated(
        "infrastructure::missing_required_prerequisite_recovers_once_without_source_repair",
        |sandbox| {
            for mode in [
                "positive",
                "merge",
                "lost",
                "no_apply",
                "auth",
                "second_failure",
                "running",
                "clean_missing",
                "foreign_after",
                "advanced_after",
                "changed_policy",
                "moved",
                "deleted",
            ] {
                in_fresh_row(sandbox, |row| {
                    tracing::info!(mode, "prerequisite recovery");
                    let fx = fixture_with_workflow(row);
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    action(
                        &host,
                        "pr_open",
                        &fx.open_input(&fx.candidate, &fx.base_sha),
                    )
                    .unwrap();
                    setup(&fx, mode);
                    if mode == "clean_missing" {
                        update(&fx, "checks-after", |value| {
                            value["total_count"] = json!(0);
                            value["check_runs"] = json!([]);
                        });
                    }
                    if mode == "foreign_after" {
                        update(&fx, "checks-after", |value| {
                            value["check_runs"][0]["app"]["id"] = json!(901);
                        });
                    }
                    if mode == "advanced_after" {
                        update(&fx, "run-after", |value| {
                            value["run_attempt"] = json!(3);
                        });
                        update(&fx, "runs-after", |value| {
                            value["workflow_runs"][0]["run_attempt"] = json!(3);
                        });
                    }
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!(if mode == "merge" { "done" } else { "review" });
                    if mode == "merge" {
                        input["reviewed_head_sha"] = json!(fx.candidate);
                    }
                    input["max_wait_seconds"] = json!(15);
                    let policy_path = fx.forge.join("infra-rules");
                    let outcome = orbit_engine::test_support::with_vcs_retry_wait(
                        move |_| {
                            if mode == "changed_policy" {
                                std::fs::write(&policy_path, "[]").unwrap();
                            }
                        },
                        || action(&host, "pr_complete", &input),
                    );
                    if matches!(mode, "positive" | "merge" | "lost") {
                        let output = outcome.expect(mode);
                        assert_eq!(
                            if mode == "merge" {
                                &output["merge"]["delivery_evidence"]["head_sha"]
                            } else {
                                &output["merge"]["head_sha"]
                            },
                            &json!(fx.candidate),
                            "{mode}: {output}"
                        );
                        assert_eq!(
                            host.status(TASK_ID),
                            if mode == "merge" {
                                TaskStatus::Done
                            } else {
                                TaskStatus::Review
                            }
                        );
                        assert_eq!(fx.merge_requests().len(), usize::from(mode == "merge"));
                    } else {
                        let error = outcome.expect_err(mode).to_string();
                        if mode == "auth" {
                            assert!(error.contains("HTTP 403"), "{error}");
                        }
                        assert!(fx.merge_requests().is_empty());
                        assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    }
                    assert_eq!(
                        fx.head(),
                        fx.candidate,
                        "infrastructure never changes source"
                    );
                    if mode == "moved" {
                        assert_ne!(fx.remote_tip(BRANCH), fx.candidate);
                    } else if mode == "deleted" {
                        assert!(
                            git(
                                &fx.repo,
                                &["ls-remote", "origin", &format!("refs/heads/{BRANCH}")]
                            )
                            .is_empty()
                        );
                    } else {
                        assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
                    }
                    assert_eq!(git(&fx.repo, &["status", "--short"]), "");
                    assert_eq!(
                        fs::read_to_string(fx.forge.join("calls"))
                            .unwrap()
                            .lines()
                            .filter(|line| *line == "run rerun")
                            .count(),
                        1,
                        "one durable bounded mutation: {mode}"
                    );
                    let reservations =
                        pr_infrastructure_reservations(&host.history(TASK_ID)).unwrap();
                    assert_eq!(reservations.len(), 1);
                    assert_eq!(reservations[0].evidence.run_attempt, 1);
                    assert_eq!(reservations[0].evidence.workflow_source, SOURCE);
                    assert!(reservations[0].evidence.log.contains("HTTP 503"));
                    assert!(
                        pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty(),
                        "no agent repair admitted"
                    );
                    assert!(
                        host.scripted_inputs.lock().unwrap().is_empty(),
                        "no repair agent invoked"
                    );
                    assert!(host.has_artifact(
                        TASK_ID,
                        &format!("auto-fix/{RUN_ID}/infrastructure-700-original.json")
                    ));
                    if mode == "no_apply" {
                        let before = host.history(TASK_ID);
                        // Same execution, then a sibling execution, both read the
                        // durable original reservation and only reconcile it.
                        for execution in [RUN_ID, "sibling-run"] {
                            input["run_id"] = json!(execution);
                            assert!(
                                orbit_engine::test_support::with_vcs_retry_wait(
                                    |_| {},
                                    || action(&host, "pr_complete", &input)
                                )
                                .is_err()
                            );
                        }
                        assert_eq!(host.history(TASK_ID), before);
                        assert_eq!(
                            fs::read_to_string(fx.forge.join("calls"))
                                .unwrap()
                                .lines()
                                .filter(|line| *line == "run rerun")
                                .count(),
                            1
                        );
                    }
                });
            }
        },
    );
}

#[test]
fn missing_required_prerequisite_refuses_incomplete_or_foreign_evidence() {
    isolated(
        "infrastructure::missing_required_prerequisite_refuses_incomplete_or_foreign_evidence",
        |sandbox| {
            for case in [
                "optional",
                "review",
                "policy",
                "rules",
                "app",
                "head",
                "base",
                "repo",
                "event",
                "run",
                "attempt",
                "running",
                "jobs",
                "source",
                "dag",
                "log_auth",
                "log_unreadable",
                "log_truncated",
                "remote_moved",
                "remote_deleted",
                "superseded",
                "spent",
                "admin",
                "deadline",
                "step",
                "checks",
                "workflow",
                "suite",
                "source_base",
                "api403",
                "rules_truncated",
                "policy_absent",
                "dag_malformed",
                "wrong_job",
                "review_unreadable",
            ] {
                in_fresh_row(sandbox, |row| {
                    tracing::info!(case, "prerequisite counterfactual");
                    let fx = if case == "dag_malformed" {
                        fixture_with_source(row, "jobs: [invalid\n")
                    } else {
                        fixture_with_workflow(row)
                    };
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    action(
                        &host,
                        "pr_open",
                        &fx.open_input(&fx.candidate, &fx.base_sha),
                    )
                    .unwrap();
                    setup(&fx, "positive");
                    match case {
                        "api403" => fs::write(fx.forge.join("infra-auth403"), "denied").unwrap(),
                        "rules_truncated" => update(&fx, "rules", |value| {
                            *value = json!(vec![value[0].clone(); 100]);
                        }),
                        "policy_absent" => update(&fx, "policy", |value| {
                            value["data"]["repository"]["pullRequest"]["baseRef"]
                                .as_object_mut()
                                .unwrap()
                                .remove("branchProtectionRule");
                        }),
                        "dag_malformed" => update(&fx, "contents", |value| {
                            value["size"] = json!("jobs: [invalid\n".len());
                        }),
                        "wrong_job" => update(&fx, "jobs", |value| {
                            value["jobs"][0]["run_id"] = json!(701);
                        }),
                        "review_unreadable" => update(&fx, "policy", |value| {
                            value["data"]["repository"]["pullRequest"]
                                .as_object_mut()
                                .unwrap()
                                .remove("reviewDecision");
                        }),
                        "source_base" => update(&fx, "run-before", |value| {
                            value["pull_requests"][0]["base"]["sha"] = json!("foreign");
                        }),
                        "admin" => update(&fx, "rules", |value| {
                            value
                                .as_array_mut()
                                .unwrap()
                                .push(json!({"type":"merge_queue"}));
                        }),
                        "step" => update(&fx, "jobs", |value| {
                            value["jobs"][0]["steps"][0]["name"] = json!("Foreign step");
                        }),
                        "checks" => update(&fx, "checks-before", |value| {
                            value["total_count"] = json!(3);
                        }),
                        "workflow" => update(&fx, "workflow", |value| {
                            value["id"] = json!(601);
                        }),
                        "suite" => update(&fx, "suite", |value| {
                            value["repository"]["id"] = json!(501);
                        }),
                        "deadline" => {}
                        "optional" => update(&fx, "checks-before", |value| {
                            value["total_count"] = json!(2);
                            value["check_runs"].as_array_mut().unwrap().push(json!({"id":1200,"head_sha":fx.candidate,"name":"Protected (rust)","app":{"id":900},"status":"completed","conclusion":"success"}));
                        }),
                        "review" => fx.script_checks(&["review_required"]),
                        "policy" => update(&fx, "policy", |value| {
                            value["errors"] = json!([{"message":"forbidden"}]);
                        }),
                        "rules" => update(&fx, "rules", |value| {
                            value[0]["parameters"]["required_status_checks"][0]
                                .as_object_mut()
                                .unwrap()
                                .remove("integration_id");
                        }),
                        "app" => update(&fx, "suite", |value| {
                            value["app"]["id"] = json!(901);
                        }),
                        "head" => update(&fx, "run-before", |value| {
                            value["head_sha"] = json!(fx.base_sha);
                        }),
                        "base" => update(&fx, "pr", |value| {
                            value["base"]["ref"] = json!("main");
                        }),
                        "repo" => update(&fx, "pr", |value| {
                            value["head"]["repo"]["id"] = json!(501);
                        }),
                        "event" => update(&fx, "run-before", |value| {
                            value["event"] = json!("push");
                        }),
                        "run" => update(&fx, "run-before", |value| {
                            value["id"] = json!(701);
                        }),
                        "attempt" => update(&fx, "run-before", |value| {
                            value["run_attempt"] = json!(2);
                        }),
                        "running" => update(&fx, "run-before", |value| {
                            value["status"] = json!("in_progress");
                        }),
                        "jobs" => update(&fx, "jobs", |value| {
                            value["total_count"] = json!(99);
                        }),
                        "source" => update(&fx, "contents", |value| {
                            value["size"] = json!(1);
                        }),
                        "dag" => update(&fx, "rules", |value| {
                            value[0]["parameters"]["required_status_checks"][0]["context"] =
                                json!("Foreign check");
                        }),
                        "log_auth" => fs::write(
                            fx.forge.join("infra-log"),
                            "gh: Service Unavailable (HTTP 503)\ngh: Bad credentials (HTTP 403)\n",
                        )
                        .unwrap(),
                        "log_unreadable" => fs::write(fx.forge.join("infra-log"), "").unwrap(),
                        "log_truncated" => fs::write(
                            fx.forge.join("infra-log"),
                            format!(
                                "{}\ngh: Service Unavailable (HTTP 503)\n",
                                "x".repeat(70000)
                            ),
                        )
                        .unwrap(),
                        "remote_moved" => {
                            let other = fx.commit("foreign.txt", "foreign\n");
                            git(
                                &fx.repo,
                                &["push", "origin", &format!("{other}:refs/heads/{BRANCH}")],
                            );
                        }
                        "remote_deleted" => {
                            git(
                                &fx.repo,
                                &["push", "origin", &format!(":refs/heads/{BRANCH}")],
                            );
                        }
                        "superseded" => update(&fx, "runs-before", |value| {
                            value["total_count"] = json!(2);
                            let mut second = value["workflow_runs"][0].clone();
                            second["id"] = json!(701);
                            value["workflow_runs"].as_array_mut().unwrap().push(second);
                        }),
                        "spent" => {
                            let response = orbit_engine::run_private_vcs_operation("ci.prerequisite_evidence",&json!({"workspace_path":fx.repo,"pr":PR_NUMBER,"head_sha":fx.candidate,"head_branch":BRANCH,"base_branch":BASE,"deadline_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 60000})).unwrap();
                            let evidence: orbit_types::workflow::PrInfrastructureEvidence =
                                serde_json::from_value(response["evidence"].clone()).unwrap();
                            let mut reservation =
                                orbit_types::workflow::PrInfrastructureReservation {
                                    execution: "sibling-run".into(),
                                    evidence,
                                };
                            reservation.evidence.head_sha = fx.base_sha.clone();
                            auto_fix::seed_auto_fix_history(
                                &host,
                                PR_INFRASTRUCTURE_RESERVED_EVENT,
                                &reservation,
                            );
                        }
                        _ => unreachable!(),
                    }
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    if case == "deadline" {
                        input["max_wait_seconds"] = json!(0);
                    }
                    let result = orbit_engine::test_support::with_vcs_retry_wait(
                        |_| {},
                        || action(&host, "pr_complete", &input),
                    );
                    let error = result.expect_err(case).to_string();
                    let expected = match case {
                        "optional" => "only optional",
                        "review" => "review is required",
                        "policy" => "classic policy errors",
                        "rules" => "required integration",
                        "app" => "protected integration differs",
                        "head" => "run head",
                        "base" => "changed base",
                        "repo" => "changed repository",
                        "event" => "workflow event",
                        "run" => "changed run",
                        "attempt" => "latest attempt",
                        "running" => "running, superseded",
                        "jobs" => "truncated jobs",
                        "source" => "truncated workflow source",
                        "dag" => "no unique source-bound",
                        "log_auth" => "no narrow transient",
                        "log_unreadable" => "log",
                        "log_truncated" => "truncated",
                        "remote_moved" | "remote_deleted" => "delivery_evidence_stale",
                        "superseded" => "superseded workflow run",
                        "spent" => "PR identity changed",
                        "admin" => "administrative rule",
                        "deadline" => "deadline",
                        "step" => "source command",
                        "checks" => "truncated check_runs",
                        "workflow" => "workflow identity",
                        "suite" => "suite repository",
                        "source_base" => "workflow event base commit",
                        "api403" => "HTTP 403",
                        "rules_truncated" => "truncated effective rules",
                        "policy_absent" => "classic branch rule",
                        "dag_malformed" => "no unique source-bound",
                        "wrong_job" => "job run",
                        "review_unreadable" => "unreadable review decision",
                        _ => unreachable!(),
                    };
                    assert!(
                        error.contains(expected),
                        "{case}: expected {expected}: {error}"
                    );
                    assert_eq!(
                        pr_infrastructure_reservations(&host.history(TASK_ID))
                            .unwrap()
                            .len(),
                        usize::from(case == "spent"),
                        "incomplete evidence spends no infrastructure budget: {case}"
                    );
                    assert!(fx.merge_requests().is_empty());
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    assert!(pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty());
                    assert_eq!(
                        fs::read_to_string(fx.forge.join("calls"))
                            .unwrap()
                            .lines()
                            .filter(|line| *line == "run rerun")
                            .count(),
                        0,
                        "{case} must authorize no rerun"
                    );
                });
            }
        },
    );
}

mod bounds;
