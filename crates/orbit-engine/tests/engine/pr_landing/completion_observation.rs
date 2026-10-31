//! Unsettled prerequisite observations through the public completion job.
use super::*;
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
                        assert!(error.contains("workflow prerequisite evidence is unavailable (required integration)"), "{error}");
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
