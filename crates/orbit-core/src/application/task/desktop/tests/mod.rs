#![allow(missing_docs)]

mod write;

use orbit_types::tool::{McpCapability, ToolSessionContext};
pub(super) fn session(operator: bool) -> ToolSessionContext {
    let mut session = ToolSessionContext::default();
    session.effective_capabilities.insert(if operator {
        McpCapability::Operator
    } else {
        McpCapability::Agent
    });
    session
}
