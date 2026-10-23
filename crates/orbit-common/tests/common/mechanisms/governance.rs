//! Caller capability resolution from the signals a chokepoint observed.

use std::collections::BTreeSet;

use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, CallerProvenance,
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
