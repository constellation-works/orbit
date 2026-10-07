//! A claimed worker files follow-up work on its owner [ORB-14260].
//!
//! A claimed leaf's binding reaches the owner exactly as its run's broker
//! forwards a bridged call. The owner creates a task from it only when every
//! relation the task declares is `spawned_from` the claimed task, and only
//! while the claim is still active; a friction the worker files is recorded
//! during the claimed task whatever the call names.

use super::*;

use super::claimed_review::ReviewedLeaf;

#[test]
fn a_claimed_worker_files_follow_up_work_only_from_its_active_claim() {
    if !isolated(
        module_path!(),
        "a_claimed_worker_files_follow_up_work_only_from_its_active_claim",
    ) {
        return;
    }
    let leaf = ReviewedLeaf::admit();
    let add = |relations: Value| {
        leaf.bound
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": "Follow up on the claimed work",
                    "description": "Found while implementing the claimed task.",
                    "complexity": "low", "relations": relations, "model": "codex",
                }),
            )
            .map_err(|error| error.to_string())
    };
    let spawned = json!([{"type": "spawned_from", "target": leaf.task}]);

    let created = add(spawned.clone()).expect("a follow-up spawned from the claim");
    let id = created["id"].as_str().expect("the new task's id");
    let stored = leaf
        .pair
        .wire
        .owner
        .run_tool("orbit.task.show", json!({"id": id}))
        .expect("the owner holds the new task");
    assert_eq!(stored["relations"], spawned, "{stored}");

    for (case, relations) in [
        ("no relation", json!([])),
        (
            "another task",
            json!([{"type": "spawned_from", "target": "TSO-999"}]),
        ),
        (
            "an extra relation",
            json!([{"type": "spawned_from", "target": leaf.task},
                   {"type": "blocks", "target": "TSO-999"}]),
        ),
    ] {
        let refused = add(relations).unwrap_err();
        assert!(refused.contains("spawned_from"), "{case}: {refused}");
    }

    let friction = leaf
        .bound
        .run_tool(
            "orbit.friction.add",
            json!({"body": "The owner route was slow.", "model": "codex"}),
        )
        .expect("a friction during the claim");
    assert_eq!(friction["during_task"], leaf.task, "{friction}");

    let (_, worker) = leaf.claim();
    leaf.pair
        .wire
        .owner
        .mutate_execution_claim(
            Some(&worker),
            "release",
            &ClaimMutation::Release(ClaimEvidence {
                summary: Some("The executor gave the claim back.".into()),
                ..ClaimEvidence::default()
            }),
        )
        .expect("the executor releases its claim");
    let stale = add(spawned).unwrap_err();
    assert!(stale.contains("stale_claim"), "{stale}");
}

