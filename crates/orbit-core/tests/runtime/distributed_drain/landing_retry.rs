//! Public operator retry of the existing authorized landing.

use super::*;

/// Reproduces the missing public same-handoff retry seam. The old stopped
/// attempt survives, the normal landing action completes, and replay keeps
/// naming that one run after completion without changing the leaf binding.
fn a_stopped_authorized_handoff_retries_once_through_the_public_operator_tool() {
    let evidence = json!({
        "schema_version":1, "kind":"owner_landing_refusal",
        "reason":{"value":"required provider review was not yet satisfied"},
        "observation":{"required_checks":{"state":"not_collected"}},
    });
    let (owner, task, request, stopped) =
        retry_fixture_with_evidence(true, Some(JobRunState::Cancelled), &evidence.to_string());
    let runtime = &owner.pair.wire.owner;
    let before = owner.pair.owner_task(&task);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let call = || {
        let reserved = barrier.clone();
        let open = barrier.clone();
        orbit_core::test_support::with_landing_retry_reservation_hook(
            move || {
                reserved.wait();
            },
            || {
                #[cfg(unix)]
                {
                    orbit_core::test_support::with_pipeline_worker_log_open_hook(
                        move |path| {
                            let directory = std::fs::metadata(path.parent().unwrap()).unwrap();
                            use std::os::unix::fs::MetadataExt;
                            let _ = writeln!(
                                std::io::stderr(),
                                "successor create-open rendezvous: path={} directory=({}, {}, {})",
                                path.display(),
                                directory.dev(),
                                directory.ino(),
                                directory.nlink()
                            );
                            open.wait();
                        },
                        || retry_tool(runtime, &request, McpCapability::Operator),
                    )
                }
                #[cfg(not(unix))]
                {
                    let _ = open;
                    retry_tool(runtime, &request, McpCapability::Operator)
                }
            },
        )
    };
    let responses = std::thread::scope(|scope| {
        let a = scope.spawn(call);
        let b = scope.spawn(call);
        [a.join().unwrap().unwrap(), b.join().unwrap().unwrap()]
    });
    // Both submissions really launch pending workers in this fixture. Own all
    // of them, including duplicate delivery, before landing or dropping roots.
    let successor = responses[0]["run_id"].as_str().unwrap();
    let launched = responses.iter().filter(|r| r["submitted"] == true).count();
    owner.workers.own_count(successor, launched).unwrap();
    let response = &responses[0];
    assert_eq!(
        response["run_id"], responses[1]["run_id"],
        "concurrent same-key calls resolve one run"
    );
    assert_eq!(response["attempt"], stopped.attempt + 1);
    let replay = retry_tool(runtime, &request, McpCapability::Operator).unwrap();
    assert_eq!(response["run_id"], replay["run_id"]);
    assert_eq!(replay["submitted"], false);
    assert_eq!(
        runtime.landing_attempts().unwrap()[0].attempt,
        stopped.attempt + 1
    );
    let historical = runtime
        .landing_attempt_record(&request.handoff_id, stopped.attempt)
        .unwrap()
        .unwrap();
    assert_eq!(
        historical, stopped,
        "retry retains the original stop and run"
    );
    let landed = owner.land(&task).unwrap();
    assert_eq!(landed["phase"], "landed");
    assert_eq!(owner.pair.owner_status(&task), "done");
    assert_eq!(
        runtime
            .landing_attempt_record(&request.handoff_id, stopped.attempt)
            .unwrap(),
        Some(stopped),
        "completed same-handoff retry preserves the first failed-attempt diagnostic"
    );
    let completed_replay = retry_tool(runtime, &request, McpCapability::Operator).unwrap();
    assert_eq!(completed_replay["run_id"], response["run_id"]);
    let after = owner.pair.owner_task(&task);
    assert_eq!(after["job_run_id"], before["job_run_id"]);
    assert_eq!(after["job_run_machine"], before["job_run_machine"]);
    assert_eq!(
        git(&owner.pair.owner_repo, &["rev-parse", "main"]).trim(),
        request.expected_candidate_commit
    );
}

