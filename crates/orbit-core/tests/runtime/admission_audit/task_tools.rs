use orbit_engine::RuntimeHost;
use orbit_types::task::TaskStatus;
use orbit_types::tool::ToolSessionContext;
use serde_json::json;

use crate::admission_audit::tool_support::{create_task, test_runtime};

/// End-to-end coverage for the artifact read surface: attach through the
/// canonical put tool, list compact metadata, then retrieve the payload.
mod artifact_get {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use orbit_types::task::TaskStatus;
    use serde_json::{Value, json};

    use crate::admission_audit::tool_support::{create_task, run_tool_as_operator, test_runtime};
    use orbit_core::OrbitRuntime;

    fn attach(runtime: &OrbitRuntime, task_id: &str, path: &str, content: Vec<u8>) {
        let source = runtime.paths().repo_root.join(format!(
            "orbit-artifact-fixture-{}-{}",
            std::process::id(),
            path.replace('/', "_"),
        ));
        std::fs::write(&source, &content).expect("write artifact fixture");
        run_tool_as_operator(
            runtime,
            "orbit.task.artifact.put",
            json!({
                "id": task_id,
                "source_path": source.to_string_lossy(),
                "path": path,
                "model": "codex",
            }),
        )
        .expect("attach artifact");
        std::fs::remove_file(&source).ok();
    }

    fn get(runtime: &OrbitRuntime, task_id: &str, path: &str) -> Value {
        run_tool_as_operator(
            runtime,
            "orbit.task.artifact.get",
            json!({"id": task_id, "path": path}),
        )
        .expect("read artifact")
    }

    fn seeded_task(runtime: &OrbitRuntime, repo_root: &std::path::Path) -> String {
        create_task(
            runtime,
            repo_root,
            "artifact fixture",
            "holds synthetic artifacts",
            TaskStatus::InProgress,
            &[],
        )
        .id
        .to_string()
    }

    #[test]
    fn svg_stays_a_download_and_is_never_classified_as_a_viewable_image() {
        if !crate::admission_audit::isolate(
            module_path!(),
            "svg_stays_a_download_and_is_never_classified_as_a_viewable_image",
        ) {
            return;
        }

        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>";
        attach(&runtime, &id, "diagrams/active.svg", svg.to_vec());

        let read = get(&runtime, &id, "diagrams/active.svg");
        assert_eq!(read["media_type"], "image/svg+xml");
        assert_eq!(
            read["presentation"], "opaque",
            "SVG carries active content and must never be handed to a renderer"
        );
        // Still fully retrievable — fail-closed is about rendering, not access.
        let decoded = BASE64_STANDARD
            .decode(read["content_base64"].as_str().expect("base64 payload"))
            .expect("payload decodes");
        assert_eq!(decoded, svg.to_vec());
    }

    #[test]
    fn traversal_and_absolute_paths_are_refused_before_any_read() {
        if !crate::admission_audit::isolate(
            module_path!(),
            "traversal_and_absolute_paths_are_refused_before_any_read",
        ) {
            return;
        }

        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);

        for path in [
            "../../etc/passwd",
            "/etc/passwd",
            "notes/../../escape.txt",
            "./notes.txt",
            r"notes\escape.txt",
        ] {
            let error = run_tool_as_operator(
                &runtime,
                "orbit.task.artifact.get",
                json!({"id": id, "path": path}),
            )
            .expect_err("traversal must be refused");
            assert!(
                matches!(error, orbit_common::OrbitError::InvalidInput(_)),
                "{path} should be rejected as invalid input, got {error}"
            );
        }
    }
}

