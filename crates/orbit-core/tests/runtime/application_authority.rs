//! Public API checks for exact task consent without operator capability.
use chrono::{Duration, Utc};
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::{OrbitRuntime, application::task::TaskAddParams};
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
            operation: ApplicationOperation {
                tool: "orbit.task.update".into(),
                input: input.clone(),
                plugin: None,
            },
            callbacks: vec![],
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
        application: Some(Box::new(ApplicationInvocation {
            consent: consent.clone(),
            scope_index: 0,
            callback: false,
        })),
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
        input: json!({}),
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

crate::scenario_table!(
    application_authority,
    application_task_consent_is_exact_and_wire_metadata_cannot_grant_it
);
