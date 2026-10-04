use crate::application::task::tests::{enter_isolated_child, test_runtime};
use orbit_types::{desktop::*, tool::ToolSessionContext};

use super::{create, session};

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