fn task_artifacts_retain_trusted_local_provenance_and_reject_ssh_mcp_attribution() {
    use orbit_tools::ToolContext;
    use orbit_types::tool::{McpCapability, McpTransport};
    use std::collections::BTreeSet;

    let (_root, runtime, repo_root) = test_runtime();
    let runtime = runtime.with_automation_machine_identity(Some("local-runtime-box".into()));
    let task = create_task(
        &runtime,
        &repo_root,
        "artifact provenance task",
        "verifies trusted provenance retention",
        TaskStatus::InProgress,
        &[],
    );

    // 1. Local session with process identity records local process provenance.
    let local_source = repo_root.join("local-proc.txt");
    std::fs::write(&local_source, "from local process").expect("write local fixture");
    let local_process_session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        process_machine_id: Some("worker-proc-1".into()),
        process_machine_name: Some("worker-host-1".into()),
        caller_machine_id: Some("untrusted-caller-box".into()),
        caller_machine_name: Some("untrusted-caller-host".into()),
        ..Default::default()
    };
    runtime
        .run_tool_with_context(
            "orbit.task.artifact.put",
            json!({
                "id": task.id,
                "source_path": local_source.to_string_lossy(),
                "path": "reports/local-proc.txt",
            }),
            ToolContext {
                session_context: local_process_session,
                cwd: Some(repo_root.to_string_lossy().to_string()),
                ..ToolContext::default()
            },
        )
        .expect("local process artifact put");

    // 2. Local session without process identity falls back to trusted runtime identity.
    let runtime_source = repo_root.join("local-runtime.txt");
    std::fs::write(&runtime_source, "from local runtime").expect("write runtime fixture");
    let local_runtime_session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        process_machine_id: None,
        process_machine_name: Some("worker-host-1".into()),
        caller_machine_id: Some("untrusted-caller-box".into()),
        ..Default::default()
    };
    runtime
        .run_tool_with_context(
            "orbit.task.artifact.put",
            json!({
                "id": task.id,
                "source_path": runtime_source.to_string_lossy(),
                "path": "reports/local-runtime.txt",
            }),
            ToolContext {
                session_context: local_runtime_session,
                cwd: Some(repo_root.to_string_lossy().to_string()),
                ..ToolContext::default()
            },
        )
        .expect("local runtime artifact put");

    // 3. SSH MCP session carries self-asserted caller labels and destination process id;
    // preloaded payload is accepted, but origin remains None.
    let ssh_session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        transport: Some(McpTransport::SshMcp),
        process_machine_id: Some("destination-machine".into()),
        process_machine_name: Some("destination-host".into()),
        caller_machine_id: Some("remote-spoke-box".into()),
        caller_machine_name: Some("remote-spoke-host".into()),
        ..Default::default()
    };
    runtime
        .run_tool_with_context(
            "orbit.task.artifact.put",
            json!({
                "id": task.id,
                "artifacts": [{
                    "path": "reports/ssh-remote.txt",
                    "media_type": "text/plain",
                    "content": "from ssh remote",
                }],
            }),
            ToolContext {
                session_context: ssh_session,
                cwd: Some(repo_root.to_string_lossy().to_string()),
                ..ToolContext::default()
            },
        )
        .expect("ssh mcp artifact put");

    let manifest = runtime
        .get_task_artifact_manifest(&task.id)
        .expect("artifact manifest");

    let proc_art = manifest
        .iter()
        .find(|a| a.path == "reports/local-proc.txt")
        .expect("local proc artifact");
    assert_eq!(
        proc_art.origin,
        Some(orbit_types::task::ExecutionLocation {
            machine_id: "worker-proc-1".into(),
            machine_name: Some("worker-host-1".into()),
        })
    );

    let runtime_art = manifest
        .iter()
        .find(|a| a.path == "reports/local-runtime.txt")
        .expect("local runtime artifact");
    assert_eq!(
        runtime_art.origin,
        Some(orbit_types::task::ExecutionLocation {
            machine_id: "local-runtime-box".into(),
            machine_name: Some("worker-host-1".into()),
        })
    );

    let remote_art = manifest
        .iter()
        .find(|a| a.path == "reports/ssh-remote.txt")
        .expect("remote artifact");
    assert_eq!(remote_art.origin, None);
}

