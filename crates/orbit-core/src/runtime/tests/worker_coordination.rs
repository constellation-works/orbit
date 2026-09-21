use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use orbit_common::OrbitError;
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_store::{TaskCommitBoundary, contracts::*};
use orbit_tools::{OwnerCoordinator, ToolContext};
use orbit_types::{
    policy::Role,
    task::TaskStatus,
    tool::{McpCapability, ToolSessionContext, WorkerInvocation},
};
use serde_json::{Value, json};

use crate::{
    OrbitRuntime,
    adapter::tool_host::test_support::{create_context_task, test_runtime},
};

struct Owner {
    runtime: OrbitRuntime,
    offline: AtomicBool,
}

impl OwnerCoordinator for Owner {
    fn call(
        &self,
        name: &str,
        mut input: Value,
        mut session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(OrbitError::Execution("owner unavailable".into()));
        }
        if let Some(map) = input.as_object_mut() {
            map.remove("workspace");
        }
        session.process_machine_id = Some("owner".into());
        session.workspace = None;
        self.runtime.run_tool_with_context_and_role(
            name,
            input,
            Role::Admin,
            ToolContext {
                session_context: session,
                ..Default::default()
            },
        )
    }
}

fn isolated_child() -> bool {
    if std::env::var_os("ORBIT_WORKER_FIXTURE_CHILD").is_some() {
        return false;
    }
    let home = tempfile::tempdir().expect("home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.args(["--exact", "runtime::tests::worker_coordination::owner_routing_fences_generic_writes_across_separate_stores", "--nocapture"])
        .env("ORBIT_WORKER_FIXTURE_CHILD", "1").env("HOME", home.path()).env("USERPROFILE", home.path()).output().expect("isolated fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[test]
fn owner_routing_fences_generic_writes_across_separate_stores() {
    if isolated_child() {
        return;
    }
    let (_owner_root, owner_runtime, repo) = test_runtime();
    let (_follower_root, follower, follower_repo) = test_runtime();
    let task = create_context_task(
        &owner_runtime,
        &repo,
        TaskStatus::Backlog,
        &["file:src/work.rs"],
    );
    let shadow = create_context_task(
        &follower,
        &follower_repo,
        TaskStatus::Backlog,
        &["file:src/work.rs"],
    );
    owner_runtime
        .stores()
        .task_documents()
        .update_task_document(
            &task.id,
            TaskDocumentUpdateParams {
                actor: "fixture".into(),
                plan: Some("Implement and validate.".into()),
                ..Default::default()
            },
        )
        .expect("plan");
    let boundary = TaskCommitBoundary::new(
        owner_runtime.sqlite_store().expect("store"),
        TaskRegistryStore::open(&task_registry_path(&owner_runtime.global_root()))
            .expect("registry"),
        owner_runtime.workspace_id().expect("workspace"),
    )
    .expect("boundary");
    let request = AdmissionRequest {
        request_id: "request".into(),
        caller_version: "fixture".into(),
        caller_schema: 1,
        caller_review_policy: "none".into(),
        run_context: AdmissionRunContext {
            run_id: "drain".into(),
            job_name: "auto".into(),
            machine_name: None,
        },
        ship: AdmissionShipContract {
            mode: "pr".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            review_policy: "none".into(),
            completion: "review".into(),
            authorization_reference: None,
        },
    };
    let location = ExecutionLocation {
        machine_id: "executor".into(),
        machine_name: Some("display".into()),
    };
    let AdmissionLookup::Found { receipt, .. } = boundary
        .admit_task(
            &AdmissionIdentity::trusted_remote(location.clone()),
            &request,
            "fixture",
            &repo,
            &owner_runtime.data_root(),
        )
        .expect("admit")
    else {
        panic!("receipt")
    };
    let claim = receipt.claim.expect("claim");
    boundary
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_worker(
                task.id.clone(),
                claim.claim_id.clone(),
                "executor".into(),
                None,
            )),
            "bind",
            &ClaimMutation::Bind {
                run: ClaimRun {
                    machine_id: "executor".into(),
                    run_id: "leaf".into(),
                },
                ship: request.ship.clone(),
            },
        )
        .expect("bind");
    let binding = WorkerInvocation {
        owner_machine_id: "owner".into(),
        owner_workspace_id: owner_runtime.workspace_id().expect("workspace"),
        owner_destination: "owner/workspace".into(),
        task_id: task.id.clone(),
        claim_id: claim.claim_id.clone(),
        execution: location,
        bound_run_id: "leaf".into(),
    };
    let owner = Arc::new(Owner {
        runtime: owner_runtime,
        offline: AtomicBool::new(false),
    });
    let worker = follower
        .clone()
        .with_worker_invocation(binding.clone(), owner.clone())
        .expect("worker");
    assert_eq!(
        worker.get_task(&task.id).expect("owner read").status,
        TaskStatus::InProgress
    );
    assert_eq!(
        follower.get_task(&shadow.id).expect("local shadow").status,
        TaskStatus::Backlog
    );
    let write = json!({"id":task.id, "execution_summary":"Owner evidence", "comment":"Worker comment", "model":"codex"});
    worker
        .run_tool("orbit.task.update", write.clone())
        .expect("owner write");
    worker.run_tool("orbit.task.update", write).expect("retry");
    assert_eq!(
        owner
            .runtime
            .get_task_comments(&task.id)
            .expect("comments")
            .len(),
        1
    );
    assert!(
        follower
            .get_task_comments(&shadow.id)
            .expect("shadow comments")
            .is_empty()
    );
    use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
    RuntimeHost::apply_task_automation_update(
        &worker,
        &task.id,
        TaskAutomationUpdate {
            plan: Some("Owner plan updated by local activity".into()),
            job_run_id: Some("leaf".into()),
            external_refs: vec![orbit_types::task::ExternalRef::github_pr("42").expect("ref")],
            ..Default::default()
        },
    )
    .expect("activity coordination on owner");
    assert_eq!(
        owner.runtime.get_task(&task.id).expect("owner task").plan,
        "Owner plan updated by local activity"
    );
    assert!(
        RuntimeHost::apply_task_automation_update(
            &worker,
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            }
        )
        .expect_err("typed handoff required")
        .to_string()
        .contains("typed handoff")
    );
    let source = follower_repo.join("evidence.json");
    std::fs::write(&source, "{\"fixture\":true}").expect("executor artifact");
    worker
        .run_tool(
            "orbit.task.artifact.put",
            json!({"id":task.id,"source_path":source,"path":"evidence.json","model":"codex"}),
        )
        .expect("executor bytes to owner");
    let manifest = owner
        .runtime
        .get_task_artifact_manifest(&task.id)
        .expect("manifest");
    assert_eq!(manifest.len(), 1);
    assert_eq!(manifest[0].origin.as_ref(), Some(&binding.execution));
    assert_eq!(
        owner
            .runtime
            .get_task(&task.id)
            .expect("link")
            .job_run_machine,
        Some(binding.execution.clone())
    );
    assert!(
        owner
            .runtime
            .stores()
            .jobs()
            .get_job_run("leaf")
            .expect("owner runs")
            .is_none()
    );
    assert!(
        follower
            .get_task_artifacts(&shadow.id)
            .expect("local artifacts")
            .is_empty()
    );
    // The mixed action resolves Git in the executor directory. Its resulting
    // diagnostic comment crosses only the coordination seam to the owner.
    let promotion = orbit_engine::execute_deterministic_action(
        &worker,
        "pr_promote",
        &json!({}),
        &json!({
            "workspace_path": follower_repo, "run_id": "leaf", "completed_task_ids": [task.id],
            "pr_number": "42", "base": "missing-fixture-base"
        }),
        false,
        &Default::default(),
        None,
    );
    assert!(promotion.is_err());
    let comments = owner
        .runtime
        .get_task_comments(&task.id)
        .expect("promotion diagnostics on owner");
    assert!(comments.iter().any(|comment| {
        comment.message.contains("[phase=obsolete-base]")
            && comment
                .message
                .contains(follower_repo.to_str().expect("fixture path"))
    }));
    assert!(
        follower
            .get_task_comments(&shadow.id)
            .expect("no local promotion write")
            .is_empty()
    );
    let mut mismatch = binding.clone();
    mismatch.owner_workspace_id = "wrong-workspace".into();
    assert!(
        follower
            .clone()
            .with_worker_invocation(mismatch, owner.clone())
            .expect("seed mismatched destination")
            .get_task(&task.id)
            .is_err()
    );
    assert!(
        worker
            .clone()
            .with_worker_invocation(
                WorkerInvocation {
                    bound_run_id: "replacement".into(),
                    ..binding.clone()
                },
                owner.clone()
            )
            .is_err()
    );
    let friction = json!({"body":"Observed an isolated failure", "model":"codex"});
    let first = worker
        .run_tool("orbit.friction.add", friction.clone())
        .expect("friction without task");
    assert_eq!(
        first,
        worker
            .run_tool("orbit.friction.add", friction.clone())
            .expect("friction retry")
    );
    assert!(
        worker
            .run_tool(
                "orbit.friction.add",
                json!({"body":"conflict", "during_task":"different", "model":"codex"})
            )
            .is_err()
    );
    assert!(
        worker
            .run_tool(
                "orbit.task.update",
                json!({"id":task.id,"claim_id":"forged","comment":"no","model":"codex"})
            )
            .is_err()
    );
    let mut elevated = ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into(),
        ..Default::default()
    };
    worker.bind_worker_session(&mut elevated).expect("session");
    assert!(
        !elevated
            .effective_capabilities
            .contains(&McpCapability::Operator)
    );
    owner.offline.store(true, Ordering::SeqCst);
    assert!(worker.get_task(&task.id).is_err());
    RuntimeHost::record_event(
        &worker,
        orbit_types::record::OrbitEvent::PolicyDenied {
            tool: "fixture-local-diagnostic".into(),
        },
    )
    .expect("local diagnostics during owner outage");
    assert!(
        !follower
            .list_session_events(10)
            .expect("executor log")
            .is_empty()
    );
    assert!(
        worker
            .run_tool(
                "orbit.task.update",
                json!({"id":task.id,"comment":"offline","model":"codex"})
            )
            .is_err()
    );
    owner.offline.store(false, Ordering::SeqCst);
    boundary
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_operator(
                task.id.clone(),
                claim.claim_id,
                "operator".into(),
            )),
            "recover",
            &ClaimMutation::Recover {
                status: TaskStatus::Backlog,
                reason: "deliberate recovery".into(),
            },
        )
        .expect("recover");
    let mut next_request = request.clone();
    next_request.request_id = "reassigned".into();
    let AdmissionLookup::Found {
        receipt: next_receipt,
        ..
    } = boundary
        .admit_task(
            &AdmissionIdentity::trusted_remote(binding.execution.clone()),
            &next_request,
            "fixture",
            &repo,
            &owner.runtime.data_root(),
        )
        .expect("reassign")
    else {
        panic!("new claim")
    };
    let next_claim = next_receipt.claim.expect("reassigned claim");
    assert_ne!(next_claim.claim_id, binding.claim_id);
    boundary
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_worker(
                task.id.clone(),
                next_claim.claim_id.clone(),
                "executor".into(),
                None,
            )),
            "new-bind",
            &ClaimMutation::Bind {
                run: ClaimRun {
                    machine_id: "executor".into(),
                    run_id: "new-leaf".into(),
                },
                ship: next_request.ship,
            },
        )
        .expect("new run");
    for mutation in [
        json!({"execution_summary":"stale summary"}),
        json!({"artifacts":[{"path":"stale.txt","media_type":"text/plain","content":[120]}]}),
        json!({"status":"review"}),
        json!({"plan":"stale plan"}),
    ] {
        let mut input = mutation;
        input["id"] = json!(task.id);
        input["model"] = json!("codex");
        assert!(worker.run_tool("orbit.task.update", input).is_err());
    }
    assert!(
        worker
            .run_tool(
                "orbit.task.artifact.put",
                json!({"id":task.id,"source_path":source,"path":"stale.txt","model":"codex"})
            )
            .expect_err("stale artifact reaches owner fence")
            .to_string()
            .contains("stale_claim")
    );
    assert!(
        RuntimeHost::apply_task_automation_update(
            &worker,
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        worker
            .run_tool(
                "orbit.task.update",
                json!({"id":task.id,"comment":"stale","model":"codex"})
            )
            .is_err()
    );
    assert!(
        worker
            .run_tool(
                "orbit.friction.add",
                json!({"body":"stale friction","model":"codex"})
            )
            .is_err()
    );
    assert_eq!(
        follower
            .get_task(&shadow.id)
            .expect("shadow preserved")
            .status,
        TaskStatus::Backlog
    );
}

