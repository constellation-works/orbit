//! macOS resource samples from the worker's process tree.
//!
//! Linux charges a scope's single cumulative counter. A process tree has no
//! such counter: a child that exits takes its CPU out of the sum, and a naive
//! subtraction goes negative or, if the remaining total is substituted, spikes
//! with a survivor's lifetime. The interval charged here is the non-negative
//! delta of every process present in both samples, plus the full CPU time of
//! a process that was not in the previous sample. The first successful read
//! is a baseline and charges nothing. An unreadable tree leaves that baseline
//! where it was, so the next success still has a previous sample to diff.

use std::collections::HashMap;

use orbit_common::process::ancestry::{ProcessStartKey, ProcessTreeProcess};

use super::scope::ScopeUsage;

/// CPU nanoseconds to charge between two snapshots.
pub(super) fn accounted_cpu_ns(
    previous: &HashMap<ProcessStartKey, u64>,
    current: &[ProcessTreeProcess],
) -> u64 {
    current.iter().fold(0u64, |total, process| {
        let delta = match previous.get(&process.key) {
            Some(before) => process.cpu_time_ns.saturating_sub(*before),
            None => process.cpu_time_ns,
        };
        total.saturating_add(delta)
    })
}

/// Turns process-tree snapshots into the monotonic usage the minute aggregator
/// already records for a cgroup scope.
pub(super) struct TreeSampler {
    previous: HashMap<ProcessStartKey, u64>,
    /// Set once a snapshot has been kept. Until then every process would look
    /// new, and the worker's lifetime CPU would land in the first interval.
    baseline: bool,
    cumulative_ns: u64,
    read: Box<dyn FnMut() -> Option<Vec<ProcessTreeProcess>> + Send>,
}

impl TreeSampler {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) fn new(
        read: impl FnMut() -> Option<Vec<ProcessTreeProcess>> + Send + 'static,
    ) -> Self {
        Self {
            previous: HashMap::new(),
            baseline: false,
            cumulative_ns: 0,
            read: Box::new(read),
        }
    }

    /// The next scope-shaped reading, or `None` when this sample's tree
    /// cannot be read. A miss does not move the baseline.
    pub(super) fn usage(&mut self) -> Option<ScopeUsage> {
        let current = (self.read)()?;
        let cpu_ns = if self.baseline {
            accounted_cpu_ns(&self.previous, &current)
        } else {
            0
        };
        self.previous.clear();
        self.previous.extend(
            current
                .iter()
                .map(|process| (process.key, process.cpu_time_ns)),
        );
        self.baseline = true;
        self.cumulative_ns = self.cumulative_ns.saturating_add(cpu_ns);
        let memory_bytes = current.iter().fold(0u64, |total, process| {
            total.saturating_add(process.phys_footprint)
        });
        Some(ScopeUsage {
            // The aggregator works in microseconds, matching `usage_usec`.
            cpu_usage_usec: self.cumulative_ns / 1_000,
            memory_bytes,
        })
    }
}
