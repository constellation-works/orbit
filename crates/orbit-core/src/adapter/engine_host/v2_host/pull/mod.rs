//! Pull drain: the refill loop, its owner/routed-owner and launcher adapters,
//! and the `pull_refill` action a follower's `workspace_pull_pipeline` runs.

pub(crate) mod adapters;
pub(crate) mod drain;
pub(crate) mod refill;

#[cfg(test)]
mod tests;
