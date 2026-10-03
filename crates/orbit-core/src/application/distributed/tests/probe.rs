//! The read-only probe, receipt lookup and claim inspection, through the tool
//! boundary.

use super::*;

#[test]
fn probe_reports_owner_facts_and_creates_no_admission_state() {
    if !enter_isolated_child("probe::probe_reports_owner_facts_and_creates_no_admission_state") {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);

    let report = run_as(&runtime, follower_session(), "orbit.drain.probe", json!({}))
        .expect("probe succeeds");

    assert_eq!(report["protocol_schema"], 1);
    assert_eq!(report["binary_version"], owner_binary_version());
    assert_eq!(report["review_policy"], "none");
    assert_eq!(report["ship"]["review_policy"], "none");
    assert_eq!(report["admits"], true);
    assert!(report["refusal"].is_null());
    assert_eq!(report["creates_admission_state"], false);
    // The forwarded label is reported for diagnostics and nothing else: the
    // session still holds only what the destination served it.
    assert_eq!(report["session"]["caller_machine_id"], FOLLOWER);
    assert_eq!(report["session"]["remote"], true);
    assert_eq!(report["session"]["capabilities"], json!(["agent"]));

    // Nothing durable moved: no claim, no reservation, no receipt, and the
    // candidate task is still in the backlog.
    assert!(
        runtime
            .inspect_distributed_claims()
            .expect("claims")
            .is_empty()
    );
    assert!(
        boundary(&runtime)
            .admission_storage_usage()
            .expect("usage")
            .receipts
            == 0
    );
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
}

#[test]
fn probe_reports_the_first_refusal_the_admission_ladder_would_raise() {
    if !enter_isolated_child(
        "probe::probe_reports_the_first_refusal_the_admission_ladder_would_raise",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();

    // Malformed version field: invalid input, not a compatibility comparison.
    let malformed = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.probe",
        json!({"caller_version": "9.9.9", "caller_schema": "not-a-number"}),
    )
    .expect_err("malformed schema refused");
    assert!(
        matches!(&malformed, orbit_common::OrbitError::InvalidInput(message)
            if message.contains("caller_schema") && !message.contains("invalid_input")),
        "the reason code travels as the error kind, not repeated in its text: {malformed:?}"
    );

    // Version is compared before mode and policy: this caller is wrong about
    // all three, and hears about the binary first.
    let stale = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.probe",
        json!({
            "caller_version": "0.0.0-old",
            "caller_schema": 2,
            "caller_review_policy": "before-pr",
        }),
    )
    .expect("probe answers rather than raising");
    assert_eq!(stale["admits"], false);
    assert_eq!(stale["refusal"], "version_mismatch");

    // A non-`none` executor policy is rejected by name, never downgraded.
    for policy in ["before-pr", "after-landing"] {
        let refused = run_as(
            &runtime,
            follower_session(),
            "orbit.drain.probe",
            json!({
                "caller_version": owner_binary_version(),
                "caller_schema": 1,
                "caller_review_policy": policy,
            }),
        )
        .expect("probe answers");
        assert_eq!(refused["admits"], false, "{policy}");
        assert_eq!(refused["refusal"], "review_policy_unsupported", "{policy}");
        assert!(
            refused["diagnostics"]
                .as_array()
                .expect("diagnostics")
                .iter()
                .any(|line| line.as_str().is_some_and(|line| line.contains(policy))),
            "{policy}: {refused}"
        );
    }

    // Still nothing durable after every refusal above.
    assert_eq!(
        boundary(&runtime)
            .admission_storage_usage()
            .expect("usage")
            .receipts,
        0
    );
}

#[test]
fn remote_probe_against_local_ship_mode_matches_fully_declared_verdict() {
    if !enter_isolated_child(
        "probe::remote_probe_against_local_ship_mode_matches_fully_declared_verdict",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = local_ship_runtime();
    let matching = json!({
        "caller_version": owner_binary_version(),
        "caller_schema": 1,
        "caller_review_policy": "none",
    });

    let bare =
        run_as(&runtime, follower_session(), "orbit.drain.probe", json!({})).expect("bare probe");
    let declared = run_as(&runtime, follower_session(), "orbit.drain.probe", matching)
        .expect("fully declared probe");
    let version_only = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.probe",
        json!({"caller_version": owner_binary_version()}),
    )
    .expect("version-only probe");

    assert_eq!(bare["ship"]["mode"], "local");
    assert_eq!(bare["session"]["remote"], true);
    assert_eq!(bare["admits"], false);
    assert_eq!(bare["refusal"], "ship_mode_unsupported");
    assert_eq!(declared["admits"], bare["admits"]);
    assert_eq!(declared["refusal"], bare["refusal"]);
    assert_ne!(version_only["refusal"], "invalid_input");
    assert_eq!(version_only["admits"], bare["admits"]);
    assert_eq!(version_only["refusal"], bare["refusal"]);
}