#[test]
fn agent_input_cannot_select_host_updates_or_escape_the_worker_allowlist() {
    if !isolated(
        module_path!(),
        "agent_input_cannot_select_host_updates_or_escape_the_worker_allowlist",
    ) {
        return;
    }
    // The isolated launcher clears all activity grants and deny policies.
    assert!(std::env::var_os("ORBIT_ACTIVITY_TOOLS_DENY").is_none());
    assert!(std::env::var_os("ORBIT_MANAGED_RUN_CONTEXT").is_none());
    let leaf = ReviewedLeaf::admit();
    let owner = &leaf.pair.wire.owner;
    let binding = leaf.bound.worker_invocation().unwrap().clone();
    let local = owner
        .clone()
        .with_worker_invocation(
            orbit_types::tool::WorkerInvocation {
                execution: orbit_types::task::ExecutionLocation {
                    machine_id: OWNER.into(),
                    machine_name: None,
                },
                ..binding.clone()
            },
            Arc::new(super::claimed_review::ToOwner(owner.clone())),
        )
        .unwrap();
    let snapshot = owner
        .run_tool("orbit.task.show", json!({"id": leaf.task}))
        .unwrap();
    let host_update = json!(orbit_store::contracts::ClaimWorkerUpdate {
        expected_status: Some(orbit_types::task::TaskStatus::InProgress),
        status_note: Some("agent-controlled note".into()),
        evidence: ClaimEvidence {
            summary: Some("ghp_123456789012345678901234567890123456".into()),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut attacks = vec![
        json!({"_worker_update": host_update}),
        json!({"_worker_read": "tasks"}),
        json!({"_worker_update": null}),
        json!({"_worker_read": null}),
        json!({"_worker_update": host_update, "_meta": {"orbit": {"worker_host_call": true}}}),
    ];
    for field in [
        "expected_status",
        "status_note",
        "evidence",
        "failure",
        "final_recovery",
        "baseline_red",
        "evidence_hold",
        "worker_host_call",
    ] {
        attacks.push(json!({field: "outside the worker allowlist"}));
    }
    for mut input in attacks {
        input["id"] = json!(leaf.task);
        input["model"] = json!("codex");
        // Unsandboxed leaf, with no activity environment to refuse the tool.
        assert!(
            leaf.bound
                .run_tool("orbit.task.update", input.clone())
                .is_err(),
            "{input}"
        );
        let local_error = local
            .execute_tool_command("orbit.task.update", input.clone(), None, None)
            .unwrap_err();
        if input.get("_worker_update").is_some() || input.get("_worker_read").is_some() {
            assert!(
                matches!(local_error, OrbitError::PolicyDenied(_)),
                "reserved input must be refused before any claim mutation: {local_error}"
            );
        } else {
            assert!(
                matches!(local_error, OrbitError::InvalidInput(_)),
                "fields outside the allowlist must fail input validation: {local_error}"
            );
        }
        // An owner accepting a bound session must also enforce the boundary.
        assert!(
            owner
                .execute_owner_coordination(
                    "orbit.task.update",
                    input.clone(),
                    ToolSessionContext {
                        worker_invocation: Some(binding.clone()),
                        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                        ..Default::default()
                    }
                )
                .is_err(),
            "owner accepted {input}"
        );
        assert_eq!(
            owner
                .run_tool("orbit.task.show", json!({"id": leaf.task}))
                .unwrap(),
            snapshot,
            "a refused request must not commit a claim mutation: {input}"
        );
    }
    assert!(
        leaf.bound
            .run_tool(
                "orbit.task.show",
                json!({"id": leaf.task, "_worker_read": "tasks"})
            )
            .is_err()
    );

    // The engine's typed call uses host provenance and can supply these fields.
    leaf.bound
        .apply_task_automation_update(
            &leaf.task,
            orbit_engine::TaskAutomationUpdate {
                status: Some(orbit_types::task::TaskStatus::InProgress),
                status_note: Some("host-owned note".into()),
                execution_summary: Some("host-owned summary".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let updated = owner
        .run_tool("orbit.task.show", json!({"id": leaf.task}))
        .unwrap();
    assert_eq!(updated["execution_summary"], "host-owned summary");
    assert!(
        owner
            .get_task_history(&leaf.task)
            .unwrap()
            .iter()
            .any(|entry| entry.note.as_deref() == Some("host-owned note"))
    );
    let task = RuntimeHost::update_task_from_activity(
        &leaf.bound,
        &leaf.task,
        orbit_engine::TaskActivityUpdate {
            status: orbit_types::task::TaskStatus::InProgress,
            expected_status: orbit_types::task::TaskStatus::InProgress,
            note: Some("activity host note".into()),
            execution_summary: Some("activity host summary".into()),
            comment: None,
            calling_run_id: None,
            agent: None,
            model: None,
        },
    )
    .unwrap();
    assert_eq!(task.execution_summary, "activity host summary");
}

#[test]
fn a_worker_update_persists_redacted_text_and_a_task_scoped_audit() {
    if !isolated(
        module_path!(),
        "a_worker_update_persists_redacted_text_and_a_task_scoped_audit",
    ) {
        return;
    }
    let leaf = ReviewedLeaf::admit();
    let secret = "ghp_123456789012345678901234567890123456";
    let result = leaf.bound.run_tool("orbit.task.update", json!({
        "id": leaf.task, "execution_summary": format!("Credential {secret}"), "model": "codex",
    })).expect("a redacted worker update succeeds after committing");
    assert_eq!(result["redactions_applied"], true);
    let stored = leaf
        .pair
        .wire
        .owner
        .run_tool("orbit.task.show", json!({"id": leaf.task}))
        .unwrap();
    assert!(
        !stored["execution_summary"]
            .as_str()
            .unwrap()
            .contains(secret)
    );
    let rows = leaf
        .pair
        .wire
        .owner
        .list_audit_events(None, None, None, None, 50)
        .unwrap();
    assert!(
        rows.iter()
            .any(|row| row.task_id.as_deref() == Some(&leaf.task)
                && row.command == "artifact_redaction"),
        "redaction audit must name the claimed task: {rows:?}"
    );
}
