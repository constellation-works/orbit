//! Public completion regressions for rerun scope, DAG work and absolute budgets.
use super::*;

fn complete_infrastructure(
    host: &DeliveryHost,
    fx: &Fixture,
) -> Result<Value, orbit_common::OrbitError> {
    let mut input = fx.complete_ungated_input();
    input["auto_fix"] = json!(true);
    input["completion"] = json!("review");
    // The action wrapper's public error is returned as OrbitError for row assertions.
    action(host, "pr_complete", &input)
        .map_err(|error| orbit_common::OrbitError::Execution(error.to_string()))
}
fn rerun_count(fx: &Fixture) -> usize {
    fs::read_to_string(fx.forge.join("calls"))
        .unwrap()
        .lines()
        .filter(|line| *line == "run rerun")
        .count()
}

#[test]
fn prerequisite_bounds_entire_repeated_failed_job() {
    isolated(
        "infrastructure::bounds::prerequisite_bounds_entire_repeated_failed_job",
        |sandbox| {
            for case in [
                "prior_write",
                "later_write",
                "prior_action",
                "prior_echo",
                "prior_composed",
                "other_failed",
                "cancelled",
                "container",
                "shell_env",
                "host_env",
                "foreign_repo_env",
                "safe_prior_read",
                "legacy_codeql",
                "legacy_wildcard_app",
                "legacy_write",
                "legacy_binding",
                "legacy_output",
                "successful_dependent_read",
                "successful_dependent_write",
                "transitive_dependent_write",
                "fresh_release",
                "fresh_write_authority",
                "fresh_unknown_action",
                "fresh_foreign_token",
                "fresh_other_secret",
                "provider_unknown_step",
                "provider_unknown_outcome",
                "provider_incomplete",
                "actor",
                "actual_ci",
                "actual_codeql",
                "actual_macos",
                "actual_windows",
                "actual_website",
            ] {
                in_fresh_row(sandbox, |row| {
                    let mut source: Value = serde_yaml::from_str(SOURCE).unwrap();
                    match case {
                        "prior_write" | "later_write" | "prior_action" | "prior_echo"
                        | "prior_composed" | "safe_prior_read" => {
                            let step = match case {
                                "prior_action" => {
                                    json!({"name":"Setup","uses":"actions/checkout@deadbeef"})
                                }
                                "prior_echo" => {
                                    json!({"name":"Setup","run":"echo gh api repos/example/pulls/42"})
                                }
                                "prior_composed" => {
                                    json!({"name":"Setup","run":"gh api repos/example/pulls/42 && gh api -X POST repos/example/issues"})
                                }
                                "safe_prior_read" => {
                                    json!({"name":"Setup","run":"gh api --method GET repos/example/pulls/42"})
                                }
                                _ => {
                                    json!({"name":"Setup","run":"gh api --method POST repos/example/issues"})
                                }
                            };
                            let steps = source["jobs"]["state"]["steps"].as_array_mut().unwrap();
                            if case == "later_write" {
                                steps.push(step);
                            } else {
                                steps.insert(0, step);
                            }
                        }
                        "other_failed" => {
                            source["jobs"]["writer"] = json!({"name":"Writer","runs-on":"ubuntu-latest","steps":[{"run":"gh api -X DELETE repos/example/issues/42"}]});
                        }
                        "container" => source["jobs"]["state"]["container"] = json!("untrusted"),
                        "shell_env" => {
                            source["jobs"]["state"]["env"] = json!({"BASH_ENV":"setup.sh"})
                        }
                        "host_env" => source["env"] = json!({"GH_HOST":"foreign.example"}),
                        "foreign_repo_env" => {
                            source["jobs"]["state"]["env"] = json!({"GH_REPO":"foreign/repo"})
                        }
                        case if case.starts_with("actual_") => {
                            // Exercise the repository's actual source, not a copy of its command.
                            let name = match case {
                                "actual_ci" => "ci.yml",
                                "actual_codeql" => "codeql.yml",
                                "actual_macos" => "ci-macos.yml",
                                "actual_windows" => "ci-windows.yml",
                                "actual_website" => "website-check.yml",
                                _ => unreachable!(),
                            };
                            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                                .join("../../.github/workflows")
                                .join(name);
                            source =
                                serde_yaml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
                        }
                        case if case.starts_with("legacy_") => {
                            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
                                "tests/engine/pr_landing/infrastructure/fixtures/legacy-codeql.yml",
                            );
                            source =
                                serde_yaml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
                            let step = &mut source["jobs"]["pr-state"]["steps"][0];
                            match case {
                                "legacy_write" => {
                                    step["run"] = json!(
                                        step["run"]
                                            .as_str()
                                            .unwrap()
                                            .replace("gh api ", "gh api --method POST ")
                                    )
                                }
                                "legacy_binding" => {
                                    step["env"]["PR_NUMBER"] = json!("$(touch marker)")
                                }
                                "legacy_output" => {
                                    step["run"] = json!(
                                        step["run"]
                                            .as_str()
                                            .unwrap()
                                            .replace("$GITHUB_OUTPUT", "$GITHUB_ENV")
                                    )
                                }
                                _ => {}
                            }
                        }
                        "successful_dependent_read"
                        | "successful_dependent_write"
                        | "transitive_dependent_write"
                        | "fresh_release" => {
                            source["jobs"]["writer"] = json!({"name":"Writer","needs":if case == "transitive_dependent_write" {"bridge"} else {"state"},"if":"${{ always() }}","runs-on":"ubuntu-latest","steps":[{"name":"Write","run":"gh api -X POST repos/example/issues"}]});
                            if case == "successful_dependent_read" {
                                source["jobs"]["writer"]["steps"][0]["run"] =
                                    json!("gh api repos/example/pulls/42");
                            }
                            if case == "transitive_dependent_write" {
                                source["jobs"]["bridge"] = json!({"name":"Bridge","needs":"state","runs-on":"ubuntu-latest","steps":[{"name":"Read","run":"gh api repos/example/pulls/42"}]});
                            }
                        }
                        "fresh_write_authority" => {
                            source["jobs"]["protected"]["permissions"] = json!({"contents":"write"})
                        }
                        "fresh_foreign_token" => {
                            source["jobs"]["protected"]["env"] = json!({"GH_TOKEN":"foreign"})
                        }
                        "fresh_other_secret" => {
                            source["jobs"]["protected"]["env"] =
                                json!({"DEPLOY_TOKEN":"${{ secrets.DEPLOY_TOKEN }}"})
                        }
                        "fresh_unknown_action" => {
                            source["jobs"]["protected"]["steps"] =
                                json!([{"uses":"unknown/action@main"}])
                        }
                        "cancelled"
                        | "provider_unknown_step"
                        | "provider_unknown_outcome"
                        | "provider_incomplete"
                        | "actor" => {}
                        _ => unreachable!(),
                    }
                    let raw_source = if case == "legacy_codeql" {
                        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(
                            "tests/engine/pr_landing/infrastructure/fixtures/legacy-codeql.yml",
                        ))
                        .unwrap()
                    } else {
                        serde_yaml::to_string(&source).unwrap()
                    };
                    let fx = fixture_with_source(row, &raw_source);
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    action(
                        &host,
                        "pr_open",
                        &fx.open_input(&fx.candidate, &fx.base_sha),
                    )
                    .unwrap();
                    setup(&fx, "positive");
                    if case.starts_with("actual_") || case.starts_with("legacy_") {
                        update(&fx, "jobs", |value| {
                            value["jobs"][0]["name"] = json!("Live PR state");
                            value["jobs"][0]["steps"][0]["name"] = json!("Read live PR state");
                        });
                        update(&fx, "checks-before", |value| {
                            value["check_runs"][0]["name"] = json!("Live PR state")
                        });
                        let context = match case {
                            "actual_ci" => "Check / Clippy / Test",
                            "actual_codeql"
                            | "legacy_codeql"
                            | "legacy_wildcard_app"
                            | "legacy_write"
                            | "legacy_binding"
                            | "legacy_output" => "Analyze (rust)",
                            "actual_macos" => "macOS Sandbox",
                            "actual_windows" => "Windows cargo check (x86_64-pc-windows-msvc)",
                            "actual_website" => "Check and build",
                            _ => unreachable!(),
                        };
                        update(&fx, "rules", |value| {
                            value[0]["parameters"]["required_status_checks"][0]["context"] =
                                json!(context)
                        });
                        update(&fx, "checks-after", |value| {
                            value["check_runs"][0]["name"] = json!(context)
                        });
                    }
                    if matches!(case, "legacy_codeql" | "legacy_wildcard_app") {
                        update(&fx, "jobs", |value| {
                            value["jobs"][0]["steps"] = json!([
                                {"name":"Set up job","number":1,"status":"completed","conclusion":"success"},
                                {"name":"Read live PR state","number":2,"status":"completed","conclusion":"failure"},
                                {"name":"Complete job","number":3,"status":"completed","conclusion":"success"}
                            ]);
                        });
                    }
                    if case == "legacy_wildcard_app" {
                        update(&fx, "rules", |value| {
                            value[0]["parameters"]["required_status_checks"][0]["integration_id"] =
                                Value::Null
                        });
                    }
                    if matches!(case, "actual_ci" | "actual_macos") {
                        update(&fx, "rules", |value| {
                            let gates = value[0]["parameters"]["required_status_checks"]
                                .as_array_mut()
                                .unwrap();
                            for (id, job) in source["jobs"].as_object().unwrap() {
                                if id != "pr-state" && job.get("strategy").is_none() {
                                    let context = job["name"].as_str().unwrap_or(id);
                                    if !gates.iter().any(|gate| gate["context"] == context) {
                                        gates.push(json!({"context":context,"integration_id":900}));
                                    }
                                }
                            }
                        });
                        update(&fx, "checks-after", |value| {
                            let contexts: Value = serde_json::from_slice(
                                &fs::read(fx.forge.join("infra-rules")).unwrap(),
                            )
                            .unwrap();
                            let gates = contexts[0]["parameters"]["required_status_checks"]
                                .as_array()
                                .unwrap();
                            let prototype = value["check_runs"][0].clone();
                            *value = json!({"total_count":gates.len(),"check_runs":gates.iter().enumerate().map(|(index, gate)| { let mut check = prototype.clone(); check["name"] = gate["context"].clone(); check["id"] = json!(1200+index); check }).collect::<Vec<_>>()});
                        });
                    }
                    if matches!(
                        case,
                        "successful_dependent_read"
                            | "successful_dependent_write"
                            | "transitive_dependent_write"
                    ) {
                        update(&fx, "jobs", |value| {
                            let row = value["jobs"]
                                .as_array_mut()
                                .unwrap()
                                .iter_mut()
                                .find(|row| row["name"] == "Writer")
                                .unwrap();
                            row["conclusion"] = json!("success");
                            row["steps"] = json!([{"name":"Write","number":1,"status":"completed","conclusion":"success"}]);
                        });
                    }
                    match case {
                        "provider_unknown_step" => update(&fx, "jobs", |value| {
                            value["jobs"][0]["steps"].as_array_mut().unwrap().push(json!({"name":"Post action","number":2,"status":"completed","conclusion":"success"}));
                        }),
                        "provider_unknown_outcome" => update(&fx, "jobs", |value| {
                            value["jobs"][0]["steps"][0]["status"] = json!("unknown")
                        }),
                        "provider_incomplete" => update(&fx, "jobs", |value| {
                            value["jobs"].as_array_mut().unwrap().truncate(1);
                            value["total_count"] = json!(1);
                        }),
                        "actor" => update(&fx, "run-before", |value| {
                            value.as_object_mut().unwrap().remove("actor");
                        }),
                        _ => {}
                    }
                    if matches!(case, "other_failed" | "cancelled") {
                        update(&fx, "jobs", |value| {
                            let mut extra = value["jobs"][0].clone();
                            extra["id"] = json!(3000);
                            extra["name"] = json!("Writer");
                            extra["conclusion"] = json!(if case == "cancelled" {
                                "cancelled"
                            } else {
                                "failure"
                            });
                            value["jobs"]
                                .as_array_mut()
                                .unwrap()
                                .retain(|row| row["name"] != "Writer");
                            value["jobs"].as_array_mut().unwrap().push(extra);
                            value["total_count"] = json!(value["jobs"].as_array().unwrap().len());
                        });
                    }
                    let allowed = matches!(
                        case,
                        "safe_prior_read"
                            | "legacy_codeql"
                            | "legacy_wildcard_app"
                            | "successful_dependent_read"
                    ) || case.starts_with("actual_");
                    let result = orbit_engine::test_support::with_vcs_retry_wait(
                        |_| {},
                        || complete_infrastructure(&host, &fx),
                    );
                    assert_eq!(result.is_ok(), allowed, "{case}: {result:?}");
                    assert_eq!(rerun_count(&fx), usize::from(allowed), "{case}");
                    assert_eq!(
                        pr_infrastructure_reservations(&host.history(TASK_ID))
                            .unwrap()
                            .len(),
                        usize::from(allowed),
                        "{case}"
                    );
                    assert!(fx.merge_requests().is_empty());
                });
            }
        },
    );
}

