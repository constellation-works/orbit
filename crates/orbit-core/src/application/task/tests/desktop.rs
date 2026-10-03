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