/// Coverage evidence is read only after its action stops, when nobody can fix
/// it, so malformed coverage must name the field, type and template recovery
/// at put and store nothing; well-formed evidence bytes remain unchanged.
fn artifact_put_refuses_malformed_coverage_evidence_with_the_parse_error() {
    use crate::admission_audit::tool_support::run_tool_as_operator;

    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "coverage evidence task",
        "receives automation coverage evidence",
        TaskStatus::InProgress,
        &[],
    );
    let put = |content: &str| {
        let source = repo_root.join("automation-coverage-fixture.json");
        std::fs::write(&source, content).expect("write coverage fixture");
        let result = run_tool_as_operator(
            &runtime,
            "orbit.task.artifact.put",
            json!({
                "id": task.id,
                "source_path": source.to_string_lossy(),
                "path": "automation-coverage.json",
                "model": "codex",
            }),
        );
        std::fs::remove_file(&source).ok();
        result
    };

    let revision = json!({"commit": "c1", "tree": "t1"});
    let evidence = json!({
        "schema_version": 2,
        "batch_id": "batch",
        "consumer": "consumer",
        "epoch": "epoch",
        "input_digest": "digest",
        "action_id": task.id,
        "attempt": 1,
        "coverage": "landed_code_review_v1",
        "from_exclusive": revision,
        "through_inclusive": revision,
        "examined_commits": ["c1"],
        "examined_deliveries": ["pr:owner/repo:1"],
        "examination_complete": true,
        "delivery_examinations": [{
            "delivery": "pr:owner/repo:1",
            "examined_paths": ["src/lib.rs"],
            "skipped_paths": [],
            "verdict": {"findings": ["finding-id"]},
            "rationale": "Reviewed the complete changed path and filed the observed defect.",
        }],
        "checks": [{"subject": "range", "method": "review", "observation": "examined"}],
        "findings": ["finding-id"],
    });
    // Audit excerpts do not reveal which string contained a map. Exercise
    // identity, list and nested check fields so the next refusal records it.
    for (pointer, field) in [
        ("/batch_id", "batch_id"),
        ("/examined_commits/0", "examined_commits[0]"),
        ("/examined_deliveries/0", "examined_deliveries[0]"),
        ("/checks/0/observation", "checks[0].observation"),
        ("/findings/0", "findings[0]"),
        (
            "/delivery_examinations/0/verdict/findings/0",
            "delivery_examinations[0].verdict.findings[0]",
        ),
    ] {
        let mut malformed = evidence.clone();
        *malformed.pointer_mut(pointer).expect("fixture field") = json!({"id": "wrong shape"});
        let error = put(&malformed.to_string()).expect_err("map in a string field");
        let orbit_common::OrbitError::InvalidInput(message) = error else {
            panic!("expected invalid input, got {error}");
        };
        for expected in [
            field,
            "invalid type: map, expected a string",
            "assigned task",
            "evidence template",
            "re-put",
        ] {
            assert!(
                message.contains(expected),
                "artifact refusal must identify the field, type and recovery ({expected}): {message}"
            );
        }
        assert!(
            runtime
                .get_task_artifact(&task.id, "automation-coverage.json")
                .expect("read artifact")
                .is_none(),
            "invalid coverage must be refused before attachment: {field}"
        );
    }
    // Wrapping serde must preserve unknown-field and trailing-input refusals.
    let mut unknown = evidence.clone();
    unknown["extra"] = json!(true);
    for malformed in [unknown.to_string(), format!("{evidence} {{}}"), "{".into()] {
        put(&malformed).expect_err("strict JSON evidence parsing");
        assert!(
            runtime
                .get_task_artifact(&task.id, "automation-coverage.json")
                .expect("read artifact")
                .is_none()
        );
    }
    let content = evidence.to_string();
    put(&content).expect("well-formed completed evidence is stored");
    assert_eq!(
        runtime
            .get_task_artifact(&task.id, "automation-coverage.json")
            .expect("read artifact")
            .expect("accepted coverage")
            .content,
        content.as_bytes(),
        "valid evidence bytes must be preserved unchanged"
    );
    // Old receipts still decode; acceptance/settlement owns schema-version
    // policy, not the artifact parser.
    let mut legacy = evidence;
    legacy["schema_version"] = json!(1);
    legacy
        .as_object_mut()
        .expect("object")
        .remove("delivery_examinations");
    put(&legacy.to_string()).expect("legacy evidence remains parseable");
}

fn seed_open_delivery_batch_attempt(
    runtime: &orbit_core::OrbitRuntime,
    machine_id: &str,
    task_id: &str,
) -> orbit_types::workflow::automation::BatchAttempt {
    use chrono::Utc;
    use orbit_types::workflow::automation::{
        AutomationState, BatchAttempt, BatchState, CoverageBatch, CoverageClass, Delivery,
        SourceRevision,
    };

    let now = Utc::now();
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let consumer = format!("{machine_id}/{workspace_id}/auto-task/delivery-code-review");
    let revision = SourceRevision {
        commit: "c1".into(),
        tree: "t1".into(),
    };
    let delivery = Delivery {
        key: "pr:owner/repo:1".into(),
        repository: "owner/repo".into(),
        branch: "main".into(),
        before: revision.clone(),
        after: revision.clone(),
        commits: vec!["c1".into()],
        task_ids: vec![],
        unattributed: None,
        evidence_reference: "pull:1".into(),
        evidence_digest: "digest".into(),
        landed_at: now,
    };
    let batch = CoverageBatch {
        schema_version: 1,
        id: "batch-open".into(),
        consumer: consumer.clone(),
        epoch: "epoch".into(),
        repository: "owner/repo".into(),
        branch: "main".into(),
        coverage: CoverageClass::LandedCodeReviewV1,
        from_exclusive: revision.clone(),
        through_inclusive: revision.clone(),
        commits: vec!["c1".into()],
        deliveries: vec![delivery.clone()],
        exclusions: vec![],
        created_at: now,
        max_attempts: 2,
        retry_until: now + chrono::Duration::hours(4),
    };
    let attempt = BatchAttempt {
        batch,
        input_digest: "digest".into(),
        attempt: 1,
        action_key: "automation:batch-open:1".into(),
        action_id: Some(task_id.into()),
        state: BatchState::Admitted,
        reason: None,
        retry_after: None,
        reissue: None,
    };
    let baseline = AutomationState {
        members: None,
        consumer,
        epoch: "epoch".into(),
        trigger: None,
        repository: "owner/repo".into(),
        branch: "main".into(),
        generation: 0,
        baseline: revision.clone(),
        observed: revision.clone(),
        covered: revision,
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        lookup_retries: Default::default(),
        active: None,
        stall: None,
    };
    let mut admitted = baseline.clone();
    admitted.generation = 1;
    admitted.pending_commits = vec!["c1".into()];
    admitted.pending = vec![delivery];
    admitted.active = Some(attempt.clone());
    let store = runtime.automation_store().expect("automation store");
    assert!(store.automation_initialize(&baseline).expect("init state"));
    assert!(
        store
            .automation_commit(&baseline, &admitted, None)
            .expect("admit attempt")
    );
    attempt
}