#[test]
fn prerequisite_dag_work_is_bounded_by_nodes_and_edges() {
    isolated(
        "infrastructure::bounds::prerequisite_dag_work_is_bounded_by_nodes_and_edges",
        |sandbox| {
            for case in [
                "converging",
                "cycle",
                "unresolved",
                "oversized",
                "source_oversized",
            ] {
                in_fresh_row(sandbox, |row| {
                    let mut source: Value = serde_yaml::from_str(SOURCE).unwrap();
                    let jobs = source["jobs"].as_object_mut().unwrap();
                    let count = if case == "oversized" { 99 } else { 12 };
                    for index in 1..=count {
                        let needs = if index == 1 {
                            json!(["state"])
                        } else if index == 2 {
                            json!(["j1", "state"])
                        } else {
                            json!([format!("j{}", index - 1), format!("j{}", index - 2)])
                        };
                        jobs.insert(format!("j{index}"),json!({"needs":needs,"runs-on":"ubuntu-latest","steps":[{"name":"Read graph","run":"gh api repos/example/pulls/42"}]}));
                    }
                    if case == "cycle" {
                        jobs.get_mut("j1").unwrap()["needs"] = json!(["j12"]);
                    }
                    if case == "unresolved" {
                        jobs.get_mut("j1").unwrap()["needs"] = json!(["absent"]);
                    }
                    let node_count = jobs.len();
                    let edge_count: usize = jobs
                        .values()
                        .map(|job| match &job["needs"] {
                            Value::Array(edges) => edges.len(),
                            Value::String(_) => 1,
                            _ => 0,
                        })
                        .sum();
                    let mut source = serde_yaml::to_string(&source).unwrap();
                    if case == "source_oversized" {
                        source.push_str(&format!("\n# {}\n", "x".repeat(65536)));
                    }
                    let fx = fixture_with_source(row, &source);
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    action(
                        &host,
                        "pr_open",
                        &fx.open_input(&fx.candidate, &fx.base_sha),
                    )
                    .unwrap();
                    setup(&fx, "positive");
                    let work = std::rc::Rc::new(std::cell::Cell::new(0));
                    let observed = std::rc::Rc::clone(&work);
                    let result = orbit_engine::test_support::with_prerequisite_clock(
                        || 1000,
                        move |label, _| {
                            if matches!(
                                label,
                                "workflow node"
                                    | "workflow edge"
                                    | "selection node"
                                    | "selection edge"
                            ) {
                                observed.set(observed.get() + 1);
                                assert!(
                                    observed.get() <= 4 * (node_count + 2 * edge_count),
                                    "evidence and pre-mutation refresh each consume at most 2*(V+2E) for cycle validation and rerun selection"
                                );
                            }
                        },
                        || {
                            orbit_engine::test_support::with_vcs_retry_wait(
                                |_| {},
                                || complete_infrastructure(&host, &fx),
                            )
                        },
                    );
                    assert_eq!(result.is_ok(), case == "converging", "{case}: {result:?}");
                    if case == "converging" {
                        assert_eq!(
                            work.get(),
                            4 * (node_count + 2 * edge_count),
                            "each node and edge is visited once per cycle/selection traversal"
                        );
                    }
                    assert_eq!(
                        rerun_count(&fx),
                        usize::from(case == "converging"),
                        "{case}"
                    );
                    assert_eq!(
                        pr_infrastructure_reservations(&host.history(TASK_ID))
                            .unwrap()
                            .len(),
                        usize::from(case == "converging"),
                        "{case}"
                    );
                    assert!(fx.merge_requests().is_empty());
                });
            }
        },
    );
}

