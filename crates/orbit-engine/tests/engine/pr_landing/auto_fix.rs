//! Resumed auto-fix authority through the public job and PR boundaries.

use super::*;
use std::io::Write as _;

// A successful publication checkpoint deliberately keeps H0 when completion
// fails after repairing it. Resume executes completion as R2, retaining R0's
// task/worktree ownership but inheriting only its durable ancestor authority.
fn resume_auto_fix(fx: &Fixture, host: &DeliveryHost, reviewed: bool) -> Result<Value, String> {
    let mut input = fx.complete_ungated_input();
    input["auto_fix"] = json!(true);
    input["completion"] = json!("review");
    input["published_head_sha"] = json!("{{ steps.push.output.local_sha }}");
    if reviewed {
        input["reviewed_head_sha"] = json!(fx.candidate);
    }
    let mut job = job_asset(json!([
        {"id": "commit", "target": "activity:fixture_checkpoint"},
        {"id": "push", "target": "activity:fixture_checkpoint"},
        {"id": "complete_pr", "target": "activity:pr_complete", "default_input": input}
    ]));
    let mut catalog = V2ActivityCatalog::new();
    for (name, action) in [
        ("fixture_checkpoint", "test_stub_checkpoint"),
        ("pr_complete", "pr_complete"),
    ] {
        catalog.insert(
            name,
            ActivityV2 {
                description: name.into(),
                input_schema_json: Value::Null,
                output_schema_json: Value::Null,
                fs_profile: None,
                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: action.into(),
                    config: Value::Null,
                }),
            },
        );
    }
    host.script("test_stub_checkpoint", json!({"unexpected": true}));
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    let mut resume = PipelineState::new(RUN_ID.into(), "fixture-job".into(), json!({}));
    resume.step_states = BTreeMap::from([
        (0, orbit_types::workflow::JobRunState::Success),
        (1, orbit_types::workflow::JobRunState::Success),
        (2, orbit_types::workflow::JobRunState::Failed),
    ]);
    resume.step_outputs = BTreeMap::from([
        (0, json!({"commit_sha": fx.candidate})),
        (1, json!({"local_sha": fx.candidate})),
    ]);
    let audit = tempfile::tempdir().unwrap();
    let (writer, _, _) = build_writer_and_sinks(audit.path(), "jrun-resumed");
    let outcome =
        execute_job_with_resume(&job, json!({}), "jrun-resumed", writer, host, Some(&resume))
            .map_err(|error| error.to_string())?;
    assert!(
        !host
            .scripted_inputs
            .lock()
            .unwrap()
            .contains_key("test_stub_checkpoint"),
        "successful prefix is restored, never replayed"
    );
    if !outcome.success {
        return Err(outcome.message.unwrap_or_default());
    }
    Ok(outcome.pipeline["complete_pr"].clone())
}

pub(super) fn seed_auto_fix_history(
    host: &DeliveryHost,
    event: &str,
    note: &impl serde::Serialize,
) {
    host.history
        .lock()
        .unwrap()
        .entry(TASK_ID.into())
        .or_default()
        .push(TaskHistoryEntry {
            at: Utc::now(),
            by: "system".into(),
            event: event.into(),
            note: Some(serde_json::to_string(note).unwrap()),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        });
}

fn auto_fix_resume_fixture(fx: &Fixture, source: &str) -> DeliveryHost {
    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
    action(
        &host,
        "pr_open",
        &fx.open_input(&fx.candidate, &fx.base_sha),
    )
    .unwrap();
    for (run, parent) in [
        (RUN_ID, None),
        ("jrun-middle", Some(source)),
        ("jrun-resumed", Some("jrun-middle")),
    ] {
        host.runs.lock().unwrap().insert(
            run.into(),
            serde_json::from_value(json!({
                "run_id": run, "job_id": "fixture-job", "attempt": 1, "state": "failed",
                "scheduled_at": Utc::now(), "created_at": Utc::now(),
                "retry_source_run_id": parent
            }))
            .unwrap(),
        );
    }
    seed_auto_fix_history(
        &host,
        PR_AUTO_FIX_ADMITTED_EVENT,
        &PrAutoFixAttempt {
            pr_number: PR_NUMBER.into(),
            head_sha: fx.candidate.clone(),
            blocker: PrAutoFixBlocker::FailedChecks,
            attempt: 1,
            budget: PR_AUTO_FIX_BUDGET,
            run_id: RUN_ID.into(),
            crew: "fixture".into(),
            crew_source: "fixture".into(),
        },
    );
    host
}

