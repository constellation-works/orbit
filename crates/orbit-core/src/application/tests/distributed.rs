//! The owner's read-only distributed-drain surface [ORB-12495].
//!
//! These exercise the tool boundary rather than the application functions
//! directly, because the boundary is where the authority facts are decided:
//! what a session may speak for, which namespace it reads, and whether a
//! refusal happens before anything durable is written.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionLookup, AdmissionRequest, AdmissionRunContext,
    AdmissionShipContract, ClaimInvocation, ClaimMutation, ExecutionLocation,
};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::TaskStatus;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::command::ToolEntryPoint;
use crate::adapter::tool_host::test_support::{create_context_task, test_runtime};
use crate::application::distributed::{
    DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED, DrainEntryRefusal,
    ensure_distributed_mutation_available, owner_binary_version,
};
use crate::application::workflow::ShipMode;
use crate::runtime::WorkspaceRuntimeBinding;

const FOLLOWER: &str = "hm_follower";
const OWNER: &str = "hm_owner";

/// A follower's session: it reached the owner over SSH and holds `agent`.
/// Its machine label is forwarded attribution, not a credential.
fn follower_session() -> ToolSessionContext {
    ToolSessionContext {
        caller_machine_id: Some(FOLLOWER.to_string()),
        process_machine_id: Some(OWNER.to_string()),
        transport: Some(McpTransport::SshMcp),
        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
        ..ToolSessionContext::default()
    }
}

/// An owner-local MCP session: served by this machine's own server, so it
/// carries the capability that server grants.
///
/// `orbit tool run` builds a different envelope — no capabilities at all, with
/// the caller's authority in the process envelope — which is why the owner-local
/// CLI route is covered end to end by `crates/orbit-cli/tests/tool_list.rs`
/// rather than by a session synthesized here [ORB-12582].
fn owner_local_session() -> ToolSessionContext {
    ToolSessionContext {
        caller_machine_id: Some(OWNER.to_string()),
        process_machine_id: Some(OWNER.to_string()),
        transport: Some(McpTransport::Local),
        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
        ..ToolSessionContext::default()
    }
}

fn operator_session() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Agent, McpCapability::Operator]),
        ..owner_local_session()
    }
}

/// Owner whose resolved ship mode is `local`, matching `workspace init --ship-mode local`.
fn local_ship_runtime() -> (tempfile::TempDir, OrbitRuntime, std::path::PathBuf) {
    let (root, runtime, repo_root) = test_runtime();
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let runtime = OrbitRuntime::from_roots_with_binding(
        &runtime.global_root(),
        &repo_root.join(".orbit"),
        WorkspaceRuntimeBinding {
            logical_workspace_id: workspace_id.clone(),
            task_partition_id: workspace_id,
            owner_machine_id: None,
            repo_root: repo_root.clone(),
            ship_mode: ShipMode::Local,
            base_branch: None,
        },
    )
    .expect("local-ship runtime");
    (root, runtime, repo_root)
}

fn run_as(
    runtime: &OrbitRuntime,
    session: ToolSessionContext,
    tool: &str,
    input: Value,
) -> Result<Value, orbit_common::OrbitError> {
    runtime.run_tool_with_context_and_role(
        tool,
        input,
        Role::Admin,
        ToolContext {
            session_context: session,
            ..ToolContext::default()
        },
    )
}

/// The commit boundary for the runtime's own partition, so a test can author
/// an admission receipt the way the (not yet public) pull path will.
fn boundary(runtime: &OrbitRuntime) -> TaskCommitBoundary {
    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root()))
        .expect("open task registry");
    TaskCommitBoundary::new(
        runtime.sqlite_store().expect("sqlite store"),
        registry,
        runtime.workspace_id().expect("workspace id"),
    )
    .expect("commit boundary")
}