#[test]
fn prerequisite_expiry_stops_next_read_and_preserves_single_shot() {
    isolated(
        "infrastructure::bounds::prerequisite_expiry_stops_next_read_and_preserves_single_shot",
        |sandbox| {
            for case in [
                "evidence",
                "observation",
                "source",
                "revalidation",
                "lost_reply",
                "log_fallback",
                "shrinking",
            ] {
                in_fresh_row(sandbox, |row| {
                    let fx = fixture_with_workflow(row);
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    action(
                        &host,
                        "pr_open",
                        &fx.open_input(&fx.candidate, &fx.base_sha),
                    )
                    .unwrap();
                    setup(
                        &fx,
                        if case == "lost_reply" {
                            "lost"
                        } else {
                            "positive"
                        },
                    );
                    if case == "observation" {
                        let response = orbit_engine::run_private_vcs_operation("ci.prerequisite_evidence",&json!({"workspace_path":fx.repo,"pr":PR_NUMBER,"head_sha":fx.candidate,"head_branch":BRANCH,"base_branch":BASE,"deadline_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64+60000})).unwrap();
                        let evidence =
                            serde_json::from_value(response["evidence"].clone()).unwrap();
                        auto_fix::seed_auto_fix_history(
                            &host,
                            PR_INFRASTRUCTURE_RESERVED_EVENT,
                            &orbit_types::workflow::PrInfrastructureReservation {
                                execution: RUN_ID.into(),
                                evidence,
                            },
                        );
                    }
                    if case == "log_fallback" {
                        fs::write(fx.forge.join("infra-log"), "").unwrap();
                    }
                    // Baseline reads used for setup are not part of this completion.
                    fs::write(fx.forge.join("calls"), "").unwrap();
                    let now = std::rc::Rc::new(std::cell::Cell::new(1000_u64));
                    let clock = std::rc::Rc::clone(&now);
                    let forge = fx.forge.clone();
                    let reads = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
                    let observed = std::rc::Rc::clone(&reads);
                    let repo_reads = std::rc::Rc::new(std::cell::Cell::new(0));
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    input["max_wait_seconds"] = json!(1);
                    let result = orbit_engine::test_support::with_prerequisite_clock(
                        move || {
                            if case == "log_fallback"
                                && fs::read_to_string(forge.join("calls"))
                                    .unwrap()
                                    .lines()
                                    .any(|line| line == "run view")
                            {
                                clock.set(2001);
                            }
                            clock.get()
                        },
                        move |label, timeout| {
                            if timeout == 0 {
                                return;
                            }
                            assert!(
                                timeout <= 1000,
                                "{case}: {label} receives at most the remaining completion budget, got {timeout}"
                            );
                            observed.borrow_mut().push(label.into());
                            if label == "workflow prerequisite read repos/{owner}/{repo}" {
                                repo_reads.set(repo_reads.get() + 1);
                            }
                            let expired = match case {
                                "evidence" | "observation" => {
                                    label == "workflow prerequisite read repos/{owner}/{repo}"
                                        && repo_reads.get() == 1
                                }
                                "source" => label == "workflow blob",
                                // Expire after the first successful read of pre-mutation observation.
                                "revalidation" => {
                                    label == "workflow prerequisite read repos/{owner}/{repo}"
                                        && repo_reads.get() == 2
                                }
                                "lost_reply" => {
                                    label == "workflow prerequisite read repos/{owner}/{repo}"
                                        && repo_reads.get() == 4
                                }
                                _ => false,
                            };
                            if expired {
                                now.set(2001);
                            } else if case == "shrinking" {
                                assert_eq!(
                                    timeout,
                                    2000 - now.get(),
                                    "each successive request shrinks with the same absolute budget"
                                );
                                now.set(now.get() + 100);
                            }
                        },
                        || {
                            orbit_engine::test_support::with_vcs_retry_wait(
                                |_| {},
                                || action(&host, "pr_complete", &input),
                            )
                        },
                    );
                    let error = result.expect_err(case).to_string();
                    assert!(
                        error.contains("deadline"),
                        "{case}: {error}; reads={:?}",
                        reads.borrow()
                    );
                    let calls = fs::read_to_string(fx.forge.join("calls")).unwrap();
                    if matches!(case, "evidence" | "observation") {
                        assert!(
                            !calls
                                .lines()
                                .any(|line| line == "api repos/{owner}/{repo}/pulls/42"),
                            "expiry after repo read forbids the next PR read"
                        );
                    }
                    if matches!(
                        case,
                        "evidence" | "observation" | "revalidation" | "lost_reply"
                    ) {
                        assert_eq!(
                            reads.borrow().last().map(String::as_str),
                            Some("workflow prerequisite read repos/{owner}/{repo}"),
                            "expiry forbids every subsequent read"
                        );
                        let expected = match case {
                            "revalidation" => 1,
                            "lost_reply" => 3,
                            _ => 0,
                        };
                        assert_eq!(
                            calls
                                .lines()
                                .filter(|line| *line == "api repos/{owner}/{repo}/pulls/42")
                                .count(),
                            expected,
                            "no next PR read after expiry: {case}"
                        );
                    }
                    if case == "source" {
                        assert_eq!(
                            reads.borrow().last().map(String::as_str),
                            Some("workflow blob"),
                            "expiry forbids starting git show"
                        );
                    }
                    if case == "log_fallback" {
                        assert_eq!(
                            calls.lines().filter(|line| *line == "run view").count(),
                            1,
                            "expired log read launches no fallback"
                        );
                    }
                    assert_eq!(
                        rerun_count(&fx),
                        usize::from(case == "lost_reply"),
                        "{case}: {calls}"
                    );
                    assert_eq!(
                        pr_infrastructure_reservations(&host.history(TASK_ID))
                            .unwrap()
                            .len(),
                        usize::from(matches!(
                            case,
                            "observation" | "revalidation" | "lost_reply"
                        )),
                        "{case}"
                    );
                    assert!(fx.merge_requests().is_empty());
                });
            }
        },
    );
}
