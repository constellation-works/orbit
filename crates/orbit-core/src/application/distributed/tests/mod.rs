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
use crate::adapter::tool_host::test_support::create_context_task;
use crate::application::distributed::{
    DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED, DrainEntryRefusal,
    ensure_distributed_mutation_available, owner_binary_version,
};
use crate::application::workflow::ShipMode;
use crate::runtime::WorkspaceRuntimeBinding;

mod entry;
mod operator_remedies;
mod probe;
mod serve;

/// Names the one test a re-executed child of this test binary runs in-process.
const ISOLATED_TEST_ENV: &str = "ORBIT_TEST_DISTRIBUTED_FIXTURE_CHILD";

/// Run the calling test's body in a child of this test binary.
///
/// These fixtures write task, claim and run state, and runtime construction
/// reads ambient authority from the process environment: inherited routing can
/// reach the live workspace, and `ORBIT_WORKER_CONTEXT_REQUIRED` refuses a
/// fresh root that holds no worker binding. The child starts with that
/// authority cleared and a disposable `HOME`, `USERPROFILE` and working
/// directory.
///
/// Returns `true` inside the child, where the caller runs its body, and
/// `false` in the parent once the child ran exactly that test and passed.
fn enter_isolated_child(test: &str) -> bool {
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let exact_test = format!("{module}::{test}");
    if std::env::var_os(ISOLATED_TEST_ENV).is_some_and(|name| name == exact_test.as_str()) {
        return true;
    }

    let home = tempfile::tempdir().expect("isolated fixture home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", &exact_test, "--nocapture", "--test-threads=1"])
        .env_remove("ORBIT_WORKER_CONTEXT_REQUIRED")
        .env(ISOLATED_TEST_ENV, &exact_test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .expect("run isolated fixture");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "isolated `{exact_test}` failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "the isolated child must run `{exact_test}` itself, not filter it out:\n{stdout}"
    );
    false
}

/// The shared tool-host fixture, refused outside [`enter_isolated_child`] so a
/// new test cannot silently run with the launching process's authority.
fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, std::path::PathBuf) {
    assert!(
        std::env::var_os(ISOLATED_TEST_ENV).is_some(),
        "mutable distributed fixtures must run through `enter_isolated_child`"
    );
    crate::adapter::tool_host::test_support::test_runtime()
}

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
