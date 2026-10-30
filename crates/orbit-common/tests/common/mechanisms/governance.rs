//! Caller capability resolution from the signals a chokepoint observed.

use std::collections::BTreeSet;

use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, CallerProvenance, ToolGrantability,
    classify_agent_tool_grant, governed_command, governed_tool,
};
use orbit_types::tool::McpCapability;

#[test]
fn agent_envelope_outranks_a_terminal() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope {
        agent_declared: true,
        interactive_terminal: true,
        local_cli: true,
        ..CallerEnvelope::default()
    });
    assert_eq!(caller.provenance(), CallerProvenance::AgentEnvelope);
    assert_eq!(caller.grants(), &BTreeSet::from([McpCapability::Agent]));
}

#[test]
fn script_grant_and_revoke_are_operator_only_for_agents_and_the_cli() {
    // The trust boundary the script-steps design rests on: no agent
    // session (capability `agent`) and no plugin callback (no
    // capability) satisfies either row, so neither can grant the code
    // an agent can edit. Without these rows the classifier would
    // default both tools to grantable mutations.
    for tool in ["orbit.script.grant", "orbit.script.revoke"] {
        assert_eq!(
            classify_agent_tool_grant(tool).grantability,
            ToolGrantability::AlwaysOperatorOnly,
            "{tool} must classify operator-only"
        );
        let row = governed_tool(tool).unwrap_or_else(|| panic!("{tool} has a governed row"));
        assert_eq!(row.allowed, &[McpCapability::Operator]);
    }
    for subcommand in ["grant", "revoke"] {
        let row = governed_command("script", subcommand)
            .unwrap_or_else(|| panic!("script {subcommand} has a governed row"));
        assert_eq!(row.allowed, &[McpCapability::Operator]);
    }
}
