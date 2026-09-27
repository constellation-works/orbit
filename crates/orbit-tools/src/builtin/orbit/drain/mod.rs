//! The owner-served surface of the distributed drain.
//!
//! Two halves:
//!
//! - **Read-only:** a preflight probe, receipt reconciliation, and claim
//!   inspection.
//! - **Mutating** [ORB-13625]: pull admission, run binding, and claim
//!   settlement — exactly what a follower's drain needs to carry one attempt
//!   from admission to handoff. Completion approval, revocation and recovery
//!   are not here: they stay owner-operator actions on the dashboard.
//!
//! Placement follows the same rule as the rest of the registry: everything a
//! follower must reach over federated MCP is advertised, while claim
//! inspection stays off the MCP surface as an operator surface. Unadvertised
//! is not unreachable: claim inspection is registered active so
//! `orbit tool run orbit.drain.claims` resolves, which is the invocation the
//! shipped skill reference names and the only entry point it has [ORB-12581].
//! Its operator requirement comes from the governed-operation registry, not
//! from placement.

pub mod claim_bind;
pub mod claim_settle;
pub mod claims;
pub mod probe;
pub mod pull;
pub mod receipt_lookup;