fn admission_request(request_id: &str, caller_version: &str) -> AdmissionRequest {
    AdmissionRequest {
        request_id: request_id.to_string(),
        caller_version: caller_version.to_string(),
        caller_schema: 1,
        caller_review_policy: "none".to_string(),
        run_context: AdmissionRunContext {
            run_id: "drain-1".to_string(),
            job_name: "workspace_auto".to_string(),
            machine_name: Some("laptop".to_string()),
        },
        ship: AdmissionShipContract {
            mode: "pr".to_string(),
            base_branch: "agent-main".to_string(),
            landing_branch: "agent-main".to_string(),
            review_policy: "none".to_string(),
            completion: "review".to_string(),
            authorization_reference: None,
        },
    }
}

/// Admit one task as `machine`, using `owner_version` as the binary both ends
/// ran at the time — which is how a receipt written by an older pair is
/// reproduced without a second binary.
fn admit(
    runtime: &OrbitRuntime,
    repo_root: &Path,
    machine: &str,
    request: &AdmissionRequest,
    owner_version: &str,
) -> AdmissionLookup {
    boundary(runtime)
        .admit_task(
            &AdmissionIdentity::trusted_remote(ExecutionLocation {
                machine_id: machine.to_string(),
                machine_name: None,
            }),
            request,
            owner_version,
            repo_root,
            &runtime.data_root(),
        )
        .expect("admit")
}

fn claim_id(lookup: &AdmissionLookup) -> String {
    match lookup {
        AdmissionLookup::Found { receipt, .. } => {
            receipt.claim.as_ref().expect("claim").claim_id.clone()
        }
        other => panic!("expected an admitted receipt: {other:?}"),
    }
}