#[test]
fn an_upgraded_lookup_finds_the_original_receipt_without_rewriting_it() {
    if !enter_isolated_child(
        "probe::an_upgraded_lookup_finds_the_original_receipt_without_rewriting_it",
    ) {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    // The receipt is written by an older binary pair; the lookup below runs on
    // this one.
    let admitted = admit(
        &runtime,
        &repo_root,
        FOLLOWER,
        &admission_request("req-1", "0.0.0-old"),
        "0.0.0-old",
    );
    let claim = claim_id(&admitted);

    let found = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.receipt.lookup",
        json!({"request_id": "req-1"}),
    )
    .expect("lookup succeeds");

    assert_eq!(found["outcome"], "found");
    assert_eq!(found["machine_id"], FOLLOWER);
    assert_eq!(found["lookup_schema"], 1);
    // Original input, verbatim. The lookup does not reapply the parity, ship,
    // or policy checks the original admission made, and does not rewrite
    // `caller_version` to this binary.
    assert_eq!(found["receipt"]["request"]["caller_version"], "0.0.0-old");
    assert_eq!(found["receipt"]["request"]["request_id"], "req-1");
    assert_eq!(found["current_claim"]["claim_id"], claim.as_str());
    assert_eq!(found["current_claim"]["phase"], "claimed");
    assert_eq!(found["grants_execution_authority"], false);
    assert_eq!(found["permits_replacement_request"], false);
}

#[test]
fn an_incompatible_lookup_protocol_refuses_instead_of_answering() {
    if !enter_isolated_child("probe::an_incompatible_lookup_protocol_refuses_instead_of_answering")
    {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    admit(
        &runtime,
        &repo_root,
        FOLLOWER,
        &admission_request("req-1", owner_binary_version()),
        owner_binary_version(),
    );

    let error = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.receipt.lookup",
        json!({"request_id": "req-1", "lookup_schema": 2}),
    )
    .expect_err("incompatible protocol refuses");

    assert!(error.to_string().contains("version_mismatch"), "{error}");
    // The remedy is owner-side inspection, not a replacement request.
    assert!(error.to_string().contains("claim inspection"), "{error}");
}

#[test]
fn not_found_grants_nothing_and_licenses_no_replacement_request() {
    if !enter_isolated_child("probe::not_found_grants_nothing_and_licenses_no_replacement_request")
    {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();

    let missing = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.receipt.lookup",
        json!({"request_id": "never-sent"}),
    )
    .expect("lookup answers");

    assert_eq!(missing["outcome"], "not_found");
    assert!(missing["receipt"].is_null());
    assert!(missing["current_claim"].is_null());
    assert_eq!(missing["grants_execution_authority"], false);
    assert_eq!(missing["permits_replacement_request"], false);
    assert!(
        missing["guidance"]
            .as_str()
            .expect("guidance")
            .contains("original request ID")
    );
    // Reading an absent request creates no receipt to find next time.
    assert_eq!(
        boundary(&runtime)
            .admission_storage_usage()
            .expect("usage")
            .receipts,
        0
    );
}

#[test]
fn a_worker_reads_its_own_namespace_and_only_an_operator_reads_across_attempts() {
    if !enter_isolated_child(
        "probe::a_worker_reads_its_own_namespace_and_only_an_operator_reads_across_attempts",
    ) {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    admit(
        &runtime,
        &repo_root,
        "hm_other_follower",
        &admission_request("req-other", owner_binary_version()),
        owner_binary_version(),
    );

    // A session cannot borrow another machine's receipt namespace by naming it.
    let refused = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.receipt.lookup",
        json!({"request_id": "req-other", "machine_id": "hm_other_follower"}),
    )
    .expect_err("cross-attempt inspection refused");
    assert!(
        matches!(refused, orbit_common::OrbitError::CapabilityRefused(_)),
        "{refused}"
    );

    // Its own namespace simply has no such request; the label bought nothing.
    let own = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.receipt.lookup",
        json!({"request_id": "req-other"}),
    )
    .expect("lookup answers");
    assert_eq!(own["outcome"], "not_found");

    // The owner operator retains cross-attempt inspection for recovery.
    let operator = run_as(
        &runtime,
        operator_session(),
        "orbit.drain.receipt.lookup",
        json!({"request_id": "req-other", "machine_id": "hm_other_follower"}),
    )
    .expect("operator lookup succeeds");
    assert_eq!(operator["outcome"], "found");
    assert_eq!(operator["machine_id"], "hm_other_follower");
}