/// A real cancelled owner run leaves its attempt dispatched. Recovery must
/// prove that exact terminal run, preserve it, and reserve one normal retry.
fn terminal_owner_jobs_reconcile_dispatched_attempts_through_the_operator_tool() {
    for terminal in [JobRunState::Cancelled, JobRunState::Failed] {
        let (owner, task, request, prior) = retry_fixture(false, Some(terminal));
        let runtime = &owner.pair.wire.owner;
        let jobs = &owner.pair.follower_jobs;
        let original_run = prior.job_run_id.as_deref().unwrap();
        let before = owner.pair.owner_task(&task);
        let accepted = owner.accepted(&prior.claim_id);
        let original = jobs.get_job_run(original_run).unwrap().unwrap();
        assert_eq!(original.state, terminal);
        let responses = std::thread::scope(|scope| {
            let a = scope.spawn(|| retry_tool(runtime, &request, McpCapability::Operator));
            let b = scope.spawn(|| retry_tool(runtime, &request, McpCapability::Operator));
            [a.join().unwrap().unwrap(), b.join().unwrap().unwrap()]
        });
        assert_eq!(responses[0]["run_id"], responses[1]["run_id"]);
        let successor = responses[0]["run_id"].as_str().unwrap();
        let launched = responses.iter().filter(|r| r["submitted"] == true).count();
        owner.workers.own_count(successor, launched).unwrap();
        assert_eq!(responses[0]["attempt"], prior.attempt + 1);
        // The first response could have been lost: identical replay names the
        // same run; a changed payload cannot reuse the durable request key.
        assert_eq!(
            retry_tool(runtime, &request, McpCapability::Operator).unwrap()["run_id"],
            responses[0]["run_id"]
        );
        let mut changed = request.clone();
        changed.reason = "changed decision".into();
        assert!(retry_tool(runtime, &changed, McpCapability::Operator).is_err());
        let history = runtime
            .landing_attempt_record(&request.handoff_id, prior.attempt)
            .unwrap()
            .unwrap();
        assert_eq!(history.state, LandingAttemptState::Stopped);
        assert_eq!(history.job_run_id, prior.job_run_id);
        assert_eq!(history.claim_id, prior.claim_id);
        assert_eq!(history.authorization_id, prior.authorization_id);
        assert!(history.evidence.as_deref().unwrap().contains(original_run));
        assert_eq!(jobs.get_job_run(original_run).unwrap().unwrap(), original);
        assert_eq!(owner.accepted(&prior.claim_id), accepted);
        assert_eq!(owner.land(&task).unwrap()["phase"], "landed");
        assert_eq!(owner.pair.owner_status(&task), "done");
        let after = owner.pair.owner_task(&task);
        assert_eq!(before["job_run_id"], after["job_run_id"]);
        assert_eq!(before["job_run_machine"], after["job_run_machine"]);
        assert_eq!(
            retry_tool(runtime, &request, McpCapability::Operator).unwrap()["run_id"],
            responses[0]["run_id"]
        );
    }
}