#[test]
fn auto_fix_resume_adopts_only_source_lineage_publication_chains() {
    isolated(
        "auto_fix::auto_fix_resume_adopts_only_source_lineage_publication_chains",
        |sandbox| {
            for case in [
                "published",
                "unrelated",
                "wrong_pr",
                "wrong_branch",
                "noncontiguous",
                "changed_head",
                "reviewed",
                "budget",
                "cross_job",
                "missing_parent",
                "cycle",
            ] {
                in_fresh_row(sandbox, |row| {
                    let fx = Fixture::new(row);
                    let host = auto_fix_resume_fixture(&fx, RUN_ID);
                    if case == "unrelated" {
                        host.runs
                            .lock()
                            .unwrap()
                            .get_mut("jrun-middle")
                            .unwrap()
                            .retry_source_run_id = None;
                    } else if case == "cross_job" {
                        host.runs.lock().unwrap().get_mut(RUN_ID).unwrap().job_id =
                            "other-job".into();
                    } else if case == "missing_parent" {
                        host.runs
                            .lock()
                            .unwrap()
                            .get_mut("jrun-middle")
                            .unwrap()
                            .retry_source_run_id = Some("missing".into());
                    } else if case == "cycle" {
                        host.runs
                            .lock()
                            .unwrap()
                            .get_mut(RUN_ID)
                            .unwrap()
                            .retry_source_run_id = Some("jrun-resumed".into());
                    }
                    let mut repair = fx.commit("repair.txt", "repair\n");
                    seed_auto_fix_history(
                        &host,
                        PR_AUTO_FIX_PUSHED_EVENT,
                        &PrAutoFixPush {
                            pr_number: if case == "wrong_pr" { "99" } else { PR_NUMBER }.into(),
                            from_head_sha: if case == "noncontiguous" {
                                fx.base_sha.clone()
                            } else {
                                fx.candidate.clone()
                            },
                            to_head_sha: repair.clone(),
                            blocker: PrAutoFixBlocker::FailedChecks,
                            run_id: RUN_ID.into(),
                        },
                    );
                    if case == "published" {
                        let next = fx.commit("repair2.txt", "next repair\n");
                        seed_auto_fix_history(
                            &host,
                            PR_AUTO_FIX_ADMITTED_EVENT,
                            &PrAutoFixAttempt {
                                pr_number: PR_NUMBER.into(),
                                head_sha: repair.clone(),
                                blocker: PrAutoFixBlocker::Conflict,
                                attempt: 2,
                                budget: PR_AUTO_FIX_BUDGET,
                                run_id: "jrun-middle".into(),
                                crew: "fixture".into(),
                                crew_source: "fixture".into(),
                            },
                        );
                        seed_auto_fix_history(
                            &host,
                            PR_AUTO_FIX_PUSHED_EVENT,
                            &PrAutoFixPush {
                                pr_number: PR_NUMBER.into(),
                                from_head_sha: repair,
                                to_head_sha: next.clone(),
                                blocker: PrAutoFixBlocker::Conflict,
                                run_id: "jrun-middle".into(),
                            },
                        );
                        repair = next;
                    }
                    if case == "wrong_branch" {
                        git(
                            &fx.repo,
                            &[
                                "push",
                                "origin",
                                &format!("{repair}:refs/heads/other-branch"),
                            ],
                        );
                        fs::write(fx.forge.join("pr-head"), "other-branch\n").unwrap();
                    }
                    if case == "changed_head" {
                        fx.commit("foreign.txt", "foreign\n");
                    }
                    // Exercise the publication ledger alone, independently of
                    // the local-repair admission consumer.
                    git(&fx.repo, &["reset", "--hard", &fx.candidate]);
                    if case == "budget" {
                        for attempt in 2..=PR_AUTO_FIX_BUDGET {
                            seed_auto_fix_history(
                                &host,
                                PR_AUTO_FIX_ADMITTED_EVENT,
                                &PrAutoFixAttempt {
                                    pr_number: PR_NUMBER.into(),
                                    head_sha: format!("spent-{attempt}"),
                                    blocker: PrAutoFixBlocker::Conflict,
                                    attempt,
                                    budget: PR_AUTO_FIX_BUDGET,
                                    run_id: RUN_ID.into(),
                                    crew: "fixture".into(),
                                    crew_source: "fixture".into(),
                                },
                            );
                        }
                        fx.script_checks(&["dirty"]);
                    } else {
                        fx.script_checks(&["pending", "success"]);
                    }
                    let result = resume_auto_fix(&fx, &host, case == "reviewed");
                    if case == "published" {
                        let output = result.expect("resume must watch the source-run repair instead of refusing H1 as stale");
                        assert_eq!(output["merge"]["reason"], "auto_fix_mergeable");
                        assert_eq!(output["merge"]["head_sha"], repair);
                    } else {
                        let error = result.expect_err(case);
                        let expected = match case {
                            "reviewed" => "review_gate_stale",
                            "budget" => "auto_fix_budget_exhausted",
                            "cross_job" | "missing_parent" | "cycle" => "auto_fix",
                            _ => "delivery_evidence_stale",
                        };
                        assert!(error.contains(expected), "{case}: {error}");
                    }
                    assert!(
                        fx.merge_requests().is_empty(),
                        "repair authority never merges: {case}"
                    );
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    assert_eq!(
                        pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).len(),
                        if case == "budget" {
                            3
                        } else if case == "published" {
                            2
                        } else {
                            1
                        }
                    );
                });
            }
        },
    );
}

#[test]
fn auto_fix_resume_publishes_source_admitted_local_repair_safely() {
    isolated(
        "auto_fix::auto_fix_resume_publishes_source_admitted_local_repair_safely",
        |sandbox| {
            for case in [
                "unpublished",
                "reviewed",
                "remote_moved",
                "validation_failed",
                "unrelated",
                "detached",
                "wrong_branch",
            ] {
                in_fresh_row(sandbox, |row| {
                    let fx = Fixture::new(row);
                    let host = auto_fix_resume_fixture(&fx, RUN_ID);
                    if case == "unrelated" {
                        host.runs
                            .lock()
                            .unwrap()
                            .get_mut("jrun-middle")
                            .unwrap()
                            .retry_source_run_id = None;
                    }
                    fs::write(fx.repo.join("repair.txt"), "repair\n").unwrap();
                    git(&fx.repo, &["add", "repair.txt"]);
                    git(&fx.repo, &["commit", "-m", "admitted repair"]);
                    let repair = fx.head();
                    host.require_commands(&[if case == "validation_failed" {
                        "exit 23"
                    } else {
                        "test -f repair.txt"
                    }]);
                    if case == "remote_moved" {
                        git(&fx.repo, &["push", "origin", BRANCH]);
                        fx.commit("foreign.txt", "foreign\n");
                        git(&fx.repo, &["reset", "--hard", &repair]);
                    } else if case == "detached" {
                        git(&fx.repo, &["checkout", "--detach"]);
                    } else if case == "wrong_branch" {
                        git(&fx.repo, &["checkout", "-b", "other-branch"]);
                    }
                    fx.script_checks(&["success"]);
                    let remote_before = fx.remote_tip(BRANCH);
                    let result = resume_auto_fix(&fx, &host, case == "reviewed");
                    match case {
                        "unpublished" => {
                            let output = result.expect("source-admitted committed repair must validate, publish and watch on resume");
                            assert_eq!(output["merge"]["head_sha"], repair);
                            assert_eq!(fx.remote_tip(BRANCH), repair);
                            let pushes = orbit_types::workflow::pr_auto_fix_pushes(
                                &host.history(TASK_ID),
                                PR_NUMBER,
                            );
                            assert_eq!(pushes.len(), 1);
                            assert_eq!(pushes[0].run_id, "jrun-resumed");
                            assert_eq!(pushes[0].from_head_sha, fx.candidate);
                        }
                        "reviewed" => {
                            let output = result.expect(
                                "resumed reviewed repair returns to review before publication",
                            );
                            assert_eq!(output["re_review_required"], true);
                            assert_eq!(output["rebased"]["head_sha"], repair);
                            assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
                        }
                        "remote_moved" | "validation_failed" => {
                            let error = result.expect_err(case);
                            let expected = if case == "remote_moved" {
                                "moved away"
                            } else {
                                "23"
                            };
                            assert!(error.contains(expected), "{case}: {error}");
                            assert_eq!(
                                fx.remote_tip(BRANCH),
                                remote_before,
                                "failed repair must leave the remote unchanged"
                            );
                        }
                        _ => {
                            result.expect(
                                "without admitted branch ownership completion only watches H0",
                            );
                            assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
                        }
                    }
                    assert!(
                        fx.merge_requests().is_empty(),
                        "repair authority never merges: {case}"
                    );
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    assert_eq!(
                        pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).len(),
                        1
                    );
                    if case != "unpublished" {
                        assert!(
                            orbit_types::workflow::pr_auto_fix_pushes(
                                &host.history(TASK_ID),
                                PR_NUMBER
                            )
                            .is_empty()
                        );
                    }
                });
            }
        },
    );
}