#[test]
fn a_revoked_claim_is_reported_as_revoked_and_confers_no_authority() {
    if !enter_isolated_child(
        "probe::a_revoked_claim_is_reported_as_revoked_and_confers_no_authority",
    ) {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    let admitted = admit(
        &runtime,
        &repo_root,
        FOLLOWER,
        &admission_request("req-1", owner_binary_version()),
        owner_binary_version(),
    );
    let claim = claim_id(&admitted);

    // Deliberate recovery: an operator revokes the attempt and returns the task
    // to the backlog.
    runtime
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_operator(
                task.id.clone(),
                claim.clone(),
                "operator".to_string(),
            )),
            "recover-1",
            &ClaimMutation::Recover {
                status: TaskStatus::Backlog,
                reason: "host unreachable".to_string(),
            },
        )
        .expect("recover");

    let found = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.receipt.lookup",
        json!({"request_id": "req-1"}),
    )
    .expect("lookup answers");
    assert_eq!(found["outcome"], "found");
    assert_eq!(found["current_claim"]["phase"], "revoked");
    assert_eq!(found["grants_execution_authority"], false);

    // The returning worker's own attempt cannot mutate anything after
    // revocation, whatever its receipt says.
    let stale = runtime
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_worker(
                task.id.clone(),
                claim,
                FOLLOWER.to_string(),
                None,
            )),
            "evidence-1",
            &ClaimMutation::Evidence(Default::default()),
        )
        .expect_err("stale attempt refused");
    assert!(stale.to_string().contains("stale_claim"), "{stale}");
}

