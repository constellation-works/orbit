//! Unsettled prerequisite observations through the public completion job.
use super::*;
use orbit_engine::RunLifecycle;
use orbit_types::workflow::activity_job::V2AuditEventKind;
use std::{cell::Cell, rc::Rc};

#[test]
fn unreadable_prerequisite_retries_until_green_or_the_original_deadline() {
    isolated(
        "completion_observation::unreadable_prerequisite_retries_until_green_or_the_original_deadline",
        |sandbox| {
            for persistent in [false, true] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = absent_context::setup(row, absent_context::NEW);
                    // The first required check is pending. Once it settles, a
                    // malformed prerequisite observation interrupts the next poll.
                    fs::write(fx.forge.join("pending-before-emission"), "pending").unwrap();
                    fs::write(fx.forge.join("unreadable-integration"), "2").unwrap();
                    if persistent {
                        fs::write(fx.forge.join("integration-repeat"), "repeat").unwrap();
                    }
                    let mut input = fx.complete_input();
                    input["auto_fix"] = json!(true);
                    host.script("test_checkpoint", json!({"workspace_path":fx.repo}));
                    host.script(
                        "test_final_decision",
                        json!({"decision":"escalate",
                        "diagnosis":"unexpected recovery", "human_action":"inspect"}),
                    );
                    let mut job = job_asset(json!([
                        {"id":"setup","target":"activity:checkpoint"},
                        {"id":"complete_pr","target":"activity:pr_complete","default_input":input}
                    ]));
                    // Leave recovery disabled for the persistent case so the
                    // original completion error is directly observable.
                    if !persistent {
                        job.final_recovery_activity = Some("decide".into());
                    }
                    let mut catalog = V2ActivityCatalog::new();
                    for (name, action) in [
                        ("checkpoint", "test_checkpoint"),
                        ("pr_complete", "pr_complete"),
                        ("decide", "test_final_decision"),
                    ] {
                        catalog.insert(
                            name,
                            super::auto_fix::pr_landing_fixture_activity(name, action),
                        );
                    }
                    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
                    let (writer, _, _) = build_writer_and_sinks(&row.join("audit"), RUN_ID);
                    let clock = Rc::new(Cell::new(1_000_000_u64));
                    let now = Rc::clone(&clock);
                    let waited = Rc::clone(&clock);
                    let forge = fx.forge.clone();
                    let result = orbit_engine::test_support::with_prerequisite_clock(
                        move || now.get(),
                        |_, _| {},
                        || {
                            orbit_engine::test_support::with_vcs_retry_wait(
                                move |duration| {
                                    waited.set(
                                        waited.get() + u64::try_from(duration.as_millis()).unwrap(),
                                    );
                                    if forge.join("pending-before-emission").exists() {
                                        fs::remove_file(forge.join("pending-before-emission"))
                                            .unwrap();
                                    }
                                },
                                || {
                                    execute_job_with_resume(
                                        &job,
                                        json!({"task_ids":[TASK_ID]}),
                                        RUN_ID,
                                        writer.clone(),
                                        &host,
                                        None,
                                    )
                                },
                            )
                        },
                    );
                    assert!(fx.forge.join("integration-hit").exists());
                    let reads: usize = fx
                        .forge_state("integration-reads")
                        .unwrap()
                        .parse()
                        .unwrap();
                    assert!(
                        reads >= 2,
                        "unreadable evidence must be retried: {result:?}"
                    );
                    if persistent {
                        let error = result.unwrap_err().to_string();
                        assert!(
                            error.contains(
                                "workflow prerequisite evidence is unavailable (invalid API JSON)"
                            ),
                            "{error}"
                        );
                        assert_eq!(
                            clock.get(),
                            1_030_000,
                            "retries spend the original deadline"
                        );
                        assert!(!fx.forge.join("mutations").exists());
                        assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    } else {
                        assert!(result.unwrap().success);
                        assert_eq!(host.status(TASK_ID), TaskStatus::Done);
                        assert_eq!(
                            fs::read_to_string(fx.forge.join("mutations")).unwrap(),
                            "update\nmerge\n"
                        );
                        assert!(
                            !writer
                                .events_snapshot()
                                .unwrap()
                                .iter()
                                .any(|event| matches!(
                                    event.kind,
                                    V2AuditEventKind::FinalRecoveryAttempted { .. }
                                ))
                        );
                    }
                });
            }
        },
    );
}

