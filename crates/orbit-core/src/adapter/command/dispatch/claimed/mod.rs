//! Claimed-worker owner coordination and its distinct artifact authorization scopes.

mod findings;
mod owner;
mod recovery;
mod review;

pub use owner::{ClaimedOwnerRoute, bridge_claimed_owner_call, owner_route_unavailable};
pub(super) use owner::{bridge_claimed_owner_call_with_env, execute_brokered};

#[cfg(test)]
mod tests;