fn delivery_coverage_write_refuses_outside_own_orbit_run_and_accepts_own_run() {
    use crate::admission_audit::tool_support::run_tool_as_operator;
    use orbit_engine::TaskAutomationUpdate;
    use orbit_tools::{ReservationOwnerContext, ToolContext};
    use orbit_types::tool::McpCapability;
    use orbit_types::workflow::automation::{
        COVERAGE_ARTIFACT, EVIDENCE_AUTHORITY_ARTIFACT, ExaminationCheck, evidence_template,
    };
    use std::collections::BTreeSet;

    let (_root, runtime, repo_root) = test_runtime();
    let machine_id = "local-machine";
    let runtime = runtime.with_automation_machine_identity(Some(machine_id.into()));
    let task = create_task(
        &runtime,
        &repo_root,
        "delivery review task",
        "owes delivery coverage",
        TaskStatus::InProgress,
        &[],
    );
    let attempt = seed_open_delivery_batch_attempt(&runtime, machine_id, &task.id);

    let own_run = RuntimeHost::insert_job_run(
        &runtime,
        "task_local_pipeline",
        1,
        chrono::Utc::now(),
        Some(json!({ "task_id": task.id })),
        None,
    )
    .expect("insert own run");
    let other_run = RuntimeHost::insert_job_run(
        &runtime,
        "task_local_pipeline",
        1,
        chrono::Utc::now(),
        Some(json!({ "task_id": "ORB-99999" })),
        None,
    )
    .expect("insert other run");
    RuntimeHost::apply_task_automation_update(
        &runtime,
        &task.id,
        TaskAutomationUpdate {
            job_run_id: Some(own_run.run_id.clone()),
            ..TaskAutomationUpdate::default()
        },
    )
    .expect("bind task run");

    let mut coverage = evidence_template(&attempt);
    coverage.examination_complete = true;
    coverage.checks = vec![ExaminationCheck {
        subject: "src/lib.rs".into(),
        method: "read diff".into(),
        observation: "checked change".into(),
    }];
    coverage.delivery_examinations[0].examined_paths = vec!["src/lib.rs".into()];
    coverage.delivery_examinations[0].rationale =
        "Reviewed the delivered file completely and confirmed no issues.".into();
    let payload = serde_json::to_string(&coverage).expect("serialize coverage");
    let source = repo_root.join("automation-coverage-fixture.json");
    std::fs::write(&source, &payload).expect("write coverage fixture");

    let put_input = json!({
        "id": task.id,
        "source_path": source.to_string_lossy(),
        "path": COVERAGE_ARTIFACT,
        "model": "codex",
    });

    // 1. External session (no Orbit run) is refused before storing anything.
    let external_err = run_tool_as_operator(&runtime, "orbit.task.artifact.put", put_input.clone())
        .expect_err("external coverage write must be refused");
    assert!(
        matches!(external_err, orbit_common::OrbitError::PolicyDenied(_)),
        "expected PolicyDenied, got {external_err}"
    );
    let external_msg = external_err.to_string();
    assert!(
        external_msg.contains("Orbit run")
            && external_msg.contains(&format!("orbit run ship {}", task.id)),
        "refusal must name the task's Orbit run and `orbit run ship <task-id>`: {external_msg}"
    );
    assert!(
        runtime
            .get_task_artifact(&task.id, COVERAGE_ARTIFACT)
            .expect("read artifact")
            .is_none()
    );

    // 2. A different Orbit run is also refused.
    let other_err = runtime
        .run_tool_with_context(
            "orbit.task.artifact.put",
            put_input.clone(),
            ToolContext {
                session_context: ToolSessionContext {
                    effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                    ..ToolSessionContext::default()
                },
                reservation_owner: Some(ReservationOwnerContext {
                    owner_run_id: other_run.run_id,
                    owner_metadata: None,
                }),
                ..ToolContext::default()
            },
        )
        .expect_err("other run coverage write must be refused");
    assert!(
        matches!(other_err, orbit_common::OrbitError::PolicyDenied(_)),
        "expected PolicyDenied, got {other_err}"
    );
    let other_msg = other_err.to_string();
    assert!(
        other_msg.contains("Orbit run")
            && other_msg.contains(&format!("orbit run ship {}", task.id)),
        "refusal must name the task's Orbit run and `orbit run ship <task-id>`: {other_msg}"
    );
    assert!(
        runtime
            .get_task_artifact(&task.id, COVERAGE_ARTIFACT)
            .expect("read artifact")
            .is_none()
    );

    // 3. The task's own Orbit run succeeds and records the evidence authority.
    runtime
        .run_tool_with_context(
            "orbit.task.artifact.put",
            put_input,
            ToolContext {
                session_context: ToolSessionContext {
                    effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                    ..ToolSessionContext::default()
                },
                reservation_owner: Some(ReservationOwnerContext {
                    owner_run_id: own_run.run_id,
                    owner_metadata: None,
                }),
                ..ToolContext::default()
            },
        )
        .expect("task's own run can write coverage");
    std::fs::remove_file(&source).ok();
    assert!(
        runtime
            .get_task_artifact(&task.id, COVERAGE_ARTIFACT)
            .expect("read coverage")
            .is_some()
    );
    assert!(
        runtime
            .get_task_artifact(&task.id, EVIDENCE_AUTHORITY_ARTIFACT)
            .expect("read authority")
            .is_some()
    );
}

