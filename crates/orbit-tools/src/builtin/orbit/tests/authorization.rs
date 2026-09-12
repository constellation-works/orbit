//! Guardrail: the governed-operation registry names real tools [ORB-10453],
//! and every governed tool's MCP placement is declared [ORB-10478].
//!
//! `GOVERNED_OPERATIONS` is declared in `orbit-common`, which sits below the
//! tool registry and so cannot check itself against it. The registry is keyed by
//! string, so a renamed or deleted tool would silently stop being governed —
//! the failure mode is a *disappearing* guard, which no behavioural test
//! notices because nothing starts failing. This is the test that does.
//!
//! It is also the one place both axes are visible at once. Placement (which
//! surfaces list a tool) lives in the tool registry; permission (who may
//! perform it) lives in `orbit_common::governance::authorization`. Neither crate can check
//! the pairing alone, so the invariant is asserted here rather than left to a
//! reader holding two files open. `orbit-tools` is the lowest crate that sees
//! both, so this needs no new dependency edge.

use std::collections::BTreeSet;

use orbit_common::governance::authorization::{
    GOVERNED_OPERATIONS, GovernedOperation, OperationSurface,
};
use orbit_types::tool::McpCapability;

use crate::ToolRegistry;

/// Whether MCP advertises a governed tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Placement {
    /// Listed by `tools/list`, and refused at the chokepoint to any session
    /// that does not hold the capability the registry names. Legitimate — it
    /// is how the operator MCP surface is expressed — but never accidental.
    Advertised,
    /// Kept off MCP entirely, and reached through the CLI or the dashboard.
    Unadvertised,
}

/// Every governed tool operation, paired with the placement it is meant to
/// have.
///
/// Advertisement authorizes nothing, so the two axes are free to disagree —
/// see `orbit_common::governance::authorization`'s "Placement is not permission" section.
/// What this table forbids is *silent* disagreement. Advertising a governed
/// tool, governing an advertised one, or dropping either half fails this test
/// until the table is updated, which is the moment the decision gets made on
/// purpose instead of noticed later.
const GOVERNED_TOOL_PLACEMENT: &[(&str, Placement)] = &[
    // The operator MCP surface [ORB-10534, ORB-10711]: advertised so an
    // operator session can drive dispatch over MCP, governed so an agent
    // session holding only `agent` is refused.
    // [ORB-11354] Advertised so an operator MCP session can start an
    // exploration; governed to `operator` alone (never `runner`) so a managed
    // run cannot admit a subprocess outside the sandbox it executes inside.
    ("orbit.agent.invoke", Placement::Advertised),
    ("orbit.command.exec", Placement::Advertised),
    // [ORB-11332] Grant lifecycle: advertised so an operator MCP session can
    // enable, stop, or revoke scoped automation; governed so an agent
    // session cannot grant itself authority.
    ("orbit.operation.enable", Placement::Advertised),
    ("orbit.operation.revoke", Placement::Advertised),
    ("orbit.operation.stop", Placement::Advertised),
    ("orbit.workflow.run.list", Placement::Advertised),
    ("orbit.workflow.run.resume", Placement::Advertised),
    ("orbit.workflow.run.show", Placement::Advertised),
    ("orbit.workflow.run.workers", Placement::Advertised),
    ("orbit.workflow.ship", Placement::Advertised),
    // Destructive administration: off MCP, and governed so that being off MCP
    // is not the only thing standing between an agent and the operation
    // [ORB-10453].
    ("orbit.semantic.uninstall", Placement::Unadvertised),
    ("orbit.task.delete", Placement::Unadvertised),
    ("orbit.task.locks.release", Placement::Unadvertised),
    ("orbit.task.locks.reserve", Placement::Unadvertised),
    ("orbit.task.reject", Placement::Unadvertised),
    ("orbit.workspace.claim.release", Placement::Unadvertised),
];

/// Reads an agent performs as ordinary work, which must stay ungoverned.
///
/// Governing these would refuse an agent `orbit friction list` / `show` from
/// the CLI. That is the regression the capability chokepoint declined to ship
/// [ORB-10453], and it is the reason enforcement is not derived from placement
/// [ORB-10478].
const AGENT_REACHABLE_FRICTION_READS: &[&str] = &["orbit.friction.list", "orbit.friction.show"];