#[test]
fn probe_reports_owner_facts_and_creates_no_admission_state() {
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
        malformed.to_string().contains("invalid_input"),
        "{malformed}"
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
    let _env = orbit_common::test_env::unset([
        "ORBIT_MANAGED_RUN_CONTEXT",
        "ORBIT_TASK_ACTOR_KIND",
        "ORBIT_ACTIVITY_TOOLS",
    ]);
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
/// The probe and the receipt lookup are advertised, so MCP reaches them. Claim
/// inspection is not advertised and has no subcommand of its own, which leaves
/// `orbit tool run` as its only route — and that route applies
/// `ensure_tool_agent_facing`, so registering it inactive made the documented
/// operator command fail on every surface. This drives the whole CLI dispatch
/// path, not just the registry, so a registration no entry point can reach
/// cannot land again.
#[test]
fn the_operator_reaches_claim_inspection_through_the_cli_tool_route() {
    let _env = orbit_common::test_env::unset([
        "ORBIT_MANAGED_RUN_CONTEXT",
        "ORBIT_TASK_ACTOR_KIND",
        "ORBIT_ACTIVITY_TOOLS",
    ]);
    let (_root, runtime, _repo_root) = test_runtime();

    let advertised = runtime
        .list_mcp_tool_definitions()
        .expect("mcp definitions")
        .into_iter()
        .map(|definition| definition.schema.name)
        .collect::<BTreeSet<_>>();
    assert!(advertised.contains("orbit.drain.probe"));
    assert!(advertised.contains("orbit.drain.receipt.lookup"));
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
    let _env = orbit_common::test_env::unset([
        "ORBIT_MANAGED_RUN_CONTEXT",
        "ORBIT_TASK_ACTOR_KIND",
        "ORBIT_ACTIVITY_TOOLS",
    ]);
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

/// A backlog task that automatic dispatch would actually offer: assessed
/// complexity, so the readiness assertions below are about admission rather
/// than about the task-pilot preparation gate.
fn admissible_task(runtime: &OrbitRuntime, title: &str, context_files: &[&str]) -> String {
    runtime
        .add_task(crate::application::task::TaskAddParams {
            title: title.to_string(),
            description: "shared-admission fixture".to_string(),
            plan: "fixture".to_string(),
            complexity: orbit_types::task::TaskComplexity::Low,
            context_files: context_files
                .iter()
                .map(|path| (*path).to_string())
                .collect(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add admissible task")
        .id
}

/// [ORB-12500] The retained entry points — an explicit ship, an owner drain,
/// and the independent registry-driven sweep — converge on one admission
/// decision. This is the mixed-entry-point fence: a task a live claim is
/// executing cannot be shipped beside itself, whichever surface asks.
#[test]
fn a_live_claim_fences_every_retained_entry_point_from_the_same_task() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = admissible_task(&runtime, "claimed by a live attempt", &["src/a.rs"]);
    let lookup = admit(
        &runtime,
        &repo_root,
        FOLLOWER,
        &admission_request("claimed-entry", owner_binary_version()),
        owner_binary_version(),
    );
    let claim = claim_id(&lookup);

    // Explicit shipment: the refusal names the claim rather than dispatching a
    // second attempt at the same work.
    let refused = runtime
        .submit_ship_run(
            ShipMode::Pr,
            None,
            std::slice::from_ref(&task),
            crate::application::workflow::CompletionPolicy::Review,
            &[],
            None,
            None,
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect_err("a claimed task is not shipped beside its claim");
    let message = refused.to_string();
    assert!(
        message.contains(&task) && message.contains(&claim),
        "{message}"
    );

    // The shared decision itself, for the surfaces that report rather than
    // raise.
    let decision = runtime
        .drain_entry_admission(
            crate::application::distributed::DrainEntryPoint::ExplicitShip,
            std::slice::from_ref(&task),
            false,
        )
        .expect("shared admission decision");
    assert_eq!(
        decision.refusal.as_ref().map(DrainEntryRefusal::code),
        Some("claimed_by_execution_claim")
    );

    // And automatic discovery reaches the same answer through the claim's
    // frozen footprint rather than through the task's current status.
    let readiness = runtime
        .workspace_auto_readiness(std::slice::from_ref(&task), None, 10, &[])
        .expect("readiness");
    let entry = readiness["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first())
        .cloned()
        .expect("one readiness entry");
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(
        entry["reason"], "not_backlog",
        "admission moved the claimed task out of the backlog: {entry}"
    );
}

/// [ORB-12500] The unattended sweep stands down on a host whose drain slots
/// are occupied — including by a claimed leaf, which the `task_auto_pipeline`
/// history scan it used to run could not see at all. An operator's explicit
/// invocation is not stood down by the same reading: its own leaf definition
/// bounds it.
#[test]
fn claimed_occupancy_stands_the_unattended_sweep_down_but_not_an_explicit_ship() {
    let (_root, runtime, _repo_root) = test_runtime();
    runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_claimed_local_pipeline",
            1,
            chrono::Utc::now(),
            None,
            None,
        )
        .expect("live claimed leaf");

    let sweep = runtime
        .drain_entry_admission(
            crate::application::distributed::DrainEntryPoint::ShipSweep,
            &[],
            true,
        )
        .expect("sweep decision");
    assert_eq!(
        sweep.refusal.as_ref().map(DrainEntryRefusal::code),
        Some("ship_in_flight")
    );
    assert_eq!(sweep.occupancy.occupied, 1);
    assert_eq!(
        sweep.occupancy.for_pipeline("task_claimed_local_pipeline"),
        1
    );

    let explicit = runtime
        .drain_entry_admission(
            crate::application::distributed::DrainEntryPoint::ExplicitShip,
            &[],
            false,
        )
        .expect("explicit decision");
    assert!(explicit.refusal.is_none());
    assert_eq!(explicit.occupancy.occupied, 1);
}

/// [ORB-12968] With a host reboot scheduled, the unattended sweep stands down
/// and names the schedule; an operator's explicit ship and drain are admitted
/// with the schedule reported beside the decision.
#[test]
fn a_scheduled_host_shutdown_stands_unattended_entry_down_but_not_an_explicit_one() {
    use crate::application::distributed::DrainEntryPoint;
    use crate::runtime::host_signal::{FixedHostSignals, ScheduledShutdown};

    let (_root, runtime, _repo_root) = test_runtime();
    let shutdown = ScheduledShutdown {
        mode: "poweroff".to_string(),
        scheduled_at: chrono::Utc::now() + chrono::Duration::hours(1),
        source: "fixture".to_string(),
    };
    let held = runtime.with_host_signal_probe(std::sync::Arc::new(FixedHostSignals::scheduled(
        shutdown.clone(),
    )));

    let sweep = held
        .drain_entry_admission(DrainEntryPoint::ShipSweep, &[], true)
        .expect("sweep decision");
    assert_eq!(
        sweep.refusal.as_ref().map(DrainEntryRefusal::code),
        Some("host_shutdown_scheduled")
    );
    let reason = sweep.refusal.as_ref().map(DrainEntryRefusal::reason);
    assert!(
        reason
            .as_deref()
            .is_some_and(|reason| reason.contains(&shutdown.describe())),
        "{reason:?}"
    );
    assert!(matches!(
        sweep.into_result(),
        Err(orbit_common::OrbitError::PolicyDenied(_))
    ));

    for entry in [DrainEntryPoint::ExplicitShip, DrainEntryPoint::OwnerDrain] {
        let explicit = held
            .drain_entry_admission(entry, &[], false)
            .expect("explicit decision");
        assert!(explicit.refusal.is_none(), "{}", entry.label());
        assert_eq!(explicit.host_shutdown.as_ref(), Some(&shutdown));
    }
}

/// [ORB-12500] A replica serves no owner coordination from any retained entry
/// point. It executes through pull instead, which is the whole point of the
/// role split.
#[test]
fn a_replica_checkout_refuses_every_retained_entry_point() {
    let (_root, runtime, _repo_root) = test_runtime();
    let replica = runtime.with_coordination_write_owner(Some(OWNER.to_string()));
    for entry in [
        crate::application::distributed::DrainEntryPoint::OwnerDrain,
        crate::application::distributed::DrainEntryPoint::ExplicitShip,
        crate::application::distributed::DrainEntryPoint::ShipSweep,
    ] {
        let decision = replica
            .drain_entry_admission(entry, &[], true)
            .expect("replica decision");
        assert_eq!(
            decision.refusal.as_ref().map(DrainEntryRefusal::code),
            Some("replica_checkout"),
            "{} must refuse owner coordination on a replica",
            entry.label()
        );
        assert!(matches!(
            decision.into_result(),
            Err(orbit_common::OrbitError::CapabilityRefused(_))
        ));
    }
}

/// [ORB-12500] A scheduled invocation carries no completion authority, and the
/// shared decision reports the review policy the claim contract admits so no
/// surface has to look it up for itself.
#[test]
fn the_shared_decision_reports_the_claim_contract_verdict() {
    let (_root, runtime, _repo_root) = test_runtime();
    let decision = runtime
        .drain_entry_admission(
            crate::application::distributed::DrainEntryPoint::ShipSweep,
            &[],
            true,
        )
        .expect("sweep decision");
    assert_eq!(decision.review_policy, "none");
    assert!(
        decision.claim_admission_refusal.is_none(),
        "a `none`-policy owner passes the claim contract's ladder: {:?}",
        decision.claim_admission_refusal
    );
    assert!(decision.refusal.is_none(), "an idle host admits");
}

/// [ORB-12500] The surface a legacy wave recomputes for a claimed task cannot
/// drift from the surface the claim froze, because the claim journal refuses
/// every ordinary mutation of a claimed task — including narrowing its
/// `context_files`. That is why the automatic drain needs no separate reading
/// of the claim ledger: its status-derived holder map is already the frozen
/// footprint, and the paths that reach a claimed task directly consult the
/// ledger themselves.
#[test]
fn a_claimed_tasks_declaration_cannot_drift_from_the_footprint_its_claim_froze() {
    let (_root, runtime, repo_root) = test_runtime();
    let claimed = admissible_task(&runtime, "claimed by a live attempt", &["src/a.rs"]);
    admit(
        &runtime,
        &repo_root,
        FOLLOWER,
        &admission_request("frozen-footprint", owner_binary_version()),
        owner_binary_version(),
    );

    let refused = runtime
        .stores()
        .task_records()
        .update(
            &claimed,
            crate::application::task::TaskRecordUpdateParams {
                actor: "test".to_string(),
                context_files: Some(vec!["src/unrelated.rs".to_string()]),
                ..Default::default()
            },
        )
        .expect_err("a claimed task's declaration is not narrowed out from under its attempt");
    assert!(
        refused.to_string().contains("claim"),
        "the refusal names the claim: {refused}"
    );
    assert_eq!(
        runtime.get_task(&claimed).expect("task").context_files,
        vec!["file:src/a.rs".to_string()],
        "the declaration the claim froze is still the declaration on the task"
    );

    // So an overlapping backlog task is withheld by the ordinary status lock,
    // which is that same surface.
    let overlapping = admissible_task(&runtime, "overlaps the frozen surface", &["src/a.rs"]);
    let readiness = runtime
        .workspace_auto_readiness(std::slice::from_ref(&overlapping), None, 10, &[])
        .expect("readiness");
    let entry = readiness["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first())
        .cloned()
        .expect("one readiness entry");
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(entry["reason"], "context_lock_conflict", "{entry}");
    let blockers = entry["conflicts"]
        .as_array()
        .map(|conflicts| {
            conflicts
                .iter()
                .filter_map(|conflict| conflict["locking_task_id"].as_str())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(blockers.contains(&claimed), "{entry}");
}

// ---------------------------------------------------------------------------
// The owner's mutating surface [ORB-13625]: pull, bind and settle, driven
// through the tool boundary a follower's federated session reaches.
// ---------------------------------------------------------------------------

const OTHER_FOLLOWER: &str = "hm_other_follower";

/// A second follower, holding the same `agent` capability. It can reach the
/// owner exactly as the first can; what it cannot do is act on a claim the
/// owner admitted to the first.
fn other_follower_session() -> ToolSessionContext {
    ToolSessionContext {
        caller_machine_id: Some(OTHER_FOLLOWER.to_string()),
        ..follower_session()
    }
}

/// A pull request the way the follower drain builds it: its own identity, and
/// the ship contract the owner's probe reported.
fn pull_input(runtime: &OrbitRuntime, request_id: &str) -> Value {
    let probe = run_as(runtime, follower_session(), "orbit.drain.probe", json!({})).expect("probe");
    json!({
        "request_id": request_id,
        "caller_version": owner_binary_version(),
        "caller_schema": 1,
        "caller_review_policy": "none",
        "run_context": {"run_id": "pull-drain-1", "job_name": "workspace_pull_pipeline"},
        "ship": probe["ship"],
    })
}

fn pulled_claim(runtime: &OrbitRuntime, request_id: &str) -> (Value, String) {
    let response = run_as(
        runtime,
        follower_session(),
        "orbit.task.pull",
        pull_input(runtime, request_id),
    )
    .expect("pull admits");
    let claim_id = response["receipt"]["claim"]["claim_id"]
        .as_str()
        .expect("claim id")
        .to_string();
    (response, claim_id)
}

#[test]
fn a_follower_pull_claims_one_task_for_its_own_machine_and_replays_the_receipt() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);

    let (response, _claim_id) = pulled_claim(&runtime, "req-1");
    let claim = &response["receipt"]["claim"];
    assert_eq!(claim["task_id"], task.id.as_str());
    // The claim is fenced on the session's machine; nothing in the input
    // named one.
    assert_eq!(claim["executed_on"]["machine_id"], FOLLOWER);
    assert_eq!(response["claim_state"], "claimed");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::InProgress
    );

    // A lost answer retried unchanged replays the same admission.
    let replay = run_as(
        &runtime,
        follower_session(),
        "orbit.task.pull",
        pull_input(&runtime, "req-1"),
    )
    .expect("replay");
    assert_eq!(replay["receipt"], response["receipt"]);

    // The same ID with different input is a caller bug, never a second claim.
    let mut changed = pull_input(&runtime, "req-1");
    changed["run_context"]["run_id"] = json!("another-drain");
    let mismatch = run_as(&runtime, follower_session(), "orbit.task.pull", changed)
        .expect_err("changed input refused");
    assert!(
        mismatch.to_string().contains("request_mismatch"),
        "{mismatch}"
    );

    // A fresh request with nothing else ready is idle: a receipt, no claim.
    let idle = run_as(
        &runtime,
        follower_session(),
        "orbit.task.pull",
        pull_input(&runtime, "req-2"),
    )
    .expect("idle is success");
    assert!(idle["receipt"]["claim"].is_null(), "{idle}");
    assert!(idle["claim_state"].is_null());
}

#[test]
fn a_new_request_must_carry_the_ship_contract_the_owner_resolves_now() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);

    let mut stale = pull_input(&runtime, "req-1");
    stale["ship"]["base_branch"] = json!("some-old-base");
    let error = run_as(&runtime, follower_session(), "orbit.task.pull", stale)
        .expect_err("stale contract refused");
    assert!(
        error.to_string().contains("ship_contract_mismatch"),
        "{error}"
    );
    assert!(
        runtime
            .inspect_distributed_claims()
            .expect("claims")
            .is_empty()
    );
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
}

#[test]
fn bind_and_failure_settlement_are_fenced_to_the_admitted_machine_and_run() {
    let (_root, runtime, repo_root) = test_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    let (response, claim_id) = pulled_claim(&runtime, "req-1");
    let ship = response["receipt"]["request"]["ship"].clone();

    // Another machine can reach the owner but cannot start this attempt.
    let foreign = run_as(
        &runtime,
        other_follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "ship": ship}),
    )
    .expect_err("foreign machine refused");
    assert!(!foreign.to_string().is_empty());

    let bound = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "ship": ship}),
    )
    .expect("bind");
    assert_eq!(bound["phase"], "running");
    // A lost bind answer retried is the same bind.
    run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "ship": ship}),
    )
    .expect("bind replay");
    // A second leaf never takes over the claim.
    run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": "leaf-2", "ship": ship}),
    )
    .expect_err("a different run is refused");

    let failure = json!({"Fail": {"summary": "leaf failed", "comment": null, "artifacts": []}});
    run_as(
        &runtime,
        other_follower_session(),
        "orbit.drain.claim.settle",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "settlement": failure}),
    )
    .expect_err("foreign settlement refused");
    let settled = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.settle",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "settlement": failure}),
    )
    .expect("settle failure");
    assert_eq!(settled["phase"], "failed");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Blocked
    );
    run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.settle",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "settlement": failure}),
    )
    .expect("settlement replay returns the recorded outcome");

    let unknown = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": "no-such-claim", "run_id": "leaf-1", "ship": ship}),
    )
    .expect_err("unknown claim");
    assert!(unknown.to_string().contains("stale_claim"), "{unknown}");
}

