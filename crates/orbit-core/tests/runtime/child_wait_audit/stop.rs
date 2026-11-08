//! Public child-stop refusal and settlement regressions.

use super::*;

/// Both public child-stop routes must refuse an unverifiable live worker.
#[test]
fn child_stop_refusal_preserves_owner_candidate_reservation_and_lineage() {
    if !isolated(
        "child_wait_audit::stop::child_stop_refusal_preserves_owner_candidate_reservation_and_lineage",
    ) {
        return;
    }
    use orbit_store::contracts::TaskReservationReserveParams;
    use orbit_types::workflow::JobRunState;

    for (cascade, identity) in [
        (false, "unverified"),
        (true, "unverified"),
        (false, "self"),
        (true, "self"),
        (false, "exited"),
        (true, "exited"),
        (false, "reused"),
        (true, "reused"),
    ] {
        let refused = matches!(identity, "unverified" | "self");
        let fixture = Fixture::new();
        let candidate = fixture.root.path().join("candidate");
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::write(
            candidate.join("implementation.txt"),
            "uncommitted implementation\n",
        )
        .unwrap();
        let (child, row, output) =
            fixture.observe_configured(json!([]), 1, Some("cancel_child"), |runtime, child| {
                let pid = *fixture.worker_pids().last().unwrap();
                fixture
                    .jobs
                    .mark_job_run_running(child, Utc::now(), pid)
                    .unwrap();
                // The fixture owns the kernel handle, but the durable owner
                // record cannot authorize a stop (an old/absent identity).
                let mut run = fixture.jobs.get_job_run(child).unwrap().unwrap();
                run.pid_start_time = None;
                match identity {
                    "self" => run.pid = Some(std::process::id()),
                    "exited" => {
                        let mut exited = std::process::Command::new("true").spawn().unwrap();
                        let pid = exited.id();
                        assert!(exited.wait().unwrap().success());
                        run.pid = Some(pid);
                    }
                    "reused" => {
                        run.pid_start_time = Some(format!(
                            "{}not-the-original-worker",
                            orbit_common::process::identity::STABLE_TOKEN_PREFIX,
                        ))
                    }
                    _ => {}
                }
                runtime
                    .sqlite_store()
                    .unwrap()
                    .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &run, None)
                    .unwrap();
                fixture
                    .jobs
                    .update_run_state(child, &mut |_, state| {
                        state.record_pipeline_output(
                            "worktree",
                            json!({"workspace_path": candidate}),
                        );
                        Ok(())
                    })
                    .unwrap();
                let reserved = runtime
                    .stores()
                    .task_reservations()
                    .reserve_task_reservation(TaskReservationReserveParams {
                        workspace_orbit_dir: runtime
                            .paths()
                            .orbit_dir
                            .to_string_lossy()
                            .into_owned(),
                        workspace_id: Some(runtime.workspace_id().unwrap()),
                        task_ids: vec![],
                        requested_files: vec!["file:implementation.txt".into()],
                        stored_files: vec!["file:implementation.txt".into()],
                        actor: "fixture".into(),
                        ttl_seconds: 600,
                        owner_run_id: Some(child.into()),
                        owner_metadata: None,
                    })
                    .unwrap();
                assert!(reserved.reserved);
                if cascade {
                    // invoke_and_wait already durably linked the blocking
                    // child. Legacy cancellation of this pending parent is
                    // intentional and must keep working.
                    runtime.cancel_job_run(&fixture.parent).unwrap();
                    let state = runtime.read_run_state(&fixture.parent).unwrap().unwrap();
                    let dispatch = state
                        .child_dispatches
                        .iter()
                        .find(|d| d.child_run_id == child)
                        .unwrap();
                    let cancellation = dispatch.cancellation.as_ref().unwrap();
                    assert_eq!(
                        cancellation.outcome,
                        if refused { "failed" } else { "cancelled" }
                    );
                    if refused {
                        assert!(
                            cancellation
                                .error
                                .as_deref()
                                .unwrap()
                                .contains("could not confirm")
                        );
                    } else {
                        assert!(cancellation.error.is_none());
                    }
                }
            });
        assert_eq!(
            output["status"],
            if cascade && !refused {
                "cancelled"
            } else {
                "timeout"
            },
            "{identity}: {output}"
        );
        assert!(output.get("candidate").is_none(), "{output}");
        if refused {
            assert_eq!(output["child_live"], true, "{output}");
            assert!(output.get("child_cancelled").is_none(), "{output}");
            assert!(
                output["error"]
                    .as_str()
                    .unwrap()
                    .contains("could not confirm"),
                "{output}"
            );
        } else {
            assert!(output.get("child_live").is_none(), "{output}");
            if !cascade {
                assert_eq!(output["child_cancelled"], true, "{output}");
            }
        }
        assert_eq!(
            row.status,
            if cascade && !refused {
                AuditEventStatus::Success
            } else {
                AuditEventStatus::Failure
            }
        );
        let run = fixture.jobs.get_job_run(&child).unwrap().unwrap();
        assert_eq!(
            run.state,
            if refused {
                JobRunState::Running
            } else {
                JobRunState::Cancelled
            }
        );
        assert!(
            orbit_common::process::identity::process_is_alive(
                *fixture.worker_pids().last().unwrap()
            ),
            "the substitute/bystander must never be signalled for {identity}"
        );
        let completions = fixture
            .runtime
            .list_audit_events(None, None, None, None, 1000)
            .unwrap();
        assert!(
            completions.iter().any(|event| {
                event.tool_name.as_deref() == Some("pipeline.run.cancel.completed")
                    && payload(event)["run_id"] == child
                    && payload(event)["outcome"] == if refused { "failed" } else { "cancelled" }
            }),
            "cancellation evidence must reflect {identity}"
        );
        let state = fixture.runtime.read_run_state(&child).unwrap().unwrap();
        assert!(state.failure_activity_checkpoint.is_none());
        assert_eq!(
            state.pipeline["worktree"]["workspace_path"],
            json!(candidate)
        );
        assert_eq!(
            std::fs::read_to_string(candidate.join("implementation.txt")).unwrap(),
            "uncommitted implementation\n"
        );
        let reservations = fixture
            .runtime
            .stores()
            .task_reservations()
            .list_active_task_reservations(
                &fixture.runtime.paths().orbit_dir.to_string_lossy(),
                Some(&fixture.runtime.workspace_id().unwrap()),
            )
            .unwrap();
        assert_eq!(
            reservations
                .reservations
                .iter()
                .any(|r| r.owner_run_id.as_deref() == Some(&child)),
            refused,
            "reservation release must require a confirmed stop: {identity}",
        );
    }
}