// CI API answers belong to the same bare-remote head read by the forge.
// Unlike a scripted green status, source_contract evaluates the published tree.
pub(super) fn ci_answer(host: &DeliveryHost, operation: &str, input: &Value) -> Option<Value> {
    match operation {
        "pr.required_checks" => {
            let head = git(
                &host.repo,
                &["ls-remote", "origin", &format!("refs/heads/{BRANCH}")],
            );
            let forge = host.repo.parent().unwrap().join("forge");
            let reads: usize = fs::read_to_string(forge.join("reads"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let script = fs::read_to_string(forge.join("checks")).unwrap();
            let observed = script
                .lines()
                .nth(reads.saturating_sub(1))
                .or_else(|| script.lines().last())
                .unwrap();
            let status = if observed == "pending" {
                "IN_PROGRESS"
            } else {
                "COMPLETED"
            };
            let conclusion = if observed == "pending" {
                ""
            } else if observed == "success" {
                "SUCCESS"
            } else {
                "FAILURE"
            };
            Some(
                json!({"pull_request":{"headRefOid":head.split_whitespace().next().unwrap(),"commits":{"nodes":[{"commit":{
                    "oid":head.split_whitespace().next().unwrap(),
                    "statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},"nodes":[{"name":"test","isRequired":true,"status":status,"conclusion":conclusion}]}}
                }}]}}}),
            )
        }
        "ci.commit_jobs" => {
            let commit = input["commit"].as_str().unwrap();
            let contents = git(&host.repo, &["show", &format!("{commit}:src/contract")]);
            let run = u64::from_str_radix(&commit[..12], 16).unwrap();
            Some(
                json!({"jobs":[{"name":"test","workflow":"CI","conclusion":if contents == "fixed" {"success"} else {"failure"},"run_id":run,"job_id":run+1,"failed_step":"source contract"}]}),
            )
        }

        "ci.job_log" => Some(
            json!({"log":"error: src/contract: expected fixed, found broken\nsource contract failed"}),
        ),
        _ => None,
    }
}

pub(super) fn perform_ci_repair(
    host: &DeliveryHost,
    input: &Value,
) -> Result<Value, DispatchError> {
    let mode = host.scripted.lock().unwrap()["test_ci_repair"]["mode"]
        .as_str()
        .unwrap()
        .to_string();
    host.scripted_inputs
        .lock()
        .unwrap()
        .insert("test_ci_repair".into(), input.clone());
    let error = input["error_message"].as_str().unwrap();
    assert!(
        orbit_types::workflow::is_pr_auto_fix_check_repair(error),
        "{mode}: {error}"
    );
    let report = error
        .split("before writing retry, write ")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .unwrap_or(".orbit/tmp/baseline-scope.json");
    let head = git(&host.repo, &["rev-parse", "HEAD"]);
    fs::write(host.repo.join("src/contract"), "fixed\n").unwrap();
    let mut paths = vec![
        json!({"path":"src/contract","reason":"fix the source contract named by the evidenced CI job"}),
    ];
    if mode == "unrelated" {
        fs::write(host.repo.join("unrelated.txt"), "unrelated work\n").unwrap();
    } else if mode == "workflow" {
        fs::create_dir_all(host.repo.join(".github/workflows")).unwrap();
        fs::write(host.repo.join(".github/workflows/ci.yml"), "weakened\n").unwrap();
        paths.push(json!({"path":".github/workflows/ci.yml","reason":"bypass check"}));
    }
    fs::create_dir_all(host.repo.join(".orbit/tmp")).unwrap();
    if mode != "missing" {
        fs::write(
            host.repo.join(report),
            serde_json::to_vec(&json!({
                "schema_version":1, "run_id":input["run_id"],
                "failed_head":if mode == "stale" { "stale" } else { &head },
                "cause":"shared-base src/contract violates the required source contract",
                "paths":paths,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    if mode == "remote_moved" {
        // Another writer advances the published branch while recovery edits locally.
        let foreign = git(
            &host.repo,
            &["commit-tree", "HEAD^{tree}", "-p", &head, "-m", "foreign"],
        );
        git(
            &host.repo,
            &["push", "origin", &format!("{foreign}:refs/heads/{BRANCH}")],
        );
    }
    Ok(json!({"summary":"repair the shared-base source contract"}))
}

#[test]
fn docs_only_auto_fix_repairs_shared_source_and_watches_published_green_head() {
    isolated(
        "auto_fix::docs_only_auto_fix_repairs_shared_source_and_watches_published_green_head",
        |sandbox| {
            for mode in [
                "unrelated",
                "repair",
                "landed",
                "reviewed",
                "landed_reviewed",
                "budget",
                "review_required",
                "missing",
                "stale",
                "workflow",
                "remote_moved",
                "validation_failed",
            ] {
                in_fresh_row(sandbox, |row| {
                    let mut fx = Fixture::new(row);
                    fs::create_dir_all(fx.repo.join(".git/info")).unwrap();
                    fs::write(fx.repo.join(".git/info/exclude"), ".orbit/\n").unwrap();
                    fx.commit_on_base("src/contract", "broken\n");
                    git(&fx.repo, &["reset", "--hard", BASE]);
                    fx.base_sha = fx.head();
                    delete_remote_branch(&fx, BRANCH);
                    fx.candidate = fx.commit("README.md", "documentation only\n");
                    assert_eq!(
                        git(
                            &fx.repo,
                            &["diff", "--name-only", &fx.base_sha, &fx.candidate]
                        ),
                        "README.md"
                    );
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    host.set_context_files(TASK_ID, &["file:README.md"]);
                    action(
                        &host,
                        "pr_open",
                        &fx.open_input(&fx.candidate, &fx.base_sha),
                    )
                    .unwrap();
                    host.script("test_ci_repair", json!({"mode":mode}));
                    host.script(
                        "test_checkpoint",
                        json!({"phase":"commit","task_id":TASK_ID,"commit_sha":fx.candidate}),
                    );
                    host.require_commands(&[if mode == "validation_failed" {
                        "exit 23"
                    } else {
                        "test \"$(cat src/contract)\" = fixed"
                    }]);
                    fx.script_checks(&["source_contract"]);
                    if matches!(mode, "landed" | "landed_reviewed") {
                        fx.commit_on_base("src/contract", "fixed\n");
                    }
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    if matches!(mode, "reviewed" | "landed_reviewed") {
                        input["reviewed_head_sha"] = json!(fx.candidate);
                        input["re_review_on_conflict"] = json!(true);
                        host.script(
                            "test_checkpoint",
                            json!({"phase":"commit","reviewed_head_sha":fx.candidate}),
                        );
                    }
                    if mode == "review_required" {
                        fx.script_checks(&["review_required"]);
                    }
                    if mode == "budget" {
                        for attempt in 1..=PR_AUTO_FIX_BUDGET {
                            seed_auto_fix_history(
                                &host,
                                PR_AUTO_FIX_ADMITTED_EVENT,
                                &PrAutoFixAttempt {
                                    pr_number: PR_NUMBER.into(),
                                    head_sha: format!("spent-{attempt}"),
                                    blocker: PrAutoFixBlocker::FailedChecks,
                                    attempt,
                                    budget: PR_AUTO_FIX_BUDGET,
                                    run_id: RUN_ID.into(),
                                    crew: "fixture".into(),
                                    crew_source: "fixture".into(),
                                },
                            );
                        }
                    }
                    let mut job = job_asset(json!([
                        {"id":"commit","target":"activity:checkpoint","recovery_activity":"step_failure_recovery"},
                        {"id":"complete_pr","target":"activity:pr_complete","recovery_activity":"pr_conflict_recovery","default_input":input}
                    ]));
                    let mut catalog = V2ActivityCatalog::new();
                    for (name, action) in [
                        ("checkpoint", "test_checkpoint"),
                        ("pr_complete", "pr_complete"),
                        ("step_failure_recovery", "test_ci_repair"),
                        ("pr_conflict_recovery", "unexpected_conflict"),
                    ] {
                        catalog.insert(
                            name,
                            ActivityV2 {
                                description: name.into(),
                                input_schema_json: Value::Null,
                                output_schema_json: Value::Null,
                                fs_profile: None,
                                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                                    action: action.into(),
                                    config: Value::Null,
                                }),
                            },
                        );
                    }
                    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
                    let audit = tempfile::tempdir().unwrap();
                    let (writer, _, _) = build_writer_and_sinks(audit.path(), RUN_ID);
                    let outcome =
                        execute_job_with_resume(&job, json!({}), RUN_ID, writer, &host, None);
                    if matches!(mode, "repair" | "landed") {
                        let result = outcome.expect(mode);
                        assert!(result.success, "{result:?}");
                        let head = fx.head();
                        assert_ne!(head, fx.candidate);
                        assert_eq!(fx.remote_tip(BRANCH), head);
                        assert_eq!(result.pipeline["complete_pr"]["merge"]["head_sha"], head);
                        assert_eq!(
                            result.pipeline["complete_pr"]["merge"]["reason"],
                            "auto_fix_mergeable"
                        );
                        if mode == "repair" {
                            let evidence = host
                                .validation_log(TASK_ID, &format!("auto-fix/{RUN_ID}/{head}.json"));
                            assert_eq!(evidence["changed_paths"], json!(["src/contract"]));
                            assert_eq!(evidence["failed_head"], fx.candidate);
                            assert_eq!(evidence["repair_head"], head);
                            assert_eq!(host.widenings().last().unwrap().3, vec!["src/contract"]);
                        } else {
                            let evidence = host.validation_log(
                                TASK_ID,
                                &format!("auto-fix/{RUN_ID}/landed-{}.json", fx.candidate),
                            );
                            assert_eq!(evidence["failed_head"], fx.candidate);
                            assert!(
                                !host
                                    .scripted_inputs
                                    .lock()
                                    .unwrap()
                                    .contains_key("test_ci_repair"),
                                "landed repair is adopted without duplicate agent edits"
                            );
                            assert_eq!(
                                git(&fx.repo, &["diff", "--name-only", BASE, "HEAD"]),
                                "README.md"
                            );
                        }
                        let log =
                            host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
                        assert_eq!(log["tested_head"], head);
                    } else if matches!(mode, "reviewed" | "landed_reviewed") {
                        let result = outcome.expect(mode);
                        assert!(result.success, "{result:?}");
                        assert_eq!(result.pipeline["complete_pr"]["re_review_required"], true);
                        assert_eq!(
                            fx.remote_tip(BRANCH),
                            fx.candidate,
                            "review precedes publication"
                        );
                        assert_ne!(fx.head(), fx.candidate);
                    } else {
                        let error = match outcome {
                            Ok(result) => {
                                assert!(!result.success, "{mode}: {result:?}");
                                result.message.unwrap_or_default()
                            }
                            Err(error) => error.to_string(),
                        };
                        let expected = match mode {
                            "unrelated" => "unexplained_paths",
                            "remote_moved" => "moved away",
                            "validation_failed" => "23",
                            "budget" => "auto_fix_budget_exhausted",
                            "review_required" => "REVIEW_REQUIRED",
                            _ => "CI repair scope",
                        };
                        assert!(error.contains(expected), "{mode}: {error}");
                        if matches!(mode, "budget" | "review_required") {
                            assert!(
                                !host
                                    .scripted_inputs
                                    .lock()
                                    .unwrap()
                                    .contains_key("test_ci_repair")
                            );
                        }
                        if mode != "remote_moved" {
                            assert_eq!(fx.remote_tip(BRANCH), fx.candidate, "{mode}");
                        }
                        if !matches!(mode, "remote_moved" | "validation_failed") {
                            assert_eq!(
                                fx.head(),
                                fx.candidate,
                                "scope refusal precedes commit: {mode}"
                            );
                        }
                    }
                    assert_eq!(
                        pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).len(),
                        if mode == "budget" {
                            3
                        } else if mode == "review_required" {
                            0
                        } else {
                            1
                        }
                    );
                    assert!(
                        fx.merge_requests().is_empty(),
                        "repair does not grant completion"
                    );
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                });
            }
        },
    );
}

// An admitted CI repair is published through the real PR executor first.
// Completion then receives its original H0 checkpoint after the base advances.
#[test]
fn published_ci_repair_conflict_refresh_keeps_authenticated_head_and_guards() {
    isolated(
        "auto_fix::published_ci_repair_conflict_refresh_keeps_authenticated_head_and_guards",
        |sandbox| {
            for case in [
                "refresh",
                "reviewed",
                "review_required",
                "real_conflict",
                "ordinary_stale",
                "budget",
                "validation_failed",
                "remote_moved",
                "remote_deleted",
                "wrong_base",
            ] {
                in_fresh_row(sandbox, |row| {
                    tracing::info!(case, "published CI repair conflict");
                    let fx = Fixture::new(row);
                    let host = auto_fix_resume_fixture(&fx, RUN_ID);
                    fs::write(fx.repo.join("repair.txt"), "CI repair\n").unwrap();
                    git(&fx.repo, &["add", "repair.txt"]);
                    git(&fx.repo, &["commit", "-m", "admitted CI repair"]);
                    let repaired = fx.head();
                    host.require_commands(&["test -f repair.txt"]);
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    let published = action(&host, "pr_complete", &input).unwrap();
                    assert_eq!(published["merge"]["head_sha"], repaired);
                    assert_eq!(fx.remote_tip(BRANCH), repaired);
                    let pushes = orbit_types::workflow::pr_auto_fix_pushes(
                        &host.history(TASK_ID),
                        PR_NUMBER,
                    );
                    assert_eq!(pushes.len(), 1);
                    assert_eq!(pushes[0].from_head_sha, fx.candidate);
                    assert_eq!(pushes[0].to_head_sha, repaired);
                    let failed_history = host.history(TASK_ID);

                    if case == "real_conflict" {
                        fx.commit_on_base(FEATURE, "base conflicts with candidate\n");
                    } else {
                        fx.advance_base();
                    }
                    let base = fx.remote_tip(BASE);
                    let mut checks = vec!["success"; fx.status_reads()];
                    checks.extend(["dirty", "success"]);
                    fx.script_checks(&checks);
                    if matches!(case, "reviewed" | "review_required") {
                        input["reviewed_head_sha"] = json!(repaired);
                        input["re_review_on_conflict"] = json!(case == "reviewed");
                    } else if case == "ordinary_stale" {
                        input["auto_fix"] = json!(false);
                    } else if case == "budget" {
                        for attempt in 2..=PR_AUTO_FIX_BUDGET {
                            seed_auto_fix_history(
                                &host,
                                PR_AUTO_FIX_ADMITTED_EVENT,
                                &PrAutoFixAttempt {
                                    pr_number: PR_NUMBER.into(),
                                    head_sha: format!("spent-{attempt}"),
                                    blocker: PrAutoFixBlocker::Conflict,
                                    attempt,
                                    budget: PR_AUTO_FIX_BUDGET,
                                    run_id: "sibling-run".into(),
                                    crew: "fixture".into(),
                                    crew_source: "fixture".into(),
                                },
                            );
                        }
                    } else if case == "validation_failed" {
                        host.require_commands(&["exit 23"]);
                    } else if case == "remote_moved" {
                        // Validation is the deterministic rendezvous: another
                        // writer moves the remote after rebase's ownership read.
                        host.require_commands(&[&format!("repair_race_sha=$(git -c user.name=Other -c user.email=other@example.invalid commit-tree HEAD^{{tree}} -p {repaired} -m foreign) && git push origin \"$repair_race_sha:refs/heads/{BRANCH}\"")]);
                    } else if case == "remote_deleted" {
                        host.require_commands(&[&format!("git push origin :refs/heads/{BRANCH}")]);
                    } else if case == "wrong_base" {
                        fs::write(fx.forge.join("pr-base"), "main\n").unwrap();
                    }
                    let result = action(&host, "pr_complete", &input);
                    match case {
                        "refresh" => {
                            let output = result.expect("authenticated H1 must refresh automatically despite the original H0 checkpoint");
                            let refreshed = fx.head();
                            assert_ne!(refreshed, repaired);
                            assert_eq!(fx.remote_tip(BRANCH), refreshed);
                            assert_eq!(output["merge"]["head_sha"], refreshed);
                            assert_eq!(git(&fx.repo, &["show", "HEAD:repair.txt"]), "CI repair");
                            assert_eq!(
                                host.validation_log(
                                    TASK_ID,
                                    &format!("validation/{RUN_ID}/0.json")
                                )["tested_head"],
                                refreshed
                            );
                            let pushes = orbit_types::workflow::pr_auto_fix_pushes(
                                &host.history(TASK_ID),
                                PR_NUMBER,
                            );
                            assert_eq!(pushes.len(), 2);
                            assert_eq!(pushes[1].from_head_sha, repaired);
                            assert_eq!(pushes[1].to_head_sha, refreshed);
                            assert_eq!(pushes[1].blocker, PrAutoFixBlocker::Conflict);
                        }
                        "reviewed" => {
                            let output = result.expect(
                                "reviewed authenticated H1 must rebase for independent re-review",
                            );
                            assert_eq!(output["re_review_required"], true);
                            assert_eq!(output["rebased"]["head_sha_before"], repaired);
                            assert_eq!(output["rebased"]["remote_sha_before"], repaired);
                            assert_eq!(output["rebased"]["head_sha"], fx.head());
                            assert_ne!(fx.head(), repaired);
                            assert_eq!(
                                host.validation_log(
                                    TASK_ID,
                                    &format!("validation/{RUN_ID}/0.json")
                                )["tested_head"],
                                fx.head()
                            );
                            assert_eq!(
                                fx.remote_tip(BRANCH),
                                repaired,
                                "re-review precedes publication"
                            );
                        }
                        "real_conflict" => {
                            let conflict = completion_conflict(result);
                            assert_eq!(conflict.target_base_sha, base);
                            assert_eq!(
                                fx.remote_tip(BRANCH),
                                repaired,
                                "conflict recovery retains the owned H1 lease"
                            );
                            assert_eq!(conflict.conflicting_paths, vec![FEATURE.to_string()]);
                            let recovered_fx = Fixture {
                                candidate: repaired.clone(),
                                ..fx
                            };
                            let recovered =
                                recover_completion(&recovered_fx, &host, &conflict, "resolved\n");
                            let mut checks = vec!["success"; recovered_fx.status_reads()];
                            checks.extend(["dirty", "success"]);
                            recovered_fx.script_checks(&checks);
                            let output = action(&host, "pr_complete", &input).expect(
                                "certified conflict recovery refreshes the same authenticated head",
                            );
                            assert_eq!(output["merge"]["head_sha"], recovered);
                            assert_eq!(recovered_fx.remote_tip(BRANCH), recovered);
                            assert_eq!(
                                host.validation_log(
                                    TASK_ID,
                                    &format!("validation/{RUN_ID}/0.json")
                                )["tested_head"],
                                recovered
                            );
                            assert!(recovered_fx.merge_requests().is_empty());
                            assert_eq!(recovered_fx.remote_tip(BASE), base);
                            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                            assert_eq!(input["published_head_sha"], pushes[0].from_head_sha);
                            assert_eq!(
                                pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).len(),
                                2
                            );
                            assert!(host.history(TASK_ID).starts_with(&failed_history));
                            let pushes = orbit_types::workflow::pr_auto_fix_pushes(
                                &host.history(TASK_ID),
                                PR_NUMBER,
                            );
                            assert_eq!(pushes.last().unwrap().from_head_sha, repaired);
                            assert_eq!(pushes.last().unwrap().to_head_sha, recovered);
                            return;
                        }
                        _ => {
                            let error = result.expect_err(case).to_string();
                            let expected = match case {
                                "budget" => "auto_fix_budget_exhausted",
                                "review_required" => "review_gate_stale",
                                "validation_failed" => "23",
                                "remote_moved" | "remote_deleted" => "push_lease_lost",
                                _ => "delivery_evidence_stale",
                            };
                            assert!(error.contains(expected), "{case}: {error}");
                            if case == "remote_moved" {
                                assert_ne!(
                                    fx.remote_tip(BRANCH),
                                    fx.head(),
                                    "concurrent publication survives"
                                );
                                assert_ne!(fx.remote_tip(BRANCH), repaired);
                            } else if case == "remote_deleted" {
                                assert!(
                                    git(
                                        &fx.repo,
                                        &["ls-remote", "origin", &format!("refs/heads/{BRANCH}")]
                                    )
                                    .is_empty(),
                                    "deleted branch is never recreated"
                                );
                            } else {
                                assert_eq!(
                                    fx.remote_tip(BRANCH),
                                    repaired,
                                    "refusal leaves H1 intact"
                                );
                            }
                        }
                    }
                    assert_eq!(
                        pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).len(),
                        match case {
                            "budget" => 3,
                            "ordinary_stale" | "wrong_base" | "review_required" => 1,
                            _ => 2,
                        },
                        "cumulative admission budget: {case}"
                    );
                    assert_eq!(
                        input["published_head_sha"], fx.candidate,
                        "original provenance remains immutable"
                    );
                    assert!(
                        host.history(TASK_ID).starts_with(&failed_history),
                        "failed/admitted history is retained"
                    );
                    assert!(
                        fx.merge_requests().is_empty(),
                        "repair-only authority never merges"
                    );
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    assert_eq!(fx.remote_tip(BASE), base, "repair does not modify the base");
                });
            }
        },
    );
}

#[test]
fn auto_fix_anchors_emitted_failure_after_printed_commands_and_before_cleanup() {
    isolated(
        "auto_fix::auto_fix_anchors_emitted_failure_after_printed_commands_and_before_cleanup",
        |sandbox| {
            let actual =
                "step.sh: line 8: scripts/ci-download-verified.sh: No such file or directory";
            let signature =
                "step.sh: line <n>: scripts/ci-download-verified.sh: no such file or directory";
            let display = "##[group]Run preflight\n  emit \"::error title=insufficient runner disk::need more space\"\n  error[E0123]: printed compiler text\n  lost communication with the server\n##[endgroup]\n";
            for case in [
                "raw",
                "raw_timestamp_tabs",
                "raw_timestamp_tabbed_error",
                "raw_timestamp_tabbed_compiler",
                "columns",
                "columns_tabbed_compiler",
                "successful_step",
                "annotation",
                "compiler",
                "panic",
                "test",
                "nextest",
                "unclosed",
                "orphan",
                "truncated",
                "incomplete",
                "selected",
                "wrong_step",
            ] {
                in_fresh_row(sandbox, |row| {
                    let fx = Fixture::new(row);
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    action(
                        &host,
                        "pr_open",
                        &fx.open_input(&fx.candidate, &fx.base_sha),
                    )
                    .unwrap();
                    host.script(
                        "ci.commit_jobs",
                        json!({"jobs":[{
                            "name":"test", "workflow":"CI", "conclusion":"failure",
                            "run_id":10, "job_id":11, "failed_step":"Install cargo-nextest"
                        }]}),
                    );
                    let (diagnostic, expected) = match case {
                        "annotation" => (
                            "##[error]insufficient runner disk",
                            "##[error]insufficient runner disk",
                        ),
                        "compiler" => (
                            "error[E0433]: cannot find module `real`",
                            "error[e0433]: cannot find module `real`",
                        ),
                        "raw_timestamp_tabbed_error" => {
                            ("error:\tactual\tinvariant", "error: actual invariant")
                        }
                        "raw_timestamp_tabbed_compiler" | "columns_tabbed_compiler" => (
                            "error[E0433]:\tcannot find module\t`real`",
                            "error[e0433]: cannot find module `real`",
                        ),
                        "panic" => (
                            "thread 'real_test' panicked at src/lib.rs:8:1:",
                            "thread 'real_test' panicked at src/lib.rs:<n>:<n>:",
                        ),
                        "test" => ("test real_case ... FAILED", "test real_case ... failed"),
                        "nextest" => (
                            "FAIL [ 0.3s] suite real_case",
                            "fail [ <n>.<n>s] suite real_case",
                        ),
                        _ => (actual, signature),
                    };
                    let mut log = format!(
                        "{display}preflight passed\n{display}{diagnostic}\n##[error]Process completed with exit code 127.\n{}",
                        "post-job cleanup completed\n".repeat(1000)
                    );
                    let raw_timestamp = case.starts_with("raw_timestamp_");
                    if raw_timestamp {
                        let timestamp = "2026-10-15T02:42:56Z";
                        log = format!(
                            "{timestamp} ##[group]Run preflight\n{timestamp}   printf 'key\tvalue\tdetail\\n'\n{timestamp}   key\tvalue\t##[endgroup]\n{timestamp}   error[E0123]: printed compiler text\n{timestamp} ##[endgroup]\n{timestamp} {diagnostic}\n{timestamp} ##[error]Process completed with exit code 127."
                        );
                    }
                    if matches!(
                        case,
                        "columns" | "columns_tabbed_compiler" | "successful_step" | "wrong_step"
                    ) {
                        let step = if case == "wrong_step" {
                            "successful setup"
                        } else {
                            "Install cargo-nextest"
                        };
                        log = log
                            .lines()
                            .enumerate()
                            .map(|(index, line)| {
                                let step = if case == "successful_step"
                                    && index < display.lines().count() + 1
                                {
                                    "successful setup"
                                } else {
                                    step
                                };
                                format!(
                                    "test\t{step}\t2026-10-15T02:42:56Z \x1b[31m{line}\x1b[0m\n"
                                )
                            })
                            .collect();
                    } else if case == "unclosed" {
                        log = "##[group]Run verify\n  ##[error]printed failure\n".into();
                    } else if case == "orphan" {
                        log = "  ##[error]printed failure\n##[endgroup]\n##[error]Process completed with exit code 127.\n".into();
                    }
                    let source = if raw_timestamp {
                        orbit_tools::github_cli::SOURCE_JOB_API_LOG
                    } else {
                        orbit_tools::github_cli::SOURCE_RUN_LOG
                    };
                    let mut response = json!({"log":log, "truncated":false,
                        "source_complete":true, "source":source});
                    if matches!(case, "truncated" | "selected" | "wrong_step") {
                        response["truncated"] = json!(true);
                    }
                    if case == "selected" {
                        response["log"] = json!("cleanup only");
                        response["diagnostic"] = json!(format!(
                            "{display}{actual}\n##[error]Process completed with exit code 127."
                        ));
                    } else if case == "wrong_step" || raw_timestamp {
                        response["diagnostic"] = response["log"].clone();
                    } else if case == "incomplete" {
                        response["source_complete"] = json!(false);
                    }
                    host.script("ci.job_log", response);
                    fx.script_checks(&["failure"]);
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    let error = action(&host, "pr_complete", &input)
                        .unwrap_err()
                        .to_string();
                    let actionable = !matches!(
                        case,
                        "unclosed" | "orphan" | "truncated" | "incomplete" | "wrong_step"
                    );
                    assert_eq!(
                        orbit_types::workflow::is_pr_auto_fix_check_repair(&error),
                        actionable,
                        "{case}: {error}"
                    );
                    if actionable {
                        assert!(
                            error.contains(&format!("with signature `{expected}`")),
                            "{case}: {error}"
                        );
                        assert!(
                            error.contains(diagnostic),
                            "{case}: repair evidence must retain the emitted failure: {error}"
                        );
                    } else {
                        assert!(
                            error.contains("no emitted diagnostic")
                                || error.contains("could not be read"),
                            "{case}: {error}"
                        );
                        assert!(pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty());
                    }
                    assert!(error.len() < 12000, "repair evidence remains bounded");
                    assert!(fx.merge_requests().is_empty());
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                });
            }
        },
    );
}

/// Oversized evidence must reach public repair admission without granting
/// authority to partial, ambiguous or differently attributed regions.
#[test]
fn auto_fix_preserves_complete_oversized_failure_regions() {
    isolated(
        "auto_fix::auto_fix_preserves_complete_oversized_failure_regions",
        |sandbox| {
            for case in [
                "columns",
                "unattributed",
                "raw",
                "runner_regions",
                "source_incomplete",
                "source_missing",
                "command_incomplete",
                "selection_incomplete",
                "selection_missing",
                "source_unknown",
                "provenance_missing",
                "step_missing",
                "multiple_steps",
                "job_conflict",
                "step_conflict",
                "mixed_labels",
                "omitted_conflict",
                "ambiguous_raw",
                "tail_only",
            ] {
                writeln!(std::io::stderr(), "oversized-failure-region-case: {case}").unwrap();
                in_fresh_row(sandbox, |row| {
                    let fx = Fixture::new(row);
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    open_for_completion(&fx, &host);
                    let label = if case == "unattributed" {
                        "UNKNOWN STEP"
                    } else {
                        "Tests"
                    };
                    let columns = format!("test\t{label}\t");
                    let prefix = if matches!(case, "raw" | "ambiguous_raw") {
                        ""
                    } else {
                        &columns
                    };
                    let padding =
                        format!("{prefix}PASS [ 0.001s] suite passing_test\n").repeat(6000);
                    let failure = if case == "runner_regions" {
                        "error[E0433]: cannot find module `real_failure`"
                    } else {
                        "FAIL [ 0.123s] suite real_failure"
                    };
                    let mut raw = format!(
                        "{prefix}##[group]Run cargo nextest run\n{prefix}##[endgroup]\n{padding}{prefix}{failure}\n{prefix}thread 'real_failure' panicked at tests/sample.rs:12:3:\n{prefix}assertion failed: operation completed\n{padding}{prefix}##[error]Process completed with exit code 100.\n"
                    );
                    if case == "omitted_conflict" {
                        raw = raw.replacen("test\tTests\tPASS", "test\tOther\tPASS", 1);
                    } else if case == "ambiguous_raw" {
                        raw.insert_str(0, "##[group]Run earlier\n##[endgroup]\n##[error]Process completed with exit code 1.\n");
                    }
                    assert!(
                        raw.len() > 262_144,
                        "exercise the full-command retention cap"
                    );
                    let mut collector =
                        orbit_tools::github_cli::StreamedLogCollector::new(65_536, 40);
                    collector.push(raw.as_bytes());
                    let log = collector.finish();
                    assert!(log.source_complete && log.truncated && log.diagnostic.is_none());
                    let regions = log.failure_regions.unwrap();
                    assert!(regions["text"].as_str().unwrap().len() <= 65_536);
                    assert_eq!(
                        regions["command_complete"],
                        !matches!(case, "omitted_conflict" | "ambiguous_raw")
                    );
                    let source = if prefix.is_empty() {
                        "job_api_log"
                    } else {
                        "run_log"
                    };
                    let mut response = json!({"log":log.text, "truncated":true,
                        "source_complete":true, "source":source, "failure_regions":regions});
                    let mut job = json!({"name":"test", "workflow":"CI", "conclusion":"failure",
                        "run_id":10, "job_id":11, "failed_step":"Tests", "failed_steps":["Tests"]});
                    match case {
                        "source_incomplete" => response["source_complete"] = json!(false),
                        "source_missing" => {
                            response.as_object_mut().unwrap().remove("source_complete");
                        }
                        "command_incomplete" => {
                            response["failure_regions"]["command_complete"] = json!(false)
                        }
                        "selection_incomplete" => {
                            response["failure_regions"]["selection_complete"] = json!(false)
                        }
                        "selection_missing" => {
                            response["failure_regions"]
                                .as_object_mut()
                                .unwrap()
                                .remove("selection_complete");
                        }
                        "source_unknown" => response["source"] = json!("unknown"),
                        "provenance_missing" => {
                            response.as_object_mut().unwrap().remove("source");
                        }
                        "step_missing" => {
                            job.as_object_mut().unwrap().remove("failed_steps");
                        }
                        "multiple_steps" => job["failed_steps"] = json!(["Tests", "Other"]),
                        "job_conflict" => {
                            let text = response["failure_regions"]["text"].as_str().unwrap();
                            response["failure_regions"]["text"] =
                                json!(text.replace("test\t", "other\t"));
                        }
                        "step_conflict" | "mixed_labels" => {
                            let text = response["failure_regions"]["text"].as_str().unwrap();
                            response["failure_regions"]["text"] = json!(text.replacen(
                                "test\tTests\t",
                                if case == "step_conflict" {
                                    "test\tOther\t"
                                } else {
                                    "test\tUNKNOWN STEP\t"
                                },
                                1
                            ));
                        }
                        "tail_only" => {
                            response.as_object_mut().unwrap().remove("failure_regions");
                        }
                        _ => {}
                    }
                    // Exercise the real private adapter and streaming read as
                    // part of public completion, not just a scripted projection.
                    if matches!(
                        case,
                        "columns" | "unattributed" | "runner_regions" | "multiple_steps"
                    ) {
                        fs::write(fx.forge.join("failed-command-log"), raw).unwrap();
                        let mut steps = vec![json!({"name":"Tests", "conclusion":"failure"})];
                        if case == "multiple_steps" {
                            steps.push(json!({"name":"Other", "conclusion":"failure"}));
                        }
                        fs::write(
                            fx.forge.join("failed-command-jobs"),
                            serde_json::to_vec(&json!({"jobs":[{
                                "id":11, "name":"test", "conclusion":"failure", "steps":steps
                            }]}))
                            .unwrap(),
                        )
                        .unwrap();
                    } else {
                        host.script("ci.job_log", response);
                        host.script("ci.commit_jobs", json!({"jobs":[job]}));
                    }
                    fx.script_checks(&["failure"]);
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    let error = action(&host, "pr_complete", &input)
                        .unwrap_err()
                        .to_string();
                    let actionable =
                        matches!(case, "columns" | "unattributed" | "raw" | "runner_regions");
                    assert_eq!(
                        orbit_types::workflow::is_pr_auto_fix_check_repair(&error),
                        actionable,
                        "{case}: {error}"
                    );
                    assert_eq!(
                        pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).len(),
                        usize::from(actionable),
                        "{case}: invalid evidence spends no repair authority"
                    );
                    if actionable {
                        let signature = if case == "runner_regions" {
                            "error[e0433]: cannot find module `real_failure`"
                        } else {
                            "fail [ <n>.<n>s] suite real_failure"
                        };
                        assert!(
                            error.contains(&format!("with signature `{signature}`")),
                            "{case}: stable failure identity: {error}"
                        );
                        assert!(
                            error.contains(failure),
                            "{case}: preserve the middle failure: {error}"
                        );
                        assert!(
                            error.contains("assertion failed: operation completed"),
                            "{case}: preserve captured output: {error}"
                        );
                    } else {
                        assert!(error.contains("could not be read"), "{case}: {error}");
                    }
                    assert!(error.len() < 12000, "repair evidence stays bounded");
                    assert!(fx.merge_requests().is_empty());
                    assert!(!fx.forge_calls().iter().any(|call| call == "run rerun"));
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    assert_eq!(fx.head(), fx.candidate);
                    assert_eq!(fx.remote_tip(BASE), fx.base_sha);
                });
                test_env::record_scenario_pass(&format!("oversized_failure_regions/{case}"));
            }
        },
    );
}

/// An unrelated optional failure must not end the authenticated required wait.
#[test]
fn auto_fix_watches_required_running_beside_optional_failure() {
    isolated(
        "auto_fix::auto_fix_watches_required_running_beside_optional_failure",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
            open_for_completion(&fx, &host);
            host.script("ci.prerequisite_evidence", json!({"evidence":null}));
            fx.script_checks(&["mixed_pending", "mixed_pending", "mixed_success"]);
            let mut input = fx.complete_ungated_input();
            input["auto_fix"] = json!(true);
            input["completion"] = json!("review");
            input["max_wait_seconds"] = json!(15);
            let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_u64));
            let reads = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let now = clock.clone();
            let elapsed = clock.clone();
            let captured = reads.clone();
            let completed = orbit_engine::test_support::with_prerequisite_clock(
                move || now.get(),
                move |label, timeout| captured.borrow_mut().push((label.to_string(), timeout)),
                || {
                    orbit_engine::test_support::with_vcs_retry_wait(
                        move |duration| elapsed.set(elapsed.get() + duration.as_millis() as u64),
                        || action(&host, "pr_complete", &input),
                    )
                },
            )
            .expect("optional failure must not stop the current required running watcher");
            assert_eq!(
                *reads.borrow(),
                [
                    ("PR status".into(), 15_000),
                    ("PR required checks".into(), 15_000),
                    ("PR status".into(), 10_000),
                    ("PR required checks".into(), 10_000),
                    ("PR status".into(), 5_000)
                ],
                "all observations consume the original absolute completion deadline"
            );
            assert_eq!(completed["merge"]["reason"], "auto_fix_mergeable");
            assert_eq!(completed["merge"]["head_sha"], fx.candidate);
            assert_eq!(completed["merge"]["waited_seconds"], 10);
            assert_eq!(fx.status_reads(), 3, "watch until required success");
            assert!(pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty());
            assert!(
                fx.merge_requests().is_empty(),
                "watch authority never merges"
            );
            assert_eq!(fx.head(), fx.candidate, "no source repair");
            assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
            assert_eq!(fx.remote_tip(BASE), fx.base_sha);
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert!(!fx.forge_calls().iter().any(|call| call == "run rerun"));
        },
    );
}

