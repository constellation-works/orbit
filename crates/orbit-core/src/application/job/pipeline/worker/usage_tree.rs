//! macOS resource samples from the worker's process tree.
//!
//! Linux charges a scope's single cumulative counter. A process tree has no
//! such counter: a child that exits takes its CPU out of the sum, and a naive
//! subtraction goes negative or, if the remaining total is substituted, spikes
//! with a survivor's lifetime. [`TreeCpuAccount`] charges each process's own
//! plus reaped-children CPU, so short-lived descendants count through the
//! parent that waited for them. The last observed total of a process that
//! exited is owed by the member that will reap it, so a sampled child is not
//! counted again when reaped; one reparented away owes nothing. A debt that
//! the owner's next sighting does not settle lapses, so a descendant reaped
//! outside the tree hides at most one window of the owner's reaped growth. The first
//! successful read is a baseline and charges nothing. An unreadable tree
//! leaves that baseline where it was, so the next success still has a
//! previous sample to diff.
//!
//! The tree a worker samples is a [`SessionTree`]: the worker leads its own
//! session, so a descendant orphaned to launchd is still walked, and so is
//! one in a session a member started.

#[cfg(target_os = "macos")]
use orbit_common::process::ancestry::SessionTree;
use orbit_common::process::ancestry::{ProcessStartKey, ProcessTreeProcess, TreeCpuAccount};

use super::scope::ScopeUsage;

/// Turns process-tree snapshots into the monotonic usage the minute aggregator
/// already records for a cgroup scope.
pub(super) struct TreeSampler {
    cpu: TreeCpuAccount,
    read: Box<dyn FnMut() -> Option<Vec<ProcessTreeProcess>> + Send>,
    locate: Box<dyn FnMut(u32) -> Option<(ProcessStartKey, u32)> + Send>,
}

impl TreeSampler {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) fn new(
        read: impl FnMut() -> Option<Vec<ProcessTreeProcess>> + Send + 'static,
        locate: impl FnMut(u32) -> Option<(ProcessStartKey, u32)> + Send + 'static,
    ) -> Self {
        Self {
            cpu: TreeCpuAccount::new(),
            read: Box::new(read),
            locate: Box::new(locate),
        }
    }

    /// The next scope-shaped reading, or `None` when this sample's tree
    /// cannot be read. A miss does not move the baseline.
    pub(super) fn usage(&mut self) -> Option<ScopeUsage> {
        let current = (self.read)()?;
        self.cpu.observe(&current, &mut self.locate);
        let memory_bytes = current.iter().fold(0u64, |total, process| {
            total.saturating_add(process.phys_footprint)
        });
        Some(ScopeUsage {
            // The aggregator works in microseconds, matching `usage_usec`.
            cpu_usage_usec: self.cpu.cumulative_ns() / 1_000,
            memory_bytes,
        })
    }
}

/// The snapshots a worker rooted at `root` samples: its tree plus the orphans
/// still in a session it leads.
#[cfg(target_os = "macos")]
pub(super) fn session_tree_reader(
    root: u32,
) -> impl FnMut() -> Option<Vec<ProcessTreeProcess>> + Send + 'static {
    let mut tree = SessionTree::new(root);
    move || tree.snapshot()
}
