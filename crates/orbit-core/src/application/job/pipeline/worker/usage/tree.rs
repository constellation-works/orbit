//! macOS resource samples from the worker's process tree.
//!
//! Linux charges a scope's single cumulative counter. A process tree has no
//! such counter: a child that exits takes its CPU out of the sum, and a naive
//! subtraction goes negative or, if the remaining total is substituted, spikes
//! with a survivor's lifetime. [`TreeCpuAccount`] charges each process's own
//! plus reaped-children CPU, so short-lived descendants count through the
//! parent that waited for them. The last observed total of a process that
//! exited is owed by the member that will reap it, so a sampled child is not
//! counted again when reaped; one reparented away owes nothing, but keeps its
//! last CPU total so adopting it after a missed snapshot charges only growth.
//! A debt the owner's next sighting does not settle lapses, so a descendant reaped
//! outside the tree hides at most one window of the owner's reaped growth. The first
//! successful read is a baseline and charges nothing. An unreadable tree
//! leaves that baseline where it was, so the next success still has a
//! previous sample to diff.
//!
//! The tree a worker samples is a `SessionTree`: the worker leads its own
//! session, so a descendant orphaned to launchd is still walked, and so is
//! one in a session a member started.

#[cfg(target_os = "macos")]
use orbit_common::process::ancestry::SessionTree;
use orbit_common::process::ancestry::{ProcessStartKey, ProcessTreeProcess, TreeCpuAccount};

use super::super::scope::ScopeUsage;

/// Turns process-tree snapshots into the monotonic usage the minute aggregator
/// already records for a cgroup scope.
pub(in crate::application::job::pipeline::worker) struct TreeSampler {
    cpu: TreeCpuAccount,
    #[cfg(target_os = "macos")]
    pub(super) latest: Vec<orbit_store::contracts::WorkerProcessResources>,
    #[cfg(target_os = "macos")]
    previous: std::collections::HashMap<ProcessStartKey, u64>,
    #[cfg(target_os = "macos")]
    last_read: Option<std::time::Instant>,
    read: Box<dyn FnMut() -> Option<Vec<ProcessTreeProcess>> + Send>,
    locate: Box<dyn FnMut(u32) -> Option<(ProcessStartKey, u32)> + Send>,
}

impl TreeSampler {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(in crate::application::job::pipeline::worker) fn new(
        read: impl FnMut() -> Option<Vec<ProcessTreeProcess>> + Send + 'static,
        locate: impl FnMut(u32) -> Option<(ProcessStartKey, u32)> + Send + 'static,
    ) -> Self {
        Self {
            cpu: TreeCpuAccount::new(),
            #[cfg(target_os = "macos")]
            latest: Vec::new(),
            #[cfg(target_os = "macos")]
            previous: std::collections::HashMap::new(),
            #[cfg(target_os = "macos")]
            last_read: None,
            read: Box::new(read),
            locate: Box::new(locate),
        }
    }

    /// The next scope-shaped reading, or `None` when this sample's tree
    /// cannot be read. A miss does not move the baseline.
    pub(in crate::application::job::pipeline::worker) fn usage(&mut self) -> Option<ScopeUsage> {
        let current = (self.read)()?;
        #[cfg(target_os = "macos")]
        self.track_process_levels(&current);
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

    #[cfg(target_os = "macos")]
    fn track_process_levels(&mut self, current: &[ProcessTreeProcess]) {
        let now = std::time::Instant::now();
        let wall = self
            .last_read
            .map(|before| now.duration_since(before).as_secs_f64());
        self.latest = current
            .iter()
            .map(|process| {
                let cpu_cores = wall.filter(|wall| *wall > 0.0).and_then(|wall| {
                    self.previous.get(&process.key).map(|before| {
                        process.cpu_time_ns.saturating_sub(*before) as f64 / 1e9 / wall
                    })
                });
                orbit_store::contracts::WorkerProcessResources {
                    pid: process.key.pid,
                    starttime: process.key.starttime,
                    command: orbit_common::process::ancestry::process_command_name(process.key)
                        .unwrap_or_else(|| "<unavailable>".into()),
                    memory_bytes: process.phys_footprint,
                    cpu_cores,
                }
            })
            .collect();
        self.previous = current
            .iter()
            .map(|process| (process.key, process.cpu_time_ns))
            .collect();
        self.last_read = Some(now);
    }
}

/// The snapshots a worker rooted at `root` samples: its tree plus the orphans
/// still in a session it leads.
#[cfg(target_os = "macos")]
pub(in crate::application::job::pipeline::worker) fn session_tree_reader(
    root: u32,
) -> impl FnMut() -> Option<Vec<ProcessTreeProcess>> + Send + 'static {
    let mut tree = SessionTree::new(root);
    move || tree.snapshot()
}