fn seed_completion_mutation(host: &DeliveryHost, operation: &str, head_sha: &str) {
    host.apply_task_automation_update(
        TASK_ID,
        TaskAutomationUpdate {
            append_history: vec![
                TaskHistoryEntry::new(
                    Utc::now(),
                    "system",
                    orbit_types::workflow::PR_COMPLETION_MUTATION_STARTED_EVENT,
                )
                .with_note(Some(
                    json!({
                        "run_id": RUN_ID,
                        "operation": operation,
                        "pr_number": PR_NUMBER,
                        "head_sha": head_sha,
                    })
                    .to_string(),
                )),
            ],
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn post_rebase_transient_pr_status_and_capabilities_hold() {
    const TLS: &str = "Post \"https://api.github.com/graphql\": net/http: TLS handshake timeout";
    const UNAUTHORIZED: &str = "HTTP 401: Bad credentials (https://api.github.com/graphql)";
    isolated(
        "completion_observation::post_rebase_transient_pr_status_and_capabilities_hold",
        |sandbox| {
            // 1. Prior `operation: "rebase"` + transient PR_STATUS failure -> held.
            in_fresh_row(sandbox, |row| {
                let (fx, host) = fx_host(row);
                open_for_completion(&fx, &host);
                seed_completion_mutation(&host, "rebase", &fx.candidate);
                fx.script_status_failures(10, TLS);
                host.script(
                    "test_final_decision",
                    json!({"decision":"escalate",
                    "diagnosis":"unexpected recovery", "human_action":"inspect"}),
                );
                host.script("test_failure_handoff", json!({"unexpected": true}));
                let mut job = job_asset(json!([
                    {"id":"complete_pr","target":"activity:pr_complete","default_input":fx.complete_input()}
                ]));
                job.failure_activity = Some("failure_handoff".into());
                job.final_recovery_activity = Some("decide".into());
                let mut catalog = V2ActivityCatalog::new();
                for (name, action) in [
                    ("pr_complete", "pr_complete"),
                    ("failure_handoff", "test_failure_handoff"),
                    ("decide", "test_final_decision"),
                ] {
                    catalog.insert(
                        name,
                        super::auto_fix::pr_landing_fixture_activity(name, action),
                    );
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
                let err = result.unwrap_err();
                assert!(
                    matches!(
                        &err,
                        DispatchError::DeliveryRefused {
                            refusal: orbit_types::workflow::DeliveryRefusal::ForgeObservationUnavailable,
                            message,
                            ..
                        } if message.contains("TLS handshake timeout")
                            && message.contains("no merge or auto-merge request was sent")
                    ),
                    "post-rebase PR_STATUS TLS error must hold as ForgeObservationUnavailable: {err}"
                );
                assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                assert_eq!(fx.head(), fx.candidate);
                assert!(fx.merge_requests().is_empty());
                assert!(host.landings().is_empty());
                assert!(host.failure_checkpoints().is_empty());
            });

            // 2. Post-recovery `pr_complete` after `git_rebase` conflict + transient PR_STATUS -> held.
            in_fresh_row(sandbox, |row| {
                let (fx, host) = fx_host(row);
                open_for_completion(&fx, &host);
                fx.commit_on_base(FEATURE, "base change\n");
                fx.script_checks(&["dirty"]);
                fx.script_pr_views(&["success", TLS]);
                host.script("test_conflict_recovery", json!({"resolved": true}));
                host.script(
                    "test_final_decision",
                    json!({"decision":"escalate",
                    "diagnosis":"unexpected recovery", "human_action":"inspect"}),
                );
                let mut input = fx.complete_ungated_input();
                input["completion"] = json!("done");
                let mut job = job_asset(json!([{
                    "id": "complete_pr",
                    "target": "activity:pr_complete",
                    "recovery_activity": "pr_conflict_recovery",
                    "default_input": input,
                }]));
                job.final_recovery_activity = Some("decide".into());
                let mut catalog = V2ActivityCatalog::new();
                for (name, action) in [
                    ("pr_complete", "pr_complete"),
                    ("pr_conflict_recovery", "test_conflict_recovery"),
                    ("decide", "test_final_decision"),
                ] {
                    catalog.insert(
                        name,
                        super::auto_fix::pr_landing_fixture_activity(name, action),
                    );
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
                let err = result.unwrap_err();
                assert!(
                    host.history(TASK_ID).iter().any(|entry| {
                        entry.event == orbit_types::workflow::PR_COMPLETION_MUTATION_STARTED_EVENT
                            && entry
                                .note
                                .as_deref()
                                .is_some_and(|note| note.contains("\"rebase\""))
                    }),
                    "rebase mutation provenance must be recorded"
                );
                assert!(
                    matches!(
                        &err,
                        DispatchError::DeliveryRefused {
                            refusal: orbit_types::workflow::DeliveryRefusal::ForgeObservationUnavailable,
                            message,
                            ..
                        } if message.contains("TLS handshake timeout")
                            && message.contains("original error before recovery:")
                    ),
                    "post-recovery PR_STATUS TLS error after rebase conflict must hold: {err}"
                );
                assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                assert!(fx.merge_requests().is_empty());
                assert!(host.landings().is_empty());
                assert!(host.failure_checkpoints().is_empty());
            });

            // 3. Prior `operation: "rebase"` + transient `resolved_capabilities` failure -> held.
            in_fresh_row(sandbox, |row| {
                let (fx, host) = absent_context::setup(row, absent_context::NEW);
                fs::write(fx.forge.join("base-applied"), "current").unwrap();
                fs::write(fx.forge.join("fault-match"), "autoMergeAllowed").unwrap();
                fs::write(fx.forge.join("fault-stderr"), TLS).unwrap();
                fs::write(fx.forge.join("fault-repeat"), "repeat").unwrap();
                seed_completion_mutation(&host, "rebase", &fx.candidate);
                let mut job = job_asset(json!([
                    {"id":"complete_pr","target":"activity:pr_complete","default_input":fx.complete_input()}
                ]));
                job.final_recovery_activity = Some("decide".into());
                let mut catalog = V2ActivityCatalog::new();
                for (name, action) in [
                    ("pr_complete", "pr_complete"),
                    ("decide", "test_final_decision"),
                ] {
                    catalog.insert(
                        name,
                        super::auto_fix::pr_landing_fixture_activity(name, action),
                    );
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
                let err = result.unwrap_err();
                assert!(
                    matches!(
                        &err,
                        DispatchError::DeliveryRefused {
                            refusal: orbit_types::workflow::DeliveryRefusal::ForgeObservationUnavailable,
                            message,
                            ..
                        } if message.contains("TLS handshake timeout")
                    ),
                    "post-rebase resolved_capabilities TLS error must hold: {err}"
                );
                assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                assert_eq!(fx.head(), fx.candidate);
                assert!(!fx.forge.join("mutations").exists());
                assert!(host.landings().is_empty());
            });

            // 4. In-invocation conflict refresh (rebase + push) followed by transient PR_STATUS -> held.
            in_fresh_row(sandbox, |row| {
                let (fx, host) = fx_host(row);
                open_for_completion(&fx, &host);
                fx.advance_base();
                fx.script_checks(&["dirty", "success"]);
                fx.script_pr_views(&["success", TLS]);
                let mut input = fx.complete_ungated_input();
                input["completion"] = json!("done");
                let err = complete_with_logical_clock(&host, &input).unwrap_err();
                assert!(
                    matches!(
                        &err,
                        OrbitError::DeliveryRefused {
                            refusal: orbit_types::workflow::DeliveryRefusal::ForgeObservationUnavailable,
                            message,
                        } if message.contains("TLS handshake timeout")
                    ),
                    "in-invocation conflict refresh followed by TLS error must hold: {err}"
                );
                assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                assert!(fx.merge_requests().is_empty());
                assert!(host.landings().is_empty());
            });

            // 5. Genuine merge mutation (`operation: "merge"`), ambiguous merge reconciliation,
            //    and non-transient errors remain unchanged.
            in_fresh_row(sandbox, |row| {
                let (fx, host) = fx_host(row);
                open_for_completion(&fx, &host);
                seed_completion_mutation(&host, "merge", &fx.candidate);
                fx.script_status_failures(10, TLS);
                let err = complete_with_logical_clock(&host, &fx.complete_input()).unwrap_err();
                assert!(
                    !matches!(err, OrbitError::DeliveryRefused { .. }),
                    "genuine merge mutation must not become DeliveryRefused: {err}"
                );
                assert!(err.to_string().contains("TLS handshake timeout"), "{err}");
            });
            in_fresh_row(sandbox, |row| {
                let (fx, host) = fx_host(row);
                open_for_completion(&fx, &host);
                seed_completion_mutation(&host, "rebase", &fx.candidate);
                fx.script_status_failures(10, UNAUTHORIZED);
                let err = complete_with_logical_clock(&host, &fx.complete_input()).unwrap_err();
                assert!(
                    !matches!(err, OrbitError::DeliveryRefused { .. }),
                    "non-transient error after rebase must stay a failure: {err}"
                );
                assert!(err.to_string().contains("Bad credentials"), "{err}");
            });
            in_fresh_row(sandbox, |row| {
                let (fx, host) = fx_host(row);
                open_for_completion(&fx, &host);
                seed_completion_mutation(&host, "rebase", &fx.candidate);
                fx.script_merges(&["server_error+merged"]);
                let completed = complete_with_logical_clock(&host, &fx.complete_input()).unwrap();
                assert_eq!(completed["merge"]["merged"], true);
                assert_eq!(host.status(TASK_ID), TaskStatus::Done);
            });
        },
    );
}