/// A managed child names its requirement in the environment; the binding
/// itself is host-only. When no binding resolves -- including when a `/proc`
/// probe is denied and the resolver reports "unbound" -- a required child
/// still refuses to start, while an ordinary process opens unbound.
#[test]
fn a_required_worker_context_fails_closed_when_no_binding_resolves() {
    use crate::runtime::worker_coordination::restore_process_binding;
    // The requirement is set on the child's command, never on this process:
    // a process-wide variable would fail every sibling runtime open closed.
    if let Some(root) = std::env::var_os("ORBIT_REQUIRED_CONTEXT_ROOT") {
        let refused = restore_process_binding(std::path::Path::new(&root))
            .expect_err("a required worker context has no binding to restore");
        assert!(
            matches!(refused, OrbitError::PolicyDenied(_)),
            "{refused:?}"
        );
        return;
    }
    let root = tempfile::tempdir().expect("global root");
    let unbound = {
        let _env = orbit_common::test_env::unset(["ORBIT_WORKER_CONTEXT_REQUIRED"]);
        restore_process_binding(root.path()).expect("an unbound process opens")
    };
    assert!(unbound.is_none());
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.args(["--exact", "runtime::tests::worker_coordination::a_required_worker_context_fails_closed_when_no_binding_resolves", "--nocapture"])
        .env("HOME", root.path()).env("USERPROFILE", root.path())
        .env("ORBIT_REQUIRED_CONTEXT_ROOT", root.path())
        .env("ORBIT_WORKER_CONTEXT_REQUIRED", "1")
        .output().expect("required-context child");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
