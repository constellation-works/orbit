//! Public API checks for exact task consent without operator capability.
#![cfg(unix)]
use chrono::{Duration, Utc};
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::TaskAddParams;
use orbit_core::runtime::OrbitRuntime;
use orbit_types::tool::{
    ApplicationConsentRequest, ApplicationInvocation, ApplicationOperation, ApplicationScope,
    McpCapability, ToolSessionContext,
};
use serde_json::json;

fn application_task_consent_is_exact_and_wire_metadata_cannot_grant_it() {
    let directory = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = directory.path().join("global");
    let orbit = directory.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &orbit).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Application API approval target".into(),
            ..Default::default()
        })
        .unwrap();
    let input = json!({"id":task.id,"comment":"User approved this exact comment"});
    let request = ApplicationConsentRequest {
        application_id: "api-service".into(),
        expires_at: Utc::now() + Duration::hours(1),
        scopes: vec![ApplicationScope {
            name: None,
            operation: ApplicationOperation {
                tool: "orbit.task.update".into(),
                input: Some(input.clone()),
                fields: None,
                plugin: None,
            },
            callbacks: vec![],
            rate_limit: None,
        }],
    };
    let agent = ToolSessionContext {
        effective_capabilities: [McpCapability::Agent].into(),
        ..Default::default()
    };
    assert!(
        runtime
            .consent_application(request.clone(), &agent, None)
            .is_err()
    );
    let operator = ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into(),
        ..Default::default()
    };
    let (consent, credential) = runtime
        .consent_application(request, &operator, None)
        .unwrap();
    assert!(!format!("{credential:?}").contains(consent.consent_id.as_str()));
    let session = ToolSessionContext {
        application: Some(Box::new(ApplicationInvocation::new(
            consent.clone(),
            0,
            false,
        ))),
        ..Default::default()
    };
    runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.task.update",
            input.clone(),
            None,
            None,
            ToolEntryPoint::Cli,
            session.clone(),
        )
        .unwrap();
    let updated = runtime.get_task_comments(&task.id).unwrap();
    assert!(
        updated
            .iter()
            .any(|comment| comment.message == "User approved this exact comment")
    );
    assert!(
        runtime
            .execute_tool_command_dispatch_with_session_context(
                "orbit.task.update",
                json!({"id":task.id,"comment":"unlisted"}),
                None,
                None,
                ToolEntryPoint::Cli,
                session.clone()
            )
            .is_err()
    );
    let wire = serde_json::to_value(&session).unwrap();
    assert!(wire.get("application").is_none());
    let mut spoofed = wire;
    spoofed["application"] = json!(session.application);
    let spoofed: ToolSessionContext = serde_json::from_value(spoofed).unwrap();
    assert!(spoofed.application.is_none());
    let mut mixed_agent = session.clone();
    mixed_agent
        .effective_capabilities
        .insert(McpCapability::Agent);
    assert!(
        runtime
            .execute_tool_command_dispatch_with_session_context(
                "orbit.task.update",
                input.clone(),
                None,
                None,
                ToolEntryPoint::Mcp,
                mixed_agent
            )
            .is_err()
    );
    let mut unsupported = ApplicationConsentRequest {
        application_id: "api-service".into(),
        expires_at: Utc::now() + Duration::hours(1),
        scopes: consent.scopes.clone(),
    };
    unsupported.scopes[0].operation = ApplicationOperation {
        tool: "orbit.routine.control".into(),
        input: Some(json!({})),
        fields: None,
        plugin: None,
    };
    assert!(
        runtime
            .consent_application(unsupported, &operator, None)
            .is_err()
    );
    runtime
        .revoke_application(&consent.consent_id, &operator)
        .unwrap();
    assert!(
        runtime
            .execute_tool_command_dispatch_with_session_context(
                "orbit.task.update",
                input,
                None,
                None,
                ToolEntryPoint::Cli,
                session
            )
            .is_err()
    );
}

/// An application-consented status change refused by a live local run names no
/// `--force`: the consent carries no force, so the caller must wait or ask the
/// operator. A human operator's refusal for the same run still names `--force`.
fn application_status_refusal_on_a_live_run_names_no_force() {
    use orbit_common::ForceOverridableKind;
    use orbit_core::application::task::{TaskUpdateAuthority, TaskUpdateParams, TaskWriteScreen};
    use orbit_core::runtime::context::ActorIdentity;
    use orbit_core::test_support::EngineHost;
    use orbit_core::test_support::fixture_engine::RunLifecycle;
    use orbit_types::task::TaskStatus;
    use orbit_types::workflow::JobRunState;

    let directory = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = directory.path().join("global");
    let orbit = directory.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &orbit)
        .unwrap()
        .with_actor(ActorIdentity::human("human:operator"));
    let task = runtime
        .add_task(TaskAddParams {
            title: "Application status change under a live run".into(),
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
        .unwrap();
    let run = RunLifecycle::insert_job_run(
        &EngineHost::new(&runtime),
        "application-run-ownership-fixture",
        1,
        chrono::Utc::now(),
        Some(json!({ "task_ids": [task.id] })),
        None,
    )
    .unwrap();
    RunLifecycle::mark_job_run_running(
        &EngineHost::new(&runtime),
        &run.run_id,
        chrono::Utc::now(),
        std::process::id(),
    )
    .unwrap();

    let input = json!({"id": task.id, "status": "backlog"});
    let request = ApplicationConsentRequest {
        application_id: "api-service".into(),
        expires_at: Utc::now() + Duration::hours(1),
        scopes: vec![ApplicationScope {
            name: None,
            operation: ApplicationOperation {
                tool: "orbit.task.update".into(),
                input: Some(input.clone()),
                fields: None,
                plugin: None,
            },
            callbacks: vec![],
            rate_limit: None,
        }],
    };
    let operator = ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into(),
        ..Default::default()
    };
    let (consent, _credential) = runtime
        .consent_application(request, &operator, None)
        .unwrap();
    let session = ToolSessionContext {
        application: Some(Box::new(ApplicationInvocation::new(consent, 0, false))),
        ..Default::default()
    };
    let refused = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.task.update",
            input,
            None,
            None,
            ToolEntryPoint::Cli,
            session,
        )
        .expect_err("a live run refuses the application status change");
    let refusal = refused
        .force_overridable()
        .expect("the refusal is a typed run-ownership refusal");
    assert_eq!(refusal.kind, ForceOverridableKind::RunStillOwnsTask);
    assert_eq!(refusal.run_id.as_deref(), Some(run.run_id.as_str()));
    let message = refused.to_string();
    assert!(
        !message.contains("--force"),
        "application refusal: {message}"
    );
    assert!(
        message.contains("wait for the run to finish or ask the operator to stop it"),
        "application refusal: {message}"
    );

    let human = runtime
        .update_task_screened(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            &TaskWriteScreen::unbound(false),
            TaskUpdateAuthority::Lifecycle,
            None,
            None,
        )
        .expect_err("a live run refuses the human status change");
    let refusal = human
        .force_overridable()
        .expect("the human refusal is a typed run-ownership refusal");
    assert_eq!(refusal.kind, ForceOverridableKind::RunStillOwnsTask);
    assert!(
        human
            .to_string()
            .contains("stop the run first or use --force to override"),
        "human refusal: {human}"
    );

    assert_eq!(
        runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Proposed
    );
    assert_eq!(
        runtime.show_job_run(&run.run_id).unwrap().state,
        JobRunState::Running
    );
}

crate::scenario_table!(
    application_authority,
    application_task_consent_is_exact_and_wire_metadata_cannot_grant_it,
    application_status_refusal_on_a_live_run_names_no_force
);
