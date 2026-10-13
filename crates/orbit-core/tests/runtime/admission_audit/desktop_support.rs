#![allow(missing_docs)]

use orbit_types::{
    desktop::*,
    tool::{McpCapability, ToolSessionContext},
};
pub(crate) fn session(operator: bool) -> ToolSessionContext {
    let mut session = ToolSessionContext::default();
    session.effective_capabilities.insert(if operator {
        McpCapability::Operator
    } else {
        McpCapability::Agent
    });
    session
}
pub(crate) fn create(request_id: &str) -> DesktopTaskRequest {
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
