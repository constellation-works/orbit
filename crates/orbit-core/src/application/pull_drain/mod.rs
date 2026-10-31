//! Pull drain: the refill loop, the `pull_refill` pass a follower's
//! `workspace_pull_pipeline` runs, and the settle-only passes any follower
//! process runs [ORB-13663]. The owner transport and leaf launcher the drain
//! runs over are adapters (`adapter::engine_host::actions::pull`).

mod candidate;
pub(crate) mod drain;
mod job_port;
mod refill;
mod settle;

pub(crate) use refill::{crew_window, pull_refill};

#[cfg(test)]
mod tests;