/// Unknown/foreign evidence and live owner work never become dispatch
/// authority, including when an unresolved external intent exists.
fn dispatched_retry_requires_exact_terminal_owner_evidence() {
    for case in [
        "pending",
        "running",
        "missing",
        "foreign",
        "missing_with_intent",
    ] {
        let (owner, _task, mut request, mut prior) = retry_fixture(false, None);
        let runtime = &owner.pair.wire.owner;
        let jobs = &owner.pair.follower_jobs;
        let run_id = prior.job_run_id.as_deref().unwrap();
        match case {
            "pending" => {}
            "running" => {
                jobs.mark_job_run_running(run_id, Utc::now(), owner.workers.pid(run_id).unwrap())
                    .unwrap();
            }
            "missing" | "missing_with_intent" => {
                if case == "missing_with_intent" {
                    publish_retry_intent(&owner, &prior);
                }
                owner.finish_landing_worker(run_id, JobRunState::Cancelled);
                jobs.delete_job_run(run_id).unwrap();
            }
            "foreign" => {
                // Attach a real terminal owner job with matching-looking input
                // but without this attempt's dispatch key. Worker/foreign labels
                // and a terminal status cannot substitute for the exact binding.
                RuntimeHost::record_handoff_landing(
                    runtime,
                    &orbit_engine::HandoffLandingUpdate {
                        handoff_id: request.handoff_id.clone(),
                        step: orbit_engine::HandoffLandingStep::Stop { repairable: false },
                        observed: None,
                        evidence: "prepare mismatched attachment".into(),
                    },
                )
                .unwrap();
                let auth = ClaimInvocation::trusted_operator(
                    prior.task_id.clone(),
                    prior.claim_id.clone(),
                    "operator".into(),
                );
                runtime
                    .mutate_execution_claim(
                        Some(&auth),
                        "fixture-open",
                        &ClaimMutation::DispatchLanding {
                            handoff_id: request.handoff_id.clone(),
                            job_run_id: None,
                        },
                    )
                    .unwrap();
                let foreign = jobs
                    .insert_job_run(
                        orbit_core::application::landing::LANDING_JOB,
                        1,
                        Utc::now(),
                        Some(json!({"handoff_id": request.handoff_id, "task_id": prior.task_id})),
                        None,
                    )
                    .unwrap();
                jobs.finalize_job_run(&foreign.run_id, JobRunState::Cancelled, Utc::now(), None)
                    .unwrap();
                runtime
                    .mutate_execution_claim(
                        Some(&auth),
                        "fixture-attach",
                        &ClaimMutation::DispatchLanding {
                            handoff_id: request.handoff_id.clone(),
                            job_run_id: Some(foreign.run_id),
                        },
                    )
                    .unwrap();
                prior = runtime.landing_attempts().unwrap().remove(0);
                request.expected_latest_attempt = prior.attempt;
            }
            _ => unreachable!(),
        }
        let before = runtime.landing_attempts().unwrap();
        assert!(
            retry_tool(runtime, &request, McpCapability::Operator).is_err(),
            "{case}"
        );
        assert_eq!(runtime.landing_attempts().unwrap(), before, "{case}");
        if matches!(case, "missing" | "foreign" | "missing_with_intent") {
            assert!(
                runtime.land_handoff(&request.handoff_id).is_err(),
                "the shared dispatch boundary also refuses {case} evidence"
            );
            assert_eq!(runtime.landing_attempts().unwrap(), before, "{case}");
        }
        assert!(
            jobs.automation_job_for_key(&format!(
                "landing:{}:{}",
                request.handoff_id,
                prior.attempt + 1
            ))
            .unwrap()
            .is_none(),
            "{case}"
        );
    }
}

fn publish_retry_intent(owner: &OwnerLocal, prior: &LandingAttempt) {
    let accepted = owner.accepted(&prior.claim_id);
    RuntimeHost::record_handoff_landing(
        &owner.pair.wire.owner,
        &orbit_engine::HandoffLandingUpdate {
            handoff_id: prior.handoff_id.clone(),
            step: orbit_engine::HandoffLandingStep::PublishIntent {
                intent_id: "uncertain-owner-merge".into(),
            },
            observed: Some(accepted.handoff.candidate),
            evidence: "owner sent merge before terminal job outcome".into(),
        },
    )
    .unwrap();
}

