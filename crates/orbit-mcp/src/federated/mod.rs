//! The federated MCP namespace: a mux over the accepting machine plus
//! operator-configured SSH remotes.
//!
//! This module is the one place in Orbit where an MCP server also acts as an
//! MCP *client*. It exists so a caller can see the accepting machine's
//! workspaces and every configured remote destination's workspaces as one
//! session-unbound list of live descriptors, each carrying the host-qualified
//! selector a later routed call will address. Local selectors are delivered
//! in-process; remotes reuse the v1 SSH argv. v1's byte-transparent SSH proxy
//! ([`crate::serve_mcp_remote_proxy`]) is unchanged and remains the path for a
//! caller that has already chosen one host.
//!
//! The mux is deliberately not a fleet registry: remote membership comes only
//! from the operator's host file ([`load_destinations`]). The accepting machine is an
//! implicit local destination and is not declared as an SSH row. No
//! destination's answer is cached between calls.

mod capability;
mod config;
mod descriptor;
mod host;
mod probe;
mod task_route;

#[cfg(test)]
mod tests;

pub use self::capability::{
    CapabilityClasses, McpToolClass, ensure_tool_class_held, mcp_tool_class,
};
pub use self::config::{
    Destination, DestinationTransport, MachineQualifiedSelector, federated_membership,
    load_destinations,
};
pub use self::descriptor::{Capability, CheckoutHealth, Reachability, WorkspaceDescriptor};
pub use self::host::{FEDERATED_WORKSPACE_LIST_TOOL, FederatedMcpHost};
pub use self::probe::{
    CompositeDestinationProbe, DEFAULT_PROBE_TIMEOUT, DEFAULT_ROUTED_DELIVERY_TIMEOUT,
    DestinationProbe, DestinationSnapshot, InProcessDestinationProbe, RoutedSession,
    SshDestinationProbe,
};
pub use self::task_route::{
    ID_ROUTED_TASK_TOOLS, NOT_ID_ROUTED_TOOLS, id_only_task_target, is_id_routed_tool,
};
