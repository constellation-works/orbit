//! Pull drain: the refill loop, its owner/routed-owner and launcher adapters,
//! the `pull_refill` action a follower's `workspace_pull_pipeline` runs, and
//! the settle-only passes any follower process runs [ORB-13663].

pub(crate) mod adapters;
pub(crate) mod drain;
pub(crate) mod refill;
pub(crate) mod settle;

#[cfg(test)]
mod tests;
