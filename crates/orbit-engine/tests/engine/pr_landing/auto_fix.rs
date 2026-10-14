//! Resumed auto-fix authority through the public job and PR boundaries.

use super::*;

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