#[test]
fn claim_mutation_requires_trusted_invocation_context() {
    if !enter_isolated_child("probe::claim_mutation_requires_trusted_invocation_context") {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();

    let error = runtime
        .mutate_execution_claim(
            None,
            "mutation-1",
            &ClaimMutation::Evidence(Default::default()),
        )
        .expect_err("missing context fails closed");

    assert!(
        error.to_string().contains("claim invocation context"),
        "{error}"
    );
}

#[test]
fn the_read_only_surface_serves_an_owner_local_session_and_refuses_a_replica() {
    if !enter_isolated_child(
        "probe::the_read_only_surface_serves_an_owner_local_session_and_refuses_a_replica",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();

    let local = run_as(
        &runtime,
        owner_local_session(),
        "orbit.drain.probe",
        json!({}),
    )
    .expect("owner-local probe");
    assert_eq!(local["session"]["remote"], false);
    assert_eq!(local["session"]["caller_machine_id"], OWNER);

    let replica = runtime
        .clone()
        .with_coordination_write_owner(Some(OWNER.to_string()));
    for (tool, input) in [
        ("orbit.drain.probe", json!({})),
        ("orbit.drain.receipt.lookup", json!({"request_id": "req-1"})),
    ] {
        let error = run_as(&replica, follower_session(), tool, input)
            .expect_err("a replica serves no owner question");
        assert!(
            matches!(error, orbit_common::OrbitError::CapabilityRefused(_)),
            "{tool}: {error}"
        );
    }
}

/// The identification floor, on the surface that resolves a session and
/// nothing else [ORB-12582].
///
/// An MCP session's capabilities come from the server process that serves it,
/// so a session asserting none is a caller this destination cannot name — and
/// the governed row, not a capability read inside the application function, is
/// what refuses it.
#[test]
fn a_session_without_agent_capability_reaches_neither_read_only_tool() {
    if !enter_isolated_child(
        "probe::a_session_without_agent_capability_reaches_neither_read_only_tool",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();
    let anonymous = ToolSessionContext {
        effective_capabilities: BTreeSet::new(),
        ..follower_session()
    };

    for (tool, input) in [
        ("orbit.drain.probe", json!({})),
        ("orbit.drain.receipt.lookup", json!({"request_id": "req-1"})),
    ] {
        let error = runtime
            .execute_tool_command_dispatch_with_session_context(
                tool,
                input,
                None,
                None,
                ToolEntryPoint::Mcp,
                anonymous.clone(),
            )
            .expect_err("agent capability required");
        match error {
            orbit_common::OrbitError::CapabilityDenied(message) => {
                assert!(message.contains(tool), "{tool}: {message}");
                assert!(message.contains("agent"), "{tool}: {message}");
            }
            other => panic!("expected a capability denial for {tool}, got: {other}"),
        }
    }
}

/// Every read-only drain tool has an entry point that actually resolves
/// [ORB-12581].
///
/// Probe and receipt lookup use the internal runtime route. Claim
/// inspection is not advertised and has no subcommand of its own, which leaves
/// `orbit tool run` as its only route — and that route applies
/// `ensure_tool_agent_facing`, so registering it inactive made the documented
/// operator command fail on every surface. This drives the whole CLI dispatch
/// path, not just the registry, so a registration no entry point can reach
/// cannot land again.
#[test]
fn the_operator_reaches_claim_inspection_through_the_cli_tool_route() {
    if !enter_isolated_child(
        "probe::the_operator_reaches_claim_inspection_through_the_cli_tool_route",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();

    let advertised = runtime
        .list_mcp_tool_definitions()
        .expect("mcp definitions")
        .into_iter()
        .map(|definition| definition.schema.name)
        .collect::<BTreeSet<_>>();
    assert!(!advertised.contains("orbit.drain.probe"));
    assert!(!advertised.contains("orbit.drain.receipt.lookup"));
    assert!(
        !advertised.contains("orbit.drain.claims"),
        "claim inspection stays an operator surface, off MCP"
    );

    // The gate `orbit tool run` applies before dispatch, for every read-only
    // drain tool: an unadvertised tool must still be reachable there.
    for name in [
        "orbit.drain.probe",
        "orbit.drain.receipt.lookup",
        "orbit.drain.claims",
    ] {
        runtime
            .ensure_tool_agent_facing(name)
            .unwrap_or_else(|error| panic!("{name} is reachable from no entry point: {error}"));
    }

    let claims = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.drain.claims",
            json!({}),
            None,
            None,
            ToolEntryPoint::Cli,
            operator_session(),
        )
        .expect("the operator route lists claims")
        .value;
    assert_eq!(claims, json!([]), "a fresh owner holds no claims");
}

/// The same route refuses an agent — placement is not what governs it.
#[test]
fn an_agent_session_is_refused_claim_inspection_on_the_same_route() {
    if !enter_isolated_child(
        "probe::an_agent_session_is_refused_claim_inspection_on_the_same_route",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();

    let error = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.drain.claims",
            json!({}),
            None,
            None,
            ToolEntryPoint::Cli,
            owner_local_session(),
        )
        .expect_err("an agent session must not read across attempts");
    match error {
        orbit_common::OrbitError::CapabilityDenied(message) => {
            assert!(message.contains("orbit.drain.claims"), "{message}");
            assert!(message.contains("operator"), "{message}");
        }
        other => panic!("expected a capability denial, got: {other}"),
    }
}

/// The MCP server and `orbit tool run` share one registry and one application
/// entry point, so the lifecycle checks cannot differ by surface. Opening the
/// feature registered exactly what an executor needs — pull, bind, settle —
/// and nothing that decides completion: approval, revocation and recovery
/// stay owner-operator dashboard actions with no tool at all [ORB-13625].
#[test]
fn only_the_executor_lifecycle_is_registered_when_the_feature_is_open() {
    if !enter_isolated_child(
        "probe::only_the_executor_lifecycle_is_registered_when_the_feature_is_open",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();

    const { assert!(DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED) };
    ensure_distributed_mutation_available("orbit.task.pull").expect("open");

    // Every registered tool, advertised or not: `orbit tool run` reaches an
    // unadvertised tool, so advertisement is not what keeps one unreachable.
    let registered = runtime
        .tool_registry()
        .all_schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect::<Vec<_>>();
    for name in &registered {
        assert!(
            !matches!(
                name.as_str(),
                "orbit.drain.pull"
                    | "orbit.drain.handoff.accept"
                    | "orbit.drain.handoff.approve"
                    | "orbit.drain.handoff.revoke"
                    | "orbit.drain.claim.recover"
            ),
            "{name} decides completion or recovery and must stay an owner-operator action"
        );
    }
    for name in [
        "orbit.drain.probe",
        "orbit.drain.receipt.lookup",
        "orbit.drain.claims",
        "orbit.task.pull",
        "orbit.drain.claim.bind",
        "orbit.drain.claim.settle",
    ] {
        assert!(
            registered.iter().any(|known| known == name),
            "{name} is missing from the tool registry"
        );
    }
}
