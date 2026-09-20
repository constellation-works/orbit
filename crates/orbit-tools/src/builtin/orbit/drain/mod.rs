//! The owner-served read-only surface of the distributed drain.
//!
//! Only the read-only half exists: a preflight probe, receipt reconciliation,
//! and claim inspection. Pull, run binding, settlement, handoff, and completion
//! approval are not registered tools — their integration slice has not landed,
//! and `orbit-core`'s distributed gate refuses them from every surface so an
//! incomplete feature cannot be turned on by registering a tool.
//!
//! Placement follows the same rule as the rest of the registry: the probe and
//! the receipt lookup are advertised because a follower must reach them over
//! federated MCP before it can enable pull, while claim inspection stays off
//! the MCP surface as an operator surface. Unadvertised is not unreachable:
//! claim inspection is registered active so `orbit tool run orbit.drain.claims`
//! resolves, which is the invocation the shipped skill reference names and the
//! only entry point it has [ORB-12581]. Its operator requirement comes from
//! the governed-operation registry, not from placement.

pub mod claims;
pub mod probe;
pub mod receipt_lookup;
