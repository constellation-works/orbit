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
//! `git`, `rm`, or a direct write to the data root. Nothing here changes that,
//! and no design that pretends otherwise — a password, a token, a keychain —
//! belongs here: it would buy no protection while inviting the surrounding
//! rails to be relaxed on the strength of a boundary that does not exist.
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
//! `tools/list` does no capability filtering, and the tool registry's
//! `execute` never consults availability, so a tool registered active but
//! unadvertised is still reachable through `orbit tool run`.
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
//! operation-specific
//! authorization statement Orbit makes, and its answer is surface-independent:
//! the same answer for an MCP call, a CLI `tool run`, the dashboard, and the
//! deterministic dispatcher.
//!
//! The two therefore need not agree, and deliberately do not. A tool may be
//! advertised and governed (the operator MCP surface: a session served by
//! `orbit mcp serve --operator` sees and performs it, an ordinary agent session
//! sees and is refused), or unadvertised and ungoverned (`orbit friction show`
//! — kept off the agent MCP surface because `list` already returns what an
//! agent needs, but freely readable from any CLI).
//! What must never happen is for a pairing to change by accident. The
//! guardrail that pins every governed tool's placement is
//! `crates/orbit-tools/src/builtin/orbit/tests/authorization.rs`, which lives
//! in the lowest crate that can see both axes at once [ORB-10478].
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

pub use caller::{CallerCapabilities, CallerEnvelope, CallerProvenance, CapabilityResolution};
pub use catalog::{
    DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED, DASHBOARD_AUTO_DRAIN_COMPLETE,
    DASHBOARD_AUTO_DRAIN_STOP, DASHBOARD_AUTO_TASK_MINT, DASHBOARD_AUTO_TASK_TOGGLE,
    DASHBOARD_CLAIM_RECOVER, DASHBOARD_CLOCK_CADENCE, DASHBOARD_CLOCK_SERVICE,
    DASHBOARD_CONFIG_SET, DASHBOARD_HANDOFF_APPROVE, DASHBOARD_HANDOFF_REVOKE, DASHBOARD_HOST_EDIT,
    DASHBOARD_HOST_FORWARD, DASHBOARD_JOB_RUN, DASHBOARD_PLUGIN_DISABLE, DASHBOARD_PLUGIN_ENABLE,
    DASHBOARD_ROUTINE_TOGGLE, DESKTOP_TASK_COMPLETE, DESKTOP_TASK_EDIT, GOVERNED_OPERATIONS,
    GovernedOperation, OperationSurface, PLUGIN_TOOL_MUTATING, PLUGIN_TOOL_READ_ONLY,
    governed_command, governed_dashboard, governed_plugin_tool, governed_tool,
};
pub use decision::{AuthorizationDenial, authorize};
pub use env::{OPERATOR_OVERRIDE_ENV, agent_context_declared, operator_override_active};