#[test]
fn settlement_accepts_only_a_handoff_or_a_failure() {
    let (_root, runtime, repo_root) = test_runtime();
    create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    let (_response, claim_id) = pulled_claim(&runtime, "req-1");

    let recover = json!({"Recover": {"status": "backlog", "reason": "self-serve"}});
    let error = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.settle",
        json!({"claim_id": claim_id, "settlement": recover}),
    )
    .expect_err("recovery is an owner-operator action");
    assert!(error.to_string().contains("owner-operator"), "{error}");
}

#[test]
fn a_follower_cannot_hand_off_a_candidate_only_its_own_checkout_holds() {
    use orbit_types::workflow::handoff::{
        HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition, TaskHandoff,
    };
    use orbit_types::workflow::{ReviewTiming, automation::SourceRevision};

    let (_root, runtime, repo_root) = test_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    let (response, claim_id) = pulled_claim(&runtime, "req-1");
    let ship = response["receipt"]["request"]["ship"].clone();
    run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.bind",
        json!({"claim_id": claim_id, "run_id": "leaf-1", "ship": ship}),
    )
    .expect("bind");
    let handoff = TaskHandoff {
        schema_version: 1,
        workspace_id: runtime.workspace_id().expect("workspace"),
        task_id: task.id.clone(),
        claim_id: claim_id.clone(),
        machine_id: FOLLOWER.into(),
        run_id: "leaf-1".into(),
        candidate: HandoffCandidate {
            repository: "owner/repository".into(),
            source_branch: "attempt/leaf-1".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            candidate: SourceRevision {
                commit: "a".repeat(40),
                tree: "b".repeat(40),
            },
            base: SourceRevision {
                commit: "c".repeat(40),
                tree: "d".repeat(40),
            },
            delivery: HandoffDelivery::LocalCandidate,
        },
        review: HandoffReview {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        },
        execution_summary: "Outcome: success".into(),
        validation: Vec::new(),
    };
    let error = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.claim.settle",
        json!({
            "claim_id": claim_id,
            "run_id": "leaf-1",
            "settlement": {"AcceptHandoff": handoff},
        }),
    )
    .expect_err("a remote local candidate is refused");
    assert!(error.to_string().contains("local candidate"), "{error}");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::InProgress
    );
}

