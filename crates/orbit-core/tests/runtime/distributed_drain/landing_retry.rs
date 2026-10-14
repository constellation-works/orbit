//! Public operator retry of the existing authorized landing.

use super::*;

/// Reproduces the missing public same-handoff retry seam. The old stopped
/// attempt survives, the normal landing action completes, and replay keeps
/// naming that one run after completion without changing the leaf binding.
fn a_stopped_authorized_handoff_retries_once_through_the_public_operator_tool() {
    let (owner, task, request, stopped) = stopped_retry_fixture();
    let runtime = &owner.pair.wire.owner;
    let before = owner.pair.owner_task(&task);
    let responses = std::thread::scope(|scope| {
        let a = scope.spawn(|| retry_tool(runtime, &request, McpCapability::Operator));
        let b = scope.spawn(|| retry_tool(runtime, &request, McpCapability::Operator));
        [a.join().unwrap().unwrap(), b.join().unwrap().unwrap()]
    });
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
    let owner = OwnerLocal::new();
    let leaf = owner.handed_off_leaf(|checkout, _| {
        std::fs::write(checkout.join(FILE), FIRST).unwrap();
        "Outcome: success\nImplemented and validated a candidate."
    });
    let runtime = &owner.pair.wire.owner;
    let task = owner.pair.claimed_task(&leaf);
    let accepted = owner.accepted(&owner.claim_of(&leaf));
    RuntimeHost::record_handoff_landing(
        runtime,
        &orbit_engine::HandoffLandingUpdate {
            handoff_id: accepted.handoff_id.clone(),
            step: orbit_engine::HandoffLandingStep::Stop { repairable: false },
            observed: None,
            evidence: "required provider review was not yet satisfied".into(),
        },
    )
    .unwrap();
    let stopped = runtime.landing_attempts().unwrap().remove(0);
    // The substitute has no landing activity. Settle its owner-local job at
    // the public persistence boundary so the stopped fixture never depends
    // on the short-lived substitute's exit timing.
    owner
        .pair
        .follower_jobs
        .finalize_job_run(
            stopped.job_run_id.as_deref().unwrap(),
            JobRunState::Cancelled,
            Utc::now(),
            None,
        )
        .unwrap();

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
    let (owner, _task, request, stopped) = stopped_retry_fixture();
    let runtime = &owner.pair.wire.owner;
    for (field, value) in [
        ("workspace", json!("ws_foreign")),
        ("expected_candidate_commit", json!("a".repeat(40))),
        ("expected_base_commit", json!("b".repeat(40))),
        ("expected_latest_attempt", json!(stopped.attempt + 1)),
        ("expected_latest_state", json!("dispatched")),
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
        let (owner, task, request, stopped) = stopped_retry_fixture();
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
    a_stopped_authorized_handoff_retries_once_through_the_public_operator_tool,
    competing_operator_retries_reserve_one_attempt_and_refuse_stale_inputs,
    operator_retry_reservations_recover_before_submission_and_after_lost_attachment,
    revoked_changed_and_repair_pending_handoffs_refuse_operator_retry
);