/// Retry preserves uncertainty until the normal consumer reads the target.
/// A merged target completes without moving it again; an unreadable target
/// refuses before clearing the intent or performing a new merge.
fn terminal_job_retry_reconciles_external_intent_before_landing() {
    for case in ["not_merged", "already_merged", "unreadable"] {
        let (owner, task, request, prior) = retry_fixture(false, Some(JobRunState::Cancelled));
        let runtime = &owner.pair.wire.owner;
        publish_retry_intent(&owner, &prior);
        retry_tool(runtime, &request, McpCapability::Operator).unwrap();
        assert_eq!(
            runtime
                .resolve_execution_claims()
                .unwrap()
                .into_iter()
                .find(|c| c.claim.claim_id == prior.claim_id)
                .unwrap()
                .unresolved_merge_intent
                .as_deref(),
            Some("uncertain-owner-merge")
        );
        if case == "already_merged" {
            git(
                &owner.pair.owner_repo,
                &["merge", "--ff-only", &request.expected_candidate_commit],
            );
        } else if case == "unreadable" {
            git(
                &owner.pair.owner_repo,
                &["branch", "-m", "main", "unavailable-target"],
            );
            assert!(owner.land(&task).is_err());
            assert_eq!(
                runtime
                    .resolve_execution_claims()
                    .unwrap()
                    .into_iter()
                    .find(|c| c.claim.claim_id == prior.claim_id)
                    .unwrap()
                    .unresolved_merge_intent
                    .as_deref(),
                Some("uncertain-owner-merge")
            );
            assert_eq!(
                git(&owner.pair.owner_repo, &["rev-parse", "HEAD"]).trim(),
                request.expected_base_commit
            );
            git(
                &owner.pair.owner_repo,
                &["branch", "-m", "unavailable-target", "main"],
            );
        }
        assert_eq!(owner.land(&task).unwrap()["phase"], "landed");
        assert_eq!(
            git(&owner.pair.owner_repo, &["rev-parse", "main"]).trim(),
            request.expected_candidate_commit
        );
        assert!(
            runtime
                .resolve_execution_claims()
                .unwrap()
                .into_iter()
                .find(|c| c.claim.claim_id == prior.claim_id)
                .unwrap()
                .unresolved_merge_intent
                .is_none()
        );
        // A landed claim has no fresh retry authority even with a new key.
        let mut fresh = request.clone();
        fresh.request_key = "after-completion".into();
        assert!(retry_tool(runtime, &fresh, McpCapability::Operator).is_err());
    }
}

fn replica_and_managed_callers_cannot_recover_a_terminal_owner_job() {
    let (owner, _task, request, _prior) = retry_fixture(false, Some(JobRunState::Cancelled));
    let runtime = &owner.pair.wire.owner;
    let before = runtime.landing_attempts().unwrap();
    let replica = runtime
        .clone()
        .with_coordination_write_owner(Some("another-owner".into()));
    assert!(retry_tool(&replica, &request, McpCapability::Operator).is_err());
    assert!(retry_tool(runtime, &request, McpCapability::Agent).is_err());
    {
        let _managed = orbit_common::test_env::scoped([("ORBIT_MANAGED_RUN_CONTEXT", Some("1"))]);
        assert!(retry_tool(runtime, &request, McpCapability::Operator).is_err());
    }
    assert_eq!(runtime.landing_attempts().unwrap(), before);
}

