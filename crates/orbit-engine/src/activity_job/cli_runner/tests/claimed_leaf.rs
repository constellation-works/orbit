//! Owner task tools inside a claimed leaf.
//!
//! A claimed leaf runs another machine's task, so every agent it launches
//! must lose the owner-routed task tools, not only the implementer that is
//! handed `claimed: true`. The recovery hooks the executor dispatches for a
//! failed leaf step get no such input, so the trusted worker binding on the
//! host is what marks them.

use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::AuditSink;
use orbit_types::workflow::activity_job::ActivityToolPolicyMode;
use serde_json::json;
use tempfile::tempdir;

use super::super::super::audit_writer::V2AuditWriter;
use super::super::run_cli_backend;
use super::orchestrator_env::{delegated_policy, policy_checking_script, policy_test_host};
use super::test_support::{RecordingSink, TestHost, test_agent_loop_spec_for};

/// Activities a claimed leaf can run as an agent besides the implementer: the
/// recovery hooks of its steps.
const CLAIMED_LEAF_RECOVERY_ACTIVITIES: &[&str] =
    &["step_failure_recovery", "pr_conflict_recovery"];

fn worker_bound_host(script: &std::path::Path) -> TestHost {
    let mut host = policy_test_host(script, &[]);
    if let Some(context) = host.task_context.as_mut() {
        context["worker_bound"] = json!(true);
    }
    host
}

fn audit(run_id: &str) -> Arc<V2AuditWriter> {
    Arc::new(V2AuditWriter::new(
        run_id,
        "grok:grok-build",
        Arc::new(RecordingSink::default()) as Arc<dyn AuditSink>,
    ))
}

fn deny_spec() -> orbit_types::workflow::activity_job::AgentLoopSpec {
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.tool_disallow_list = Some(vec![
        "orbit.workflow.ship".to_string(),
        "proc.*".to_string(),
    ]);
    spec
}

/// Recovery input as the executor builds it for run `run_id`: it names the
/// failed target's tasks and run, and has no `claimed` key.
fn recovery_input(run_id: &str) -> serde_json::Value {
    json!({"prompt": "hi", "task_id": "ORB-13315", "task_ids": ["ORB-13315"], "run_id": run_id})
}

/// The child's own checks ran and passed. Conflict recovery is the exception:
/// it refuses to spawn without a real stopped rebase in a linked worktree,
/// which this policy test does not build. It resolves and records its tool
/// set before that refusal, and the audit event is what the callers assert.
fn assert_child_ran_unless_conflict_recovery(
    activity: &str,
    result: Result<
        crate::activity_job::dispatcher::DispatchOutcome,
        crate::activity_job::dispatcher::DispatchError,
    >,
    failure: &str,
) {
    if activity == "pr_conflict_recovery" {
        return;
    }
    let outcome = result.expect("run succeeds");
    assert!(
        outcome.success,
        "{activity} {failure}: {:?}",
        outcome.output
    );
}

#[test]
fn recovery_agents_in_a_claimed_leaf_lose_owner_task_tools_without_a_claimed_input() {
    for activity in CLAIMED_LEAF_RECOVERY_ACTIVITIES {
        let temp = tempdir().expect("tempdir");
        let script = policy_checking_script(
            temp.path(),
            r#"[ "$ORBIT_ACTIVITY_TOOLS_DENY" = "orbit.workflow.ship,proc.*,orbit.task.show,orbit.task.update" ] || fail owner_task_tools_not_denied
[ "$ORBIT_ACTIVITY_TOOLS" = "orbit.search,github.run.list" ] || fail owner_task_tools_still_callable"#,
        );
        let audit = audit("job-claimed-recovery");

        let result = run_cli_backend(
            &worker_bound_host(&script),
            &deny_spec(),
            activity,
            "job-claimed-recovery",
            audit.clone(),
            &recovery_input("job-claimed-recovery"),
            None,
        );
        assert_child_ran_unless_conflict_recovery(activity, result, "kept owner task tools");
        let (effective_tools, tool_policy, disallow_list) = delegated_policy(&audit);
        assert_eq!(effective_tools, ["orbit.search", "github.run.list"]);
        assert_eq!(tool_policy, Some(ActivityToolPolicyMode::Deny));
        let disallow_list = disallow_list.expect("deny list recorded");
        for tool in ["orbit.task.show", "orbit.task.update"] {
            assert!(
                disallow_list.iter().any(|denied| denied == tool),
                "{activity} did not deny {tool}: {disallow_list:?}"
            );
        }
    }
}

#[test]
fn recovery_agents_outside_a_claim_keep_their_task_tools() {
    for activity in CLAIMED_LEAF_RECOVERY_ACTIVITIES {
        let temp = tempdir().expect("tempdir");
        let script = policy_checking_script(
            temp.path(),
            r#"[ "$ORBIT_ACTIVITY_TOOLS_DENY" = "orbit.workflow.ship,proc.*" ] || fail deny_list_widened_outside_claim
[ "$ORBIT_ACTIVITY_TOOLS" = "orbit.task.show,orbit.search,github.run.list" ] || fail task_tools_removed_outside_claim"#,
        );
        let audit = audit("job-unclaimed-recovery");

        let result = run_cli_backend(
            &policy_test_host(&script, &[]),
            &deny_spec(),
            activity,
            "job-unclaimed-recovery",
            audit.clone(),
            &recovery_input("job-unclaimed-recovery"),
            None,
        );
        assert_child_ran_unless_conflict_recovery(activity, result, "lost task tools");
        let (effective_tools, _, _) = delegated_policy(&audit);
        assert_eq!(
            effective_tools,
            ["orbit.task.show", "orbit.search", "github.run.list"]
        );
    }
}

/// The implementer is guarded by the binding too, so a claimed pipeline that
/// stopped passing `claimed: true` would still lose the tools.
#[test]
fn implementer_in_a_claimed_leaf_loses_owner_task_tools_without_a_claimed_input() {
    let temp = tempdir().expect("tempdir");
    let script = policy_checking_script(
        temp.path(),
        r#"[ "$ORBIT_ACTIVITY_TOOLS" = "orbit.search" ] || fail owner_task_tools_still_allowed"#,
    );
    let audit = audit("job-claimed-implement");
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.tools = vec![
        "orbit.task.show".to_string(),
        "orbit.task.update".to_string(),
        "orbit.search".to_string(),
    ];

    let outcome = run_cli_backend(
        &worker_bound_host(&script),
        &spec,
        "agent_implement",
        "job-claimed-implement",
        audit.clone(),
        &recovery_input("job-claimed-implement"),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success, "{:?}", outcome.output);
    let (effective_tools, tool_policy, _) = delegated_policy(&audit);
    assert_eq!(effective_tools, ["orbit.search"]);
    assert_eq!(tool_policy, Some(ActivityToolPolicyMode::Allow));
}
