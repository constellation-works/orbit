use super::super::identity::{McpSessionAuthority, mcp_server_identity};
use orbit_types::tool::McpCapability;
use std::collections::BTreeSet;

#[test]
fn a_default_server_serves_agent_sessions_only() {
    let root = tempfile::tempdir().expect("global root");

    let identity = mcp_server_identity(root.path(), None, McpSessionAuthority::Agent)
        .expect("MCP server identity");

    assert_eq!(
        identity.session_context.effective_capabilities,
        BTreeSet::from([McpCapability::Agent]),
        "an agent server must never stamp operator authority onto its sessions"
    );
}