fn builtin_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    registry
}

fn governed_tool_operations() -> impl Iterator<Item = &'static GovernedOperation> {
    GOVERNED_OPERATIONS
        .iter()
        .filter(|operation| operation.surface == OperationSurface::Tool)
}

/// Canonical names MCP advertises, from the same source the server enumerates.
fn advertised_tool_names(registry: &ToolRegistry) -> BTreeSet<String> {
    registry
        .mcp_tool_definitions()
        .expect("the builtin registry composes valid MCP definitions")
        .into_iter()
        .map(|definition| definition.schema.name)
        .collect()
}

#[test]
fn every_governed_tool_operation_names_a_registered_tool() {
    let registry = builtin_registry();

    for operation in GOVERNED_OPERATIONS
        .iter()
        .filter(|operation| operation.surface == OperationSurface::Tool)
    {
        assert!(
            registry.has(operation.id),
            "governed operation '{}' names no registered tool — a rename dropped its guard \
             silently, since a governed id that matches nothing simply never denies",
            operation.id
        );
    }
}

#[test]
fn the_destructive_builtins_this_task_closed_are_still_governed() {
    // These are the tools ORB-10453 found reachable through `orbit tool run`
    // despite being hidden from the MCP surface. Pinning them by name keeps a
    // future refactor from quietly returning them to the ungoverned set.
    let governed: Vec<&str> = GOVERNED_OPERATIONS
        .iter()
        .filter(|operation| operation.surface == OperationSurface::Tool)
        .map(|operation| operation.id)
        .collect();

    for expected in [
        "orbit.task.delete",
        "orbit.task.reject",
        "orbit.task.locks.release",
        "orbit.semantic.uninstall",
    ] {
        assert!(
            governed.contains(&expected),
            "'{expected}' must stay governed; currently governed: {governed:?}"
        );
    }
}

#[test]
fn every_governed_tool_declares_the_mcp_placement_it_actually_has() {
    let registry = builtin_registry();
    let advertised = advertised_tool_names(&registry);

    let mut observed: Vec<(&str, Placement)> = governed_tool_operations()
        .map(|operation| {
            let placement = if advertised.contains(operation.id) {
                Placement::Advertised
            } else {
                Placement::Unadvertised
            };
            (operation.id, placement)
        })
        .collect();
    observed.sort();

    let mut declared = GOVERNED_TOOL_PLACEMENT.to_vec();
    declared.sort();

    assert_eq!(
        observed, declared,
        "the governed tools' MCP placement drifted from what this table declares. \
         Advertisement authorizes nothing, so a mismatch is not automatically a bug — \
         but it is a decision, so record it here and in \
         docs/design/operations-as-data/4_decisions.md rather than letting the two \
         axes diverge unobserved"
    );
}

#[test]
fn an_advertised_governed_tool_is_deliberately_out_of_reach_for_an_agent_session() {
    let registry = builtin_registry();
    let advertised = advertised_tool_names(&registry);

    for operation in governed_tool_operations().filter(|op| advertised.contains(op.id)) {
        assert!(
            !operation.allowed.contains(&McpCapability::Agent),
            "'{}' is advertised to every MCP session and governed, yet lists `agent` among its \
             allowed capabilities — governing an operation the ordinary MCP caller already \
             holds the capability for buys nothing and hides the entries that do refuse",
            operation.id
        );
    }
}

#[test]
fn the_agent_reachable_friction_reads_stay_ungoverned() {
    // These were left ungoverned on purpose [ORB-10453] and ORB-10478 confirms
    // the choice: the chokepoint is surface-independent, so governing a read
    // here would refuse `orbit friction list` to an agent on the CLI, not just
    // on MCP.
    let governed: Vec<&str> = governed_tool_operations()
        .map(|operation| operation.id)
        .collect();

    for read in AGENT_REACHABLE_FRICTION_READS {
        assert!(
            !governed.contains(read),
            "'{read}' became governed, which refuses an agent an ordinary friction read from \
             any surface; currently governed: {governed:?}"
        );
    }
}