#[test]
fn a_replica_serves_no_mutating_entry_point() {
    let (_root, runtime, _repo_root) = test_runtime();
    let input = pull_input(&runtime, "req-1");
    let replica = runtime
        .clone()
        .with_coordination_write_owner(Some(OWNER.to_string()));
    for (tool, input) in [
        ("orbit.task.pull", input),
        (
            "orbit.drain.claim.bind",
            json!({"claim_id": "c", "run_id": "r", "ship": {}}),
        ),
        (
            "orbit.drain.claim.settle",
            json!({"claim_id": "c", "settlement": {"Fail": {"summary": null, "comment": null, "artifacts": []}}}),
        ),
    ] {
        let error = run_as(&replica, follower_session(), tool, input)
            .expect_err("a replica mints no claim");
        assert!(
            matches!(
                error,
                orbit_common::OrbitError::CapabilityRefused(_)
                    | orbit_common::OrbitError::InvalidInput(_)
            ),
            "{tool}: {error}"
        );
    }
    let error = run_as(
        &replica,
        follower_session(),
        "orbit.task.pull",
        pull_input(&runtime, "req-2"),
    )
    .expect_err("replica pull");
    assert!(
        matches!(error, orbit_common::OrbitError::CapabilityRefused(_)),
        "{error}"
    );
}

