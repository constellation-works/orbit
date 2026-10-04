use super::{enter_isolated_child, test_runtime};
use orbit_types::{
    desktop::*,
    tool::{McpCapability, ToolSessionContext},
};
fn session(operator: bool) -> ToolSessionContext {
    let mut session = ToolSessionContext::default();
    session.effective_capabilities.insert(if operator {
        McpCapability::Operator
    } else {
        McpCapability::Agent
    });
    session
}
fn create(request_id: &str) -> DesktopTaskRequest {
    DesktopTaskRequest {
        request_id: request_id.into(),
        operation: DesktopTaskOperation::Create {
            title: "Desktop fixture".into(),
            description: "bounded".into(),
            acceptance_criteria: vec!["verified behavior".into()],
            priority: orbit_types::task::TaskPriority::Medium,
            crew: None,
        },
    }
}
#[test]
fn desktop_redacts_before_receipt_and_preserves_retries() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_redacts_before_receipt_and_preserves_retries",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let session = session(false);
    let snapshot = runtime
        .desktop_task_write(create("create"), None, None, &session)
        .unwrap()
        .snapshot;
    let secret = "abc123def456ghi789SECRETTOKEN";
    let request = DesktopTaskRequest {
        request_id: "redacted".into(),
        operation: DesktopTaskOperation::Comment {
            id: snapshot.task.id.clone(),
            expected_revision: snapshot.revision,
            comment: format!("Authorization: Bearer {secret}"),
        },
    };
    let once = runtime
        .desktop_task_write(request.clone(), None, None, &session)
        .unwrap();
    let twice = runtime
        .desktop_task_write(request, None, None, &session)
        .unwrap();
    assert!(twice.replayed);
    assert_eq!(once.snapshot.revision, twice.snapshot.revision);
    assert!(
        !runtime.get_task_comments(&snapshot.task.id).unwrap()[0]
            .message
            .contains(secret)
    );
}

// A crafted empty session cannot be produced by the ordinary MCP launcher;
// exercise this authority boundary directly, including a stolen retry identity.
#[test]
fn desktop_status_receipt_does_not_grant_authority() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_status_receipt_does_not_grant_authority",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let agent = session(false);
    let snapshot = runtime
        .desktop_task_write(create("authority-create"), None, None, &agent)
        .unwrap()
        .snapshot;
    let edit = DesktopTaskRequest {
        request_id: "authority-status".into(),
        operation: DesktopTaskOperation::Edit {
            id: snapshot.task.id.clone(),
            expected_revision: snapshot.revision.clone(),
            fields: DesktopTaskFields {
                status: Some(orbit_types::task::TaskStatus::Backlog),
                ..Default::default()
            },
        },
    };
    let anonymous = ToolSessionContext::default();
    let anonymous_snapshot = runtime
        .desktop_task_snapshot(&snapshot.task.id, &anonymous)
        .unwrap();
    assert!(!anonymous_snapshot.actions.edit.enabled);
    let mut crew_edit = edit.clone();
    if let DesktopTaskOperation::Edit { fields, .. } = &mut crew_edit.operation {
        fields.status = None;
        fields.crew = Some(String::new());
    }
    assert!(
        runtime
            .desktop_task_write(crew_edit, None, None, &anonymous)
            .is_err()
    );
    assert!(
        runtime
            .desktop_task_write(edit.clone(), None, None, &anonymous)
            .is_err()
    );
    assert_eq!(
        runtime
            .desktop_task_snapshot(&snapshot.task.id, &agent)
            .unwrap()
            .revision,
        snapshot.revision
    );
    let result = runtime
        .desktop_task_write(edit.clone(), None, None, &agent)
        .unwrap();
    assert!(
        runtime
            .desktop_task_write(edit, None, None, &anonymous)
            .is_err()
    );
    assert_eq!(
        runtime
            .desktop_task_snapshot(&snapshot.task.id, &agent)
            .unwrap()
            .revision,
        result.snapshot.revision
    );
}

// Persist a pre-status-extension receipt directly: using today's serializer to
// make that receipt would not exercise compatibility with an old process.
#[test]
fn desktop_legacy_edit_receipt_survives_status_contract_extension() {
    if !enter_isolated_child(
        module_path!(),
        "desktop_legacy_edit_receipt_survives_status_contract_extension",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let agent = session(false);
    let snapshot = runtime
        .desktop_task_write(create("legacy-create"), None, None, &agent)
        .unwrap()
        .snapshot;
    let old_payload = format!(
        r#"{{"request_id":"legacy-edit","operation":{{"kind":"edit","id":{},"expected_revision":{},"fields":{{"title":"Before upgrade","description":null,"acceptance_criteria":null,"priority":null,"crew":null}}}}}}"#,
        serde_json::to_string(&snapshot.task.id).unwrap(),
        serde_json::to_string(&snapshot.revision).unwrap()
    );
    let fields = DesktopTaskFields {
        title: Some("Before upgrade".into()),
        ..Default::default()
    };
    runtime
        .stores()
        .tasks()
        .apply_desktop_task_mutation(
            &snapshot.task.id,
            &orbit_store::contracts::DesktopTaskMutationParams {
                actor: "codex".into(),
                request_id: "legacy-edit".into(),
                payload_digest: orbit_common::security::release::sha256_hex(old_payload.as_bytes()),
                expected_revision: snapshot.revision.clone(),
                fields: fields.clone(),
                comment: None,
                status: None,
            },
        )
        .unwrap();
    let committed = runtime
        .desktop_task_snapshot(&snapshot.task.id, &agent)
        .unwrap();
    let result = runtime
        .desktop_task_write(
            DesktopTaskRequest {
                request_id: "legacy-edit".into(),
                operation: DesktopTaskOperation::Edit {
                    id: snapshot.task.id,
                    expected_revision: snapshot.revision,
                    fields,
                },
            },
            None,
            None,
            &agent,
        )
        .unwrap();
    assert!(
        result.replayed,
        "the persisted old payload remains the same request"
    );
    assert_eq!(result.snapshot.revision, committed.revision);
    assert_eq!(result.snapshot.task.title, "Before upgrade");
}
