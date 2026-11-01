//! Operation authorization shared by Orbit's execution surfaces.
//!
//! This module declares which operations are governed, which capabilities may
//! perform them, and the decision function every enforcing surface calls. Every
//! surface — CLI, MCP, dashboard, managed run — resolves the same capability
//! vocabulary here; they differ only in which signals may contribute to it,
//! which is [`CapabilityResolution`].
//!
//! # What this is, and what it is not
//!
//! This is an **accident guard, not a security boundary**. Every agent on a
//! development box runs as the same OS user and can bypass Orbit entirely with
//! `git`, `rm`, or a direct write to the data root. Application credentials
//! identify exact host-issued consents within this guard; they do not protect
//! against arbitrary same-user code. No surrounding sandbox or authority check
//! may be relaxed because a credential exists.
//!
//! The goal is narrower and achievable: unintended destruction fails loudly
//! and leaves a record.
//!
//! # Placement is not permission
//!
//! Orbit has two independent axes, and this module owns exactly one of them.
//!
//! *Placement* is [`crate::governance::operation::OperationSpec::mcp_scope`] and its
//! registry counterparts (`register_mcp`, `register`, `register_inactive`):
//! which surfaces *list* a tool. It is an audience decision — what an agent
//! reading `tools/list` is pointed at — and it authorizes nothing.
//! The MCP list adapter further filters `tools/list` by the session's fixed grants,
//! omitting governed tools those grants cannot authorize. `orbit.agent.invoke`
//! remains discoverable because of its separate managed-run explorer admission.
//! This only changes advertisement:
//! the tool registry's `execute` never consults availability, so a tool
//! registered active but unadvertised is still reachable through
//! `orbit tool run`, subject to the capability chokepoint.
//!
//! `register_inactive` is the one placement that also removes reach: CLI
//! dispatch applies `ensure_tool_agent_facing` too, so an inactive tool needs
//! a command that reaches the runtime directly — the way
//! `orbit task locks release` does — and registering one without that command
//! leaves it callable from nowhere [ORB-12581].
//!
//! *Permission* for exceptional operations is [`GOVERNED_OPERATIONS`], resolved
//! by [`authorize`] at one chokepoint per surface. A session that arrived over
//! SSH is decided the same way as a local one: the destination honors the
//! authority in the argv it was started with, because an SSH login to a machine
//! is ownership of it [ORB-12564]. The registry remains the only
//! capability-specific authorization statement Orbit makes, and its answer is surface-independent:
//! the same answer for an MCP call, a CLI `tool run`, the dashboard, and the
//! deterministic dispatcher.
//!
//! The two therefore need not agree, and deliberately do not. A tool may be
//! advertised and governed (the operator MCP surface: a session served by
//! `orbit mcp serve --operator` sees and performs it, an ordinary agent session
//! omits it from its list and is still refused if it calls the name directly),
//! or unadvertised and ungoverned (`orbit friction show`
//! — kept off the agent MCP surface because `list` already returns what an
//! agent needs, but freely readable from any CLI).
//! What must never happen is for a pairing to change by accident. The
//! guardrail that pins every governed tool's placement is
//! `crates/orbit-tools/src/builtin/orbit/tests/authorization.rs`, which lives
//! in the lowest crate that can see both axes at once [ORB-10478].
//!
//! # Application consent
//!
//! Host-issued application sessions are a separate, exact-call authority,
//! verified against live workspace, input, digest and consent state by the
//! runtime before capability resolution. They never add Operator, Runner or
//! Agent grants to this catalog. Ordinary callers keep every denial here;
//! self-declared application metadata contributes nothing to their envelope.
//!
//! # Layering
//!
//! The kernel lives in the leaf crate for the same reason the operations-as-data
//! registry does (see [`crate::governance::operation`]): every consumer surface must be able
//! to read it without acquiring a new dependency edge. It holds no runtime
//! handle, no transport types, and no store — surfaces feed it a
//! [`CallerEnvelope`] and it answers.

mod caller;
mod catalog;
mod decision;
mod env;
mod grants;

pub use caller::{CallerCapabilities, CallerEnvelope, CallerProvenance, CapabilityResolution};
pub use catalog::{
    APPLICATION_BACKLOG_TRANSITION_SOURCES, APPLICATION_DIRECT_TASK_STATUSES,
    APPLICATION_EXACT_TASK_STATUSES, APPLICATION_OPERATION_CEILINGS, APPLICATION_TASK_SHOW_FIELDS,
    APPLICATION_TRANSITION_TASK_STATUSES, ApplicationOperationCeiling,
    DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED, DASHBOARD_AUTO_DRAIN_AUTO_FIX,
    DASHBOARD_AUTO_DRAIN_COMPLETE, DASHBOARD_AUTO_DRAIN_MODE, DASHBOARD_AUTO_DRAIN_STOP,
    DASHBOARD_AUTO_TASK_DELETE, DASHBOARD_AUTO_TASK_MINT, DASHBOARD_AUTO_TASK_SCHEDULE,
    DASHBOARD_AUTO_TASK_TOGGLE, DASHBOARD_CLAIM_RECOVER, DASHBOARD_CLOCK_CADENCE,
    DASHBOARD_CLOCK_SERVICE, DASHBOARD_CONFIG_SET, DASHBOARD_EXPERIMENT_CONCLUDE,
    DASHBOARD_EXPERIMENT_CREATE, DASHBOARD_EXPERIMENT_START, DASHBOARD_EXPERIMENT_UPDATE,
    DASHBOARD_HANDOFF_APPROVE, DASHBOARD_HANDOFF_RETRY, DASHBOARD_HANDOFF_REVOKE,
    DASHBOARD_HOST_EDIT, DASHBOARD_HOST_FORWARD, DASHBOARD_JOB_RUN, DASHBOARD_LEASE_WRITE,
    DASHBOARD_PLUGIN_DISABLE, DASHBOARD_PLUGIN_ENABLE, DASHBOARD_ROUTINE_TOGGLE,
    DASHBOARD_ROUTINE_TRIGGER, DASHBOARD_TASK_PILOT_ADMISSION, DASHBOARD_TASK_POST_DEPLOY,
    DASHBOARD_TASK_REQUEUE, DASHBOARD_TASK_SETTLE_MERGED, DESKTOP_TASK_COMPLETE, DESKTOP_TASK_EDIT,
    GOVERNED_OPERATIONS, GovernedOperation, INPUT_DEPENDENT_OPERATION_FLOORS, OperationSurface,
    PLUGIN_TOOL_AGENT_CALLABLE, PLUGIN_TOOL_MUTATING, PLUGIN_TOOL_READ_ONLY,
    TASK_GRANT_TOOLS_OPERATION_ID, application_callback_tool, application_operation_ceiling,
    application_operator_floored, governed_command, governed_plugin_tool, governed_tool,
};
pub use decision::{AuthorizationDenial, authorize};
pub use env::{OPERATOR_OVERRIDE_ENV, agent_context_declared, operator_override_active};
pub use grants::{
    GrantedToolEffect, ToolGrantClassification, ToolGrantability, classify_agent_tool_grant,
};