#[test]
fn auto_fix_mixed_failure_wait_preserves_recovery_and_refusals() {
    isolated(
        "auto_fix::auto_fix_mixed_failure_wait_preserves_recovery_and_refusals",
        |sandbox| {
            for case in [
                "required_failure",
                "optional_pending",
                "administrative",
                "review",
                "foreign_head",
                "foreign_commit",
                "missing_head",
                "unknown_required",
                "unclassified",
                "truncated",
                "malformed",
                "auth",
                "timeout",
                "changed_head",
                "changed_base",
            ] {
                in_fresh_row(sandbox, |row| {
                    let fx = Fixture::new(row);
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    open_for_completion(&fx, &host);
                    host.script("ci.prerequisite_evidence", json!({"evidence":null}));
                    fx.script_checks(&["mixed_pending", "mixed_success"]);
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    let mut policy = json!({"headRefOid":fx.candidate,"commits":{"nodes":[{"commit":{
                        "oid":fx.candidate,"statusCheckRollup":{"contexts":{
                            "pageInfo":{"hasNextPage":false},"nodes":[
                                {"name":"test","isRequired":true,"status":"IN_PROGRESS","conclusion":""},
                                {"name":"coverage","isRequired":false,"status":"COMPLETED","conclusion":"FAILURE"}
                            ]}
                        }
                    }}]}});
                    let mut expected = match case {
                        "optional_pending" | "administrative" => "only optional checks failed",
                        "review" => "review is required (REVIEW_REQUIRED)",
                        "foreign_head" | "foreign_commit" | "missing_head" => "current head",
                        "unknown_required" => "unreadable",
                        "unclassified" => "did not say whether",
                        "truncated" => "truncated",
                        "malformed" => "no pull request",
                        "auth" => "HTTP 401",
                        _ => "auto_fix_stopped",
                    };
                    match case {
                        "required_failure" => {
                            fx.script_checks(&["mixed_failure"]);
                            let nodes = &mut policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]
                                ["contexts"]["nodes"];
                            nodes[0]["status"] = json!("COMPLETED");
                            nodes[0]["conclusion"] = json!("FAILURE");
                            nodes.as_array_mut().unwrap().push(json!({"name":"other_required","isRequired":true,"status":"IN_PROGRESS"}));
                            host.script("ci.commit_jobs", json!({"jobs":[{"name":"test","conclusion":"failure","run_id":10,"job_id":11,"failed_step":"compile"}]}));
                            host.script(
                                "ci.job_log",
                                json!({"log":"error[E0433]: cannot find module `broken`"}),
                            );
                        }
                        "optional_pending" => {
                            policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]
                                ["nodes"][0]["isRequired"] = json!(false)
                        }
                        "administrative" => {
                            let check = &mut policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]
                                ["contexts"]["nodes"][0];
                            check["status"] = json!("COMPLETED");
                            check["conclusion"] = json!("SUCCESS");
                        }
                        "review" => {
                            fs::write(fx.forge.join("review-decision"), "REVIEW_REQUIRED").unwrap()
                        }
                        "foreign_head" => policy["headRefOid"] = json!("foreign"),
                        "foreign_commit" => {
                            policy["commits"]["nodes"][0]["commit"]["oid"] = json!("foreign")
                        }
                        "missing_head" => {
                            policy.as_object_mut().unwrap().remove("headRefOid");
                        }
                        "unknown_required" => {
                            policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]
                                ["nodes"][0]["status"] = json!("UNKNOWN_STATE")
                        }
                        "unclassified" => {
                            policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"][0].as_object_mut().unwrap().remove("isRequired");
                        }
                        "truncated" => {
                            policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]
                                ["pageInfo"]["hasNextPage"] = json!(true)
                        }
                        "malformed" => policy = Value::Null,
                        "auth" => fs::write(fx.forge.join("required-auth"), "").unwrap(),
                        "timeout" => {
                            fx.script_checks(&["mixed_pending"]);
                            input["max_wait_seconds"] = json!(10);
                            expected = "timed out after 10s";
                        }
                        "changed_head" => {
                            fs::write(fx.forge.join("pr-head"), "other-branch").unwrap();
                            expected = "delivery_evidence_stale";
                        }
                        "changed_base" => {
                            fs::write(fx.forge.join("pr-base"), "main").unwrap();
                            expected = "delivery_evidence_stale";
                        }
                        _ => unreachable!(),
                    }
                    fs::write(
                        fx.forge.join("required-policy"),
                        serde_json::to_vec(&json!({"data":{"repository":{"pullRequest":policy}}}))
                            .unwrap(),
                    )
                    .unwrap();
                    let error = action(&host, "pr_complete", &input)
                        .expect_err(case)
                        .to_string();
                    if case == "required_failure" {
                        assert!(
                            orbit_types::workflow::is_pr_auto_fix_check_repair(&error),
                            "eligible required failure: {error}"
                        );
                    } else {
                        assert!(error.contains(expected), "{case}: {error}");
                    }
                    assert_eq!(
                        pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).len(),
                        usize::from(case == "required_failure"),
                        "{case}: only required failure admits recovery"
                    );
                    assert_eq!(
                        fx.status_reads(),
                        if case == "timeout" { 2 } else { 1 },
                        "{case}: no broad pending-first wait"
                    );
                    assert!(fx.merge_requests().is_empty(), "{case}: no merge authority");
                    assert!(
                        !fx.forge_calls().iter().any(|call| call == "run rerun"),
                        "{case}: no infrastructure rerun"
                    );
                    assert_eq!(fx.head(), fx.candidate);
                    assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
                    assert_eq!(fx.remote_tip(BASE), fx.base_sha);
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                });
            }
        },
    );
}