fn a_redacted_retry_reason_preserves_success_audit_and_replay() {
    let (owner, task, mut request, stopped) = stopped_retry_fixture();
    let runtime = &owner.pair.wire.owner;
    let home = std::env::var("HOME").unwrap();
    request.reason = format!("provider state refreshed under {home}/provider");
    let result = retry_tool(runtime, &request, McpCapability::Operator);

    // Read the durable outcome even if the tool loses its successful reply.
    let boundary = orbit_store::TaskCommitBoundary::new(
        runtime.sqlite_store().unwrap(),
        orbit_store::maintenance::task_registry::TaskRegistryStore::open(
            &orbit_store::maintenance::task_registry::task_registry_path(&runtime.global_root()),
        )
        .unwrap(),
        runtime.workspace_id().unwrap(),
    )
    .unwrap();
    let receipt = boundary
        .landing_retry_receipt(&request.handoff_id, &request.request_key)
        .unwrap()
        .unwrap();
    let mut sanitized_request = request.clone();
    sanitized_request.reason = "provider state refreshed under ~/provider".into();
    assert_eq!(receipt.request, sanitized_request);
    assert_eq!(receipt.attempt, stopped.attempt + 1);
    let attempt = runtime.landing_attempts().unwrap().remove(0);
    let run_id = owner
        .pair
        .follower_jobs
        .automation_job_for_key(&format!(
            "landing:{}:{}",
            request.handoff_id, receipt.attempt
        ))
        .unwrap()
        .unwrap();
    assert_eq!(attempt.job_run_id.as_deref(), Some(run_id.as_str()));
    let response = result.expect("a redacted reason must return the durably submitted retry");
    assert_eq!(response["task_id"], task);
    assert_eq!(response["run_id"], run_id);
    assert_eq!(response["attempt"], receipt.attempt);
    assert_eq!(response["submitted"], true);
    assert_eq!(response["redactions_applied"], true);

    let rows = runtime
        .list_audit_events(None, None, None, None, 100)
        .unwrap();
    let audit = rows
        .iter()
        .find(|row| {
            row.command == "artifact_redaction"
                && row.tool_name.as_deref() == Some("orbit.task.retry_handoff")
                && row.task_id.as_deref() == Some(task.as_str())
        })
        .expect("retry reason redaction must be attributable to the persisted task");
    assert_eq!(audit.target_type.as_deref(), Some("task"));
    assert_eq!(audit.target_id.as_deref(), Some(task.as_str()));
    let payload: Value = serde_json::from_str(audit.arguments_json.as_deref().unwrap()).unwrap();
    assert_eq!(payload["field_path"], "reason");
    assert_eq!(payload["artifact_id"], task);
    assert!(!audit.arguments_json.as_deref().unwrap().contains(&home));

    let replay = retry_tool(runtime, &request, McpCapability::Operator).unwrap();
    assert_eq!(replay["run_id"], response["run_id"]);
    assert_eq!(replay["attempt"], response["attempt"]);
    assert_eq!(replay["submitted"], false);
    assert_eq!(runtime.landing_attempts().unwrap(), vec![attempt]);
    assert_eq!(owner.land(&task).unwrap()["phase"], "landed");
    let settled = runtime.landing_attempts().unwrap();
    let replay = retry_tool(runtime, &request, McpCapability::Operator).unwrap();
    assert_eq!(replay["run_id"], response["run_id"]);
    assert_eq!(replay["attempt"], response["attempt"]);
    assert_eq!(replay["submitted"], false);
    assert_eq!(runtime.landing_attempts().unwrap(), settled);
    assert_eq!(
        boundary
            .landing_retry_receipt(&request.handoff_id, &request.request_key)
            .unwrap(),
        Some(receipt)
    );
    assert_eq!(
        runtime
            .landing_attempt_record(&request.handoff_id, stopped.attempt)
            .unwrap(),
        Some(stopped)
    );
}

fn retry_tool(
    runtime: &OrbitRuntime,
    request: &orbit_types::workflow::handoff::LandingRetryRequest,
    capability: McpCapability,
) -> Result<Value, OrbitError> {
    let mut input = serde_json::to_value(request).unwrap();
    input["workspace"] = json!(runtime.workspace_id().unwrap());
    retry_tool_input(runtime, input, capability)
}

