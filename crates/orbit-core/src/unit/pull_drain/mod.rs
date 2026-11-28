//! Pull drain: the refill loop, the `pull_refill` pass a follower's
//! `workspace_pull_pipeline` runs, and the settle-only passes any follower
//! process runs [ORB-13663]. The owner transport and leaf launcher the drain
//! runs over are adapters (`adapter::engine_host::actions::pull`).

mod candidate;

mod crew_health;

mod drain;

mod job_port;

mod refill;

mod settle;

pub(crate) use crew_health::record_pool_crew_health;
pub(crate) use drain::{
    PullLauncher, PullPeer, PullTransportPort, leaf_attribution, recorded_base_no_longer_red,
};
pub(crate) use job_port::JobPort;
pub(crate) use refill::{crew_window, pull_refill};
pub use settle::{
    deliver_recorded_pull_settlements, pending_pull_settlements, settle_orphaned_pulls,
};

#[cfg(test)]
mod tests;
