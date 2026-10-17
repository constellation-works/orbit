//! macOS resource samples from the worker's process tree.
//!
//! Linux charges a scope's single cumulative counter. A process tree has no
//! such counter: a child that exits takes its CPU out of the sum, and a naive
//! subtraction goes negative or, if the remaining total is substituted, spikes
//! with a survivor's lifetime. [`TreeCpuAccount`] charges each process's own
//! plus reaped-children CPU, so short-lived descendants count through the
//! parent that waited for them, and takes back the last observed total of
//! each process that left so a sampled child is not counted again when
//! reaped. The first successful read is a baseline and charges nothing. An
//! unreadable tree leaves that baseline where it was, so the next success
//! still has a previous sample to diff.

use orbit_common::process::ancestry::{ProcessTreeProcess, TreeCpuAccount};

use super::scope::ScopeUsage;

/// Turns process-tree snapshots into the monotonic usage the minute aggregator
/// already records for a cgroup scope.
pub(super) struct TreeSampler {
    cpu: TreeCpuAccount,
    read: Box<dyn FnMut() -> Option<Vec<ProcessTreeProcess>> + Send>,
}

impl TreeSampler {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) fn new(
        read: impl FnMut() -> Option<Vec<ProcessTreeProcess>> + Send + 'static,
    ) -> Self {
        Self {
            cpu: TreeCpuAccount::new(),
            read: Box::new(read),
        }
    }

    /// The next scope-shaped reading, or `None` when this sample's tree
    /// cannot be read. A miss does not move the baseline.
    pub(super) fn usage(&mut self) -> Option<ScopeUsage> {
        let current = (self.read)()?;
        self.cpu.observe(&current);
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