fn retry_tool_input(
    runtime: &OrbitRuntime,
    input: Value,
    capability: McpCapability,
) -> Result<Value, OrbitError> {
    runtime.run_tool_with_context_and_role(
        "orbit.task.retry_handoff",
        input,
        Role::Admin,
        ToolContext {
            session_context: ToolSessionContext {
                effective_capabilities: BTreeSet::from([capability]),
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

fn stopped_retry_fixture() -> (
    OwnerLocal,
    String,
    orbit_types::workflow::handoff::LandingRetryRequest,
    LandingAttempt,
) {
    retry_fixture(true, Some(JobRunState::Cancelled))
}

fn retry_fixture(
    stop: bool,
    terminal: Option<JobRunState>,
) -> (
    OwnerLocal,
    String,
    orbit_types::workflow::handoff::LandingRetryRequest,
    LandingAttempt,
) {
    retry_fixture_with_evidence(
        stop,
        terminal,
        "required provider review was not yet satisfied",
    )
}

fn retry_fixture_with_evidence(
    stop: bool,
    terminal: Option<JobRunState>,
    evidence: &str,
) -> (
    OwnerLocal,
    String,
    orbit_types::workflow::handoff::LandingRetryRequest,
    LandingAttempt,
) {
    let owner = OwnerLocal::new();
    let leaf = owner.handed_off_leaf(|checkout, _| {
        std::fs::write(checkout.join(FILE), FIRST).unwrap();
        "Outcome: success\nImplemented and validated a candidate."
    });
    let runtime = &owner.pair.wire.owner;
    let task = owner.pair.claimed_task(&leaf);
    let accepted = owner.accepted(&owner.claim_of(&leaf));
    if stop {
        RuntimeHost::record_handoff_landing(
            runtime,
            &orbit_engine::HandoffLandingUpdate {
                handoff_id: accepted.handoff_id.clone(),
                step: orbit_engine::HandoffLandingStep::Stop { repairable: false },
                observed: None,
                evidence: evidence.into(),
            },
        )
        .unwrap();
    }
    let stopped = runtime.landing_attempts().unwrap().remove(0);
    // Acquire the actual child before settling its run. Cancellation is public;
    // Failed is produced by the real observer after a running child exits.
    let run_id = stopped.job_run_id.as_deref().unwrap();
    owner.own_landing_worker(run_id);
    if let Some(terminal) = terminal {
        owner.finish_landing_worker(run_id, terminal);
    }

    let request = orbit_types::workflow::handoff::LandingRetryRequest {
        handoff_id: accepted.handoff_id,
        expected_candidate_commit: accepted.handoff.candidate.candidate.commit,
        expected_base_commit: accepted.handoff.candidate.base.commit,
        expected_latest_attempt: stopped.attempt,
        expected_latest_state: stopped.state,
        reason: "provider conditions now permit this exact candidate".into(),
        request_key: "operator-retry-1".into(),
    };
    (owner, task, request, stopped)
}

/// Independent competing requests compare-and-reserve once. Replays keep
/// naming the winner rather than opening a further landing attempt.
fn competing_operator_retries_reserve_one_attempt_and_refuse_stale_inputs() {
    let (owner, _task, request, stopped) = retry_fixture(false, Some(JobRunState::Cancelled));
    let runtime = &owner.pair.wire.owner;
    for (field, value) in [
        ("workspace", json!("ws_foreign")),
        ("expected_candidate_commit", json!("a".repeat(40))),
        ("expected_base_commit", json!("b".repeat(40))),
        ("expected_latest_attempt", json!(stopped.attempt + 1)),
        ("expected_latest_state", json!("stopped")),
        ("reason", json!(" ")),
        ("request_key", json!("")),
        ("auto_fix", json!(true)),
        ("head_override", json!("a".repeat(40))),
    ] {
        let mut input = serde_json::to_value(&request).unwrap();
        input["workspace"] = json!(runtime.workspace_id().unwrap());
        input[field] = value;
        assert!(
            retry_tool_input(runtime, input, McpCapability::Operator).is_err(),
            "{field}"
        );
        assert_eq!(
            runtime.landing_attempts().unwrap().remove(0),
            stopped,
            "refusal changes no attempt: {field}"
        );
    }
    assert!(retry_tool(runtime, &request, McpCapability::Agent).is_err());
    let mut second = request.clone();
    second.request_key = "independent-retry-2".into();
    let responses = std::thread::scope(|scope| {
        let a = scope.spawn(|| retry_tool(runtime, &request, McpCapability::Operator));
        let b = scope.spawn(|| retry_tool(runtime, &second, McpCapability::Operator));
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(
        responses.iter().filter(|result| result.is_ok()).count(),
        1,
        "{responses:?}"
    );
    assert_eq!(
        runtime.landing_attempts().unwrap()[0].attempt,
        stopped.attempt + 1
    );
    let winner = if responses[0].is_ok() {
        &request
    } else {
        &second
    };
    let replay = retry_tool(runtime, winner, McpCapability::Operator).unwrap();
    assert_eq!(
        replay["run_id"],
        responses.iter().find_map(|r| r.as_ref().ok()).unwrap()["run_id"]
    );
    let mut changed = winner.clone();
    changed.reason = "different decision".into();
    assert!(retry_tool(runtime, &changed, McpCapability::Operator).is_err());
}

/// A crash after atomic reservation leaves the ordinary landing outbox
/// recoverable. Losing the submission/attach reply resolves the same run key.
fn operator_retry_reservations_recover_before_submission_and_after_lost_attachment() {
    for crash in ["before_submit", "after_submit", "revoked_before_submit"] {
        let (owner, _task, request, stopped) = stopped_retry_fixture();
        let runtime = &owner.pair.wire.owner;
        let accepted = owner.accepted(&stopped.claim_id);
        let context = ClaimInvocation::trusted_operator(
            stopped.task_id.clone(),
            stopped.claim_id.clone(),
            orbit_core::ActorIdentity::from_env().label,
        )
        .with_handoff_observation(HandoffObservation {
            candidate: accepted.handoff.candidate.clone(),
            footprint_widening: accepted.handoff.footprint_widening.clone(),
            required_commands: accepted.required_commands,
            owner_completion_authority: Some(
                "workspace-config:workflow.distributed_completion".into(),
            ),
            review: None,
            known_red: vec![],
        });
        // The same public coordination transaction used by the operator boundary,
        // interrupted before a job is submitted, with no timing or load injection.
        runtime
            .mutate_execution_claim(
                Some(&context),
                "landing-retry:operator-retry-1",
                &ClaimMutation::RetryLanding(request.clone()),
            )
            .unwrap();
        assert_eq!(runtime.landing_attempts().unwrap()[0].job_run_id, None);
        if crash == "revoked_before_submit" {
            runtime
                .revoke_handoff_as_operator(
                    &request.handoff_id,
                    &orbit_core::application::review::ExpectedCandidate {
                        candidate_commit: request.expected_candidate_commit.clone(),
                        base_commit: request.expected_base_commit.clone(),
                    },
                    "operator",
                    "authority withdrawn before retry submission",
                    "revoke-reserved",
                )
                .unwrap();
            assert!(retry_tool(runtime, &request, McpCapability::Operator).is_err());
            assert!(
                owner
                    .pair
                    .follower_jobs
                    .automation_job_for_key(&format!(
                        "landing:{}:{}",
                        request.handoff_id,
                        stopped.attempt + 1
                    ))
                    .unwrap()
                    .is_none()
            );
            continue;
        }
        let run_id = if crash == "after_submit" {
            // Public job persistence boundary, interrupted after the deterministic
            // action-key admission and before attachment; no synthetic timing.
            let admission = owner
                .pair
                .follower_jobs
                .insert_automation_job_run(
                    orbit_core::application::landing::LANDING_JOB,
                    json!({"handoff_id": request.handoff_id, "task_id": stopped.task_id}),
                    &format!("landing:{}:{}", request.handoff_id, stopped.attempt + 1),
                )
                .unwrap();
            let orbit_store::contracts::KeyedJobRunAdmission::Admitted(run) = admission else {
                panic!("one new run")
            };
            // This crash window intentionally stops at persistence: no worker
            // was launched, so fixture teardown has no child to acquire/reap.
            owner.workers.persisted_only(&run.run_id);
            run.run_id
        } else {
            let recovered = runtime.dispatch_landing_requests().unwrap().remove(0);
            assert_eq!(recovered.attempt, stopped.attempt + 1);
            recovered.run_id
        };
        let replay = retry_tool(runtime, &request, McpCapability::Operator).unwrap();
        assert_eq!(replay["run_id"], run_id);
        assert_eq!(replay["submitted"], false);
        assert_eq!(
            runtime.landing_attempts().unwrap()[0].job_run_id.as_deref(),
            Some(run_id.as_str())
        );
        assert_eq!(
            retry_tool(runtime, &request, McpCapability::Operator).unwrap()["run_id"],
            run_id
        );
        if crash == "after_submit" {
            owner
                .pair
                .follower_jobs
                .mark_job_run_running(&run_id, Utc::now(), std::process::id())
                .unwrap();
            RuntimeHost::record_handoff_landing(
                runtime,
                &orbit_engine::HandoffLandingUpdate {
                    handoff_id: request.handoff_id.clone(),
                    step: orbit_engine::HandoffLandingStep::PublishIntent {
                        intent_id: "live-owner-intent".into(),
                    },
                    observed: Some(accepted.handoff.candidate.clone()),
                    evidence: "owner job is still carrying the external intent".into(),
                },
            )
            .unwrap();
            let mut competing = request.clone();
            competing.request_key = "another-live-retry".into();
            competing.expected_latest_attempt = stopped.attempt + 1;
            competing.expected_latest_state = LandingAttemptState::Dispatched;
            let before = runtime.landing_attempts().unwrap();
            assert!(
                retry_tool(runtime, &competing, McpCapability::Operator).is_err(),
                "a live owner job cannot get a second uncertain-intent attempt"
            );
            assert_eq!(runtime.landing_attempts().unwrap(), before);
        }
    }
}

fn revoked_changed_and_repair_pending_handoffs_refuse_operator_retry() {
    for case in ["revoked", "changed_candidate", "repair_pending"] {
        let (owner, task, request, stopped) = retry_fixture(false, Some(JobRunState::Cancelled));
        let runtime = &owner.pair.wire.owner;
        match case {
            "revoked" => {
                runtime
                    .revoke_handoff_as_operator(
                        &request.handoff_id,
                        &orbit_core::application::review::ExpectedCandidate {
                            candidate_commit: request.expected_candidate_commit.clone(),
                            base_commit: request.expected_base_commit.clone(),
                        },
                        "operator",
                        "authority withdrawn",
                        "revoke-before-retry",
                    )
                    .unwrap();
            }
            "changed_candidate" => {
                let accepted = owner.accepted(&stopped.claim_id);
                let worktrees = git(&owner.pair.owner_repo, &["worktree", "list", "--porcelain"]);
                let path = worktrees
                    .split("\n\n")
                    .find(|entry| {
                        entry.lines().any(|line| {
                            line == format!(
                                "branch refs/heads/{}",
                                accepted.handoff.candidate.source_branch
                            )
                        })
                    })
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap()
                    .strip_prefix("worktree ")
                    .unwrap();
                git(
                    Path::new(path),
                    &[
                        "commit",
                        "-q",
                        "--allow-empty",
                        "-m",
                        "Changed source after validation",
                    ],
                );
            }
            "repair_pending" => {
                runtime.land_handoff(&request.handoff_id).unwrap();
                owner.base_moves(MOVED);
                owner.land(&task).unwrap_err();
                assert_eq!(owner.claim_phase(&stopped.claim_id), "repair_pending");
            }
            _ => unreachable!(),
        }
        let before = runtime.landing_attempts().unwrap();
        assert!(
            retry_tool(runtime, &request, McpCapability::Operator).is_err(),
            "{case}"
        );
        assert_eq!(
            runtime.landing_attempts().unwrap(),
            before,
            "{case} preserves all attempts"
        );
    }
}

crate::scenario_table!(
    public_owner_handoff_retry_scenarios,
    terminal_owner_jobs_reconcile_dispatched_attempts_through_the_operator_tool,
    dispatched_retry_requires_exact_terminal_owner_evidence,
    terminal_job_retry_reconciles_external_intent_before_landing,
    replica_and_managed_callers_cannot_recover_a_terminal_owner_job,
    a_redacted_retry_reason_preserves_success_audit_and_replay,
    a_stopped_authorized_handoff_retries_once_through_the_public_operator_tool,
    competing_operator_retries_reserve_one_attempt_and_refuse_stale_inputs,
    operator_retry_reservations_recover_before_submission_and_after_lost_attachment,
    revoked_changed_and_repair_pending_handoffs_refuse_operator_retry
);
