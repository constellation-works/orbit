use std::collections::BTreeSet;

use super::super::authorization::*;
use orbit_types::tool::McpCapability;

fn envelope() -> CallerEnvelope {
    CallerEnvelope::default()
}

#[test]
fn agent_envelope_outranks_a_terminal() {
    let caller = CallerCapabilities::resolve(&CallerEnvelope {
        agent_declared: true,
        interactive_terminal: true,
        local_cli: true,
        ..envelope()
    });
    assert_eq!(caller.provenance(), CallerProvenance::AgentEnvelope);
    assert_eq!(caller.grants(), &BTreeSet::from([McpCapability::Agent]));
}
