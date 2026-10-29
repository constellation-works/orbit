//! Polling containment inside the worker, so it survives the submitting CLI.
//! Descendants receive TERM then KILL; the worker records failure and ends last.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::process::ancestry::{self, ProcessStartKey};
use orbit_config::{MemoryLimit, MemoryUnit};
use orbit_store::contracts::{WorkerResourceLimitEvidence, WorkerResourceLimitStoreBackend};
use orbit_types::workflow::{JobRun, JobRunState};

use super::tree::{self, TreeSampler};
use crate::OrbitRuntime;
use crate::application::job::pipeline::V2RunFinalizationOptions;
use crate::application::job::pipeline::worker::scope::WORKER_RESOURCE_LIMIT_ERROR_CODE;

// Separate from the five-second profile/host sampler: reduce overshoot without
// changing profile accounting or scanning continuously on a shared host.
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(1);
const TERM_GRACE: Duration = Duration::from_secs(1);

pub(in crate::application::job::pipeline::worker) struct WorkerWatchdog {
    stop: Option<mpsc::SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for WorkerWatchdog {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl OrbitRuntime {
    pub(in crate::application::job::pipeline::worker) fn start_worker_watchdog(
        &self,
        run: &JobRun,
    ) -> Result<Option<WorkerWatchdog>, OrbitError> {
        let settings = self.context.settings().worker_containment();
        if !settings.enabled {
            return Ok(None);
        }
        let memory_max = watchdog_memory_bytes(settings.memory_max)?;
        let tasks_max = settings.tasks_max;
        // Resolve store prerequisites before executing any job activity. A
        // missing watchdog must fail the launch, never silently degrade.
        let store = self.sqlite_store()?;
        let workspace_id = self.workspace_id()?;
        let runtime = self.clone();
        let run = run.clone();
        let (stop, stopped) = mpsc::sync_channel::<()>(1);
        let thread = thread::Builder::new()
            .name("orbit-worker-watchdog".into())
            .spawn(move || {
                let mut tree = TreeSampler::new(
                    tree::session_tree_reader(std::process::id()),
                    ancestry::process_start_and_parent,
                );
                loop {
                    if let Some(usage) = tree.usage()
                        && (memory_max.is_some_and(|max| usage.memory_bytes > max)
                            || tree.latest.len() > tasks_max as usize)
                    {
                        let mut top = tree.latest.clone();
                        top.sort_by_key(|process| {
                            (std::cmp::Reverse(process.memory_bytes), process.pid)
                        });
                        top.truncate(8);
                        let evidence = WorkerResourceLimitEvidence {
                            workspace_id: workspace_id.clone(),
                            run_id: run.run_id.clone(),
                            observed_at: Utc::now(),
                            memory_bytes: usage.memory_bytes,
                            process_count: tree.latest.len(),
                            memory_max_bytes: memory_max,
                            tasks_max,
                            top_processes: top,
                        };
                        let message = describe_watchdog_breach(&evidence);
                        if let Err(error) = store.record_worker_resource_limit_evidence(&evidence) {
                            tracing::error!(%error, "could not persist watchdog resource evidence");
                        }
                        let started_at = run.started_at.unwrap_or(run.scheduled_at);
                        if let Err(error) = crate::application::job::pipeline::worker::record::watchdog_diagnostic_step(
                            runtime.stores().jobs(),
                            &run,
                            started_at,
                            Utc::now(),
                            Some(WORKER_RESOURCE_LIMIT_ERROR_CODE),
                            &message,
                            JobRunState::Failed,
                        ) {
                            tracing::error!(%error, "could not persist watchdog diagnostic");
                        }
                        tracing::warn!(run_id = run.run_id, "{message}");
                        stop_tracked_descendants(&mut tree);
                        // The executing thread joins this watchdog before its own
                        // finalization, so it cannot turn killed-tool EOF into success.
                        let error = OrbitError::Execution(message);
                        if let Err(error) = runtime.finalize_v2_pipeline_run(
                            &run,
                            &run.input.clone().unwrap_or_default(),
                            started_at,
                            Utc::now(),
                            Err(&error),
                            V2RunFinalizationOptions::DETACHED_WORKER,
                        ) {
                            tracing::error!(%error, "could not finalize watchdog failure");
                        }
                        if let Some(key) = ancestry::process_start_key(std::process::id()) {
                            signal_tracked(key, libc::SIGTERM);
                            thread::sleep(TERM_GRACE);
                            signal_tracked(key, libc::SIGKILL);
                        }
                        return;
                    }
                    match stopped.recv_timeout(WATCHDOG_INTERVAL) {
                        Err(RecvTimeoutError::Timeout) => {}
                        _ => return,
                    }
                }
            })
            .map_err(|error| OrbitError::Execution(format!("start worker watchdog: {error}")))?;
        Ok(Some(WorkerWatchdog {
            stop: Some(stop),
            thread: Some(thread),
        }))
    }
}

fn watchdog_memory_bytes(limit: MemoryLimit) -> Result<Option<u64>, OrbitError> {
    let bytes = match limit {
        MemoryLimit::Infinity => return Ok(None),
        MemoryLimit::Bytes { amount, unit } => {
            let shift = match unit {
                None => 0,
                Some(MemoryUnit::K) => 10,
                Some(MemoryUnit::M) => 20,
                Some(MemoryUnit::G) => 30,
                Some(MemoryUnit::T) => 40,
            };
            amount.saturating_mul(1u64 << shift)
        }
        MemoryLimit::Percent(percent) => {
            let host = crate::runtime::host_resource::read_host_usage(Path::new("/proc"))
                .ok_or_else(|| {
                    OrbitError::Execution(
                        "cannot resolve watchdog memory limit from physical RAM".into(),
                    )
                })?;
            ((u128::from(host.memory_total_bytes) * u128::from(percent)) / 100) as u64
        }
    };
    Ok(Some(bytes))
}

fn describe_watchdog_breach(evidence: &WorkerResourceLimitEvidence) -> String {
    let top = evidence
        .top_processes
        .iter()
        .map(|process| {
            format!(
                "{} (pid {}, {} bytes)",
                process.command, process.pid, process.memory_bytes,
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "worker watchdog resource limit: memory={} bytes (machine.worker_memory_max={}), processes={} (machine.worker_tasks_max={}); top consumers: {top}",
        evidence.memory_bytes,
        evidence
            .memory_max_bytes
            .map(|value| value.to_string())
            .unwrap_or_else(|| "infinity".into()),
        evidence.process_count,
        evidence.tasks_max
    )
}

fn signal_tracked(key: ProcessStartKey, signal: i32) {
    if let Err(error) = ancestry::bind_process_start_key(key)
        .and_then(|process| process.map(|process| process.signal(signal)).transpose())
    {
        tracing::error!(pid = key.pid, signal, %error, "could not signal tracked worker process");
    }
}

fn stop_tracked_descendants(tree: &mut TreeSampler) {
    let own_pid = std::process::id();
    let mut tracked = HashMap::new();
    let mut pending = HashSet::new();
    let mut reported = HashSet::new();
    let deadline = Instant::now() + TERM_GRACE;
    loop {
        for process in &tree.latest {
            let key = ProcessStartKey {
                pid: process.pid,
                starttime: process.starttime,
            };
            if key.pid != own_pid && !tracked.contains_key(&key) {
                pending.insert(key);
            }
        }
        for key in pending.clone() {
            match ancestry::bind_process_start_key(key) {
                Ok(Some(process)) => {
                    if let Err(error) = process.signal(libc::SIGTERM) {
                        tracing::error!(pid = key.pid, %error, "watchdog TERM failed");
                    }
                    tracked.insert(key, process);
                    pending.remove(&key);
                }
                Ok(None) => {
                    pending.remove(&key);
                }
                Err(error) if reported.insert(key) => {
                    tracing::error!(pid = key.pid, %error, "watchdog identity binding failed; retaining ownership and retrying");
                }
                Err(_) => {}
            }
        }
        if Instant::now() >= deadline {
            for (key, process) in &tracked {
                if let Err(error) = process.signal(libc::SIGKILL)
                    && reported.insert(*key)
                {
                    tracing::error!(pid = key.pid, %error, "watchdog KILL failed; retaining ownership and retrying");
                }
            }
            // Releasing reservations requires stopped children, not just a
            // signal syscall. Unreadable bindings retain ownership too.
            if pending.is_empty()
                && tracked
                    .values()
                    .all(|process| !process.is_alive().unwrap_or(true))
            {
                return;
            }
        }
        thread::sleep(Duration::from_millis(100));
        // Adopt newly orphaned members and children started during TERM grace.
        // A failed read preserves the last successful set for escalation.
        let _ = tree.usage();
    }
}
