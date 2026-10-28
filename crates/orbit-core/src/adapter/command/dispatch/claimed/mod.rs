//! Claimed-worker owner coordination and its distinct artifact authorization scopes.

mod findings;
mod owner;
mod recovery;
mod review;

pub(super) use owner::execute_brokered;
pub use owner::{ClaimedOwnerRoute, bridge_claimed_owner_call, owner_route_unavailable};

#[cfg(test)]
mod tests;