#[test]
fn a_local_ship_owner_refuses_a_remote_pull_before_admitting() {
    let (_root, runtime, repo_root) = local_ship_runtime();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    let error = run_as(
        &runtime,
        follower_session(),
        "orbit.task.pull",
        pull_input(&runtime, "req-1"),
    )
    .expect_err("followers never execute owner-local work");
    assert!(
        error.to_string().contains("ship_mode_unsupported"),
        "{error}"
    );
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
}

#[test]
fn a_pull_requires_agent_capability_on_the_session() {
    let (_root, runtime, repo_root) = test_runtime();
    create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);
    let input = pull_input(&runtime, "req-1");
    let anonymous = ToolSessionContext {
        effective_capabilities: BTreeSet::new(),
        ..follower_session()
    };
    run_as(&runtime, anonymous, "orbit.task.pull", input).expect_err("unidentified caller");
    assert!(
        runtime
            .inspect_distributed_claims()
            .expect("claims")
            .is_empty()
    );
}

/// The owner's `workflow.distributed_completion` is what its probe reports and
/// what admission pins: `done` names the owner policy as the authorization
/// reference, and a follower still carrying a `review` contract re-probes
/// before a new request is admitted.
#[test]
fn the_owner_completion_policy_reaches_the_probe_and_admission() {
    let (_root, runtime, repo_root) = test_runtime();
    let stale = pull_input(&runtime, "stale-review");
    assert_eq!(stale["ship"]["completion"], "review");
    assert!(stale["ship"]["authorization_reference"].is_null());

    std::fs::write(
        repo_root.join(".orbit/config.toml"),
        "[workflow]\ndistributed_completion = \"done\"\n",
    )
    .expect("owner config");
    let runtime = OrbitRuntime::from_roots(&runtime.global_root(), &repo_root.join(".orbit"))
        .expect("reopen with owner policy");
    create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &["src/a.rs"]);

    let probe =
        run_as(&runtime, follower_session(), "orbit.drain.probe", json!({})).expect("probe");
    assert_eq!(probe["ship"]["completion"], "done");
    assert_eq!(
        probe["ship"]["authorization_reference"],
        crate::application::distributed::OWNER_COMPLETION_POLICY
    );

    let refused = run_as(&runtime, follower_session(), "orbit.task.pull", stale)
        .expect_err("a review contract no longer matches this owner");
    assert!(
        refused.to_string().contains("ship_contract_mismatch"),
        "{refused}"
    );
    let (response, _claim_id) = pulled_claim(&runtime, "fresh-done");
    assert_eq!(response["receipt"]["request"]["ship"]["completion"], "done");
}