fn moving_delivery_coverage_task_to_in_progress_outside_orbit_run_warns() {
    use crate::admission_audit::tool_support::run_tool_as_operator;

    let (_root, runtime, repo_root) = test_runtime();
    let machine_id = "local-machine";
    let runtime = runtime.with_automation_machine_identity(Some(machine_id.into()));
    let review_task = create_task(
        &runtime,
        &repo_root,
        "delivery review task",
        "owes delivery coverage",
        TaskStatus::Backlog,
        &[],
    );
    seed_open_delivery_batch_attempt(&runtime, machine_id, &review_task.id);

    let updated = run_tool_as_operator(
        &runtime,
        "orbit.task.update",
        json!({"id": review_task.id, "status": "in-progress"}),
    )
    .expect("moving delivery-coverage task to in-progress outside Orbit run succeeds");
    assert_eq!(updated["status"], "in-progress");
    assert_eq!(
        runtime.get_task(&review_task.id).expect("task").status,
        TaskStatus::InProgress
    );
    let warnings = updated["warnings"]
        .as_array()
        .expect("warnings array present");
    assert!(
        warnings.iter().any(|warning| {
            warning.as_str().is_some_and(|text| {
                text.contains("Orbit run")
                    && text.contains(&format!("orbit run ship {}", review_task.id))
            })
        }),
        "expected guidance warning naming task's Orbit run and `orbit run ship`, got {warnings:?}"
    );

    // An ordinary task without an open delivery batch attempt emits no such warning.
    let ordinary = create_task(
        &runtime,
        &repo_root,
        "ordinary task",
        "no delivery coverage",
        TaskStatus::Backlog,
        &[],
    );
    let ordinary_updated = run_tool_as_operator(
        &runtime,
        "orbit.task.update",
        json!({"id": ordinary.id, "status": "in-progress"}),
    )
    .expect("ordinary task in-progress");
    assert!(
        ordinary_updated.get("warnings").is_none(),
        "ordinary task should not carry coverage warning: {ordinary_updated}"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    task_artifacts_retain_trusted_local_provenance_and_reject_ssh_mcp_attribution_scenarios,
    task_artifacts_retain_trusted_local_provenance_and_reject_ssh_mcp_attribution,
    artifact_put_refuses_malformed_coverage_evidence_with_the_parse_error,
    delivery_coverage_write_refuses_outside_own_orbit_run_and_accepts_own_run,
    moving_delivery_coverage_task_to_in_progress_outside_orbit_run_warns
);
