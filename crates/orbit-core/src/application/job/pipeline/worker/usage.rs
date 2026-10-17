//! Per-minute CPU and memory of a delivery run's own worker scope, and of the
//! host, for auto-concurrency profiles.
//!
//! Every [`SAMPLE_INTERVAL`] a background thread in the worker reads its own
//! `orbit-worker-*` scope, so attribution is exact and no central sampler is
//! needed:
//!
//! - **CPU** is `cpu.stat` `usage_usec` deltas over wall time, in cores. A
//!   minute's figure is the mean over the minute. Counter intervals are split
//!   proportionally by wall time at each boundary, before completed rows are
//!   written; activity within an interval is not observable.
//! - **Memory** is the scope's non-reclaimable bytes,
//!   `memory.current - file + shmem` ([`WorkerScopeCgroup::usage`]). A
//!   minute's figure is the maximum seen in it, because a short rustc or
//!   linker peak is what triggers the OOM killer.
//!
//! macOS has no worker scope. The same thread sums the worker's process tree
//! instead ([`super::usage_tree`]): CPU deltas of processes seen in both
//! samples, plus the full CPU time of processes that appeared, and the sum of
//! each process's `ri_phys_footprint`. The first read is a baseline. An
//! unreadable tree skips that sample and leaves the baseline in place.
//!
//! Each minute since the worker started becomes one `run_resource_minutes`
//! row, written as soon as the minute ends; the last, partial minute is
//! written when the run ends. The same thread offers the host's
//! `host_resource_minutes` row for each wall-clock minute it observed whole:
//! busy cores from native tick deltas and native used memory bytes (see
//! `runtime::host_resource::usage`), in the same units. Concurrent workers
//! offer the same minute and the first insert wins.
//!
//! A Linux worker with no scope (`machine.worker_containment = false`, no
//! user manager) starts no sampler, records nothing and logs nothing.
//! Recording is telemetry: a failed read skips a sample and a failed write
//! loses a row, and neither touches the run.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use orbit_store::contracts::{HostResourceMinute, ResourceUsageStoreBackend, RunResourceMinute};
use orbit_types::workflow::JobRun;

use super::delivery_class::DeliveryResourceClass;
use super::scope::{ScopeUsage, WorkerScopeCgroup};
use super::usage_tree::TreeSampler;
use crate::OrbitRuntime;
use crate::runtime::host_resource::{
    HostUsageReading, RESOURCE_SAMPLE_INTERVAL as SAMPLE_INTERVAL, read_host_usage,
};

const MINUTE: TimeDelta = TimeDelta::minutes(1);

/// The run minute being accumulated.
struct RunMinute {
    index: u32,
    cpu_usec: u64,
    wall_usec: u64,
    memory_max: u64,
    samples: u32,
}

/// The wall-clock host minute being accumulated.
struct HostMinute {
    at: DateTime<Utc>,
    /// Whether this worker had a reading from just before the minute began,
    /// so the minute's tick deltas cover all of it.
    whole: bool,
    busy_ticks: u64,
    total_ticks: u64,
    cpu_count: u32,
    memory_max: u64,
    memory_total: u64,
    samples: u32,
}

/// Where a sample's CPU counter and memory point come from.
enum ResourceSource {
    /// Linux worker scope. The cgroup counter already covers the whole tree,
    /// including processes that have exited.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Scope(WorkerScopeCgroup),
    /// macOS process tree, presented as a monotonic counter.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Tree(TreeSampler),
}

impl ResourceSource {
    fn usage(&mut self) -> Option<ScopeUsage> {
        match self {
            Self::Scope(scope) => scope.usage(),
            Self::Tree(tree) => tree.usage(),
        }
    }
}

/// Aggregates one worker's samples into minutes and writes each as it ends.
pub(crate) struct RunResourceSampler {
    store: Arc<dyn ResourceUsageStoreBackend>,
    source: ResourceSource,
    proc_root: PathBuf,
    workspace_id: String,
    run_id: String,
    class: DeliveryResourceClass,
    started_at: DateTime<Utc>,
    /// Minute index of `started_at`: past the last minute an earlier worker
    /// of the same run recorded, so a restarted worker never overwrites it.
    first_minute: u32,
    run: Option<RunMinute>,
    last_scope: Option<(DateTime<Utc>, u64)>,
    host: Option<HostMinute>,
    last_host: Option<(DateTime<Utc>, HostUsageReading)>,
}

fn positive_usec(duration: TimeDelta) -> Option<u64> {
    u64::try_from(duration.num_microseconds()?)
        .ok()
        .filter(|wall| *wall > 0)
}

/// Difference of cumulative proportional shares: rounding stays in the interval
/// and the sum of all segments is exactly the original counter delta.
fn interval_share(delta: u64, from: u64, to: u64, wall: u64) -> u64 {
    let scaled = |elapsed| u128::from(delta) * u128::from(elapsed) / u128::from(wall);
    (scaled(to) - scaled(from)) as u64
}

impl RunResourceSampler {
    /// A sampler for `run_id` that reads `scope` and `<proc_root>/{stat,meminfo}`.
    /// A run that already has minutes was restarted part way through: it
    /// continues after them and is marked resumed.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn new(
        store: Arc<dyn ResourceUsageStoreBackend>,
        scope: WorkerScopeCgroup,
        proc_root: PathBuf,
        workspace_id: String,
        run_id: String,
        class: DeliveryResourceClass,
        started_at: DateTime<Utc>,
    ) -> Self {
        Self::assemble(
            store,
            ResourceSource::Scope(scope),
            proc_root,
            workspace_id,
            run_id,
            class,
            started_at,
        )
    }

    /// A sampler that reads the process tree `read` returns. The same minute
    /// rows as [`Self::new`]. `read` returning `None` skips that sample.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn from_process_tree(
        store: Arc<dyn ResourceUsageStoreBackend>,
        read: impl FnMut() -> Option<Vec<orbit_common::process::ancestry::ProcessTreeProcess>>
        + Send
        + 'static,
        proc_root: PathBuf,
        workspace_id: String,
        run_id: String,
        class: DeliveryResourceClass,
        started_at: DateTime<Utc>,
    ) -> Self {
        Self::assemble(
            store,
            ResourceSource::Tree(TreeSampler::new(read)),
            proc_root,
            workspace_id,
            run_id,
            class,
            started_at,
        )
    }

    fn assemble(
        store: Arc<dyn ResourceUsageStoreBackend>,
        source: ResourceSource,
        proc_root: PathBuf,
        workspace_id: String,
        run_id: String,
        mut class: DeliveryResourceClass,
        started_at: DateTime<Utc>,
    ) -> Self {
        let first_minute = store
            .run_resource_minutes(&workspace_id, &run_id)
            .ok()
            .and_then(|minutes| minutes.last().map(|minute| minute.minute_index + 1))
            .unwrap_or(0);
        class.resumed |= first_minute > 0;
        Self {
            store,
            source,
            proc_root,
            workspace_id,
            run_id,
            class,
            started_at,
            first_minute,
            run: None,
            last_scope: None,
            host: None,
            last_host: None,
        }
    }

    /// Take one sample at `at`, writing every minute it completes.
    pub(crate) fn sample(&mut self, at: DateTime<Utc>) {
        if let Some(usage) = self.source.usage() {
            self.sample_scope(at, usage.cpu_usage_usec, usage.memory_bytes);
        }
        if let Some(reading) = read_host_usage(&self.proc_root) {
            self.sample_host(at, reading);
        }
    }

    /// Take a last sample at `at` and write the run's final, partial minute.
    /// The host's partial minute is left for a worker that sees it whole.
    pub(crate) fn finish(&mut self, at: DateTime<Utc>) {
        self.sample(at);
        if let Some(minute) = self.run.take() {
            self.write_run_minute(minute);
        }
    }

    fn sample_scope(&mut self, at: DateTime<Utc>, cpu_usec: u64, memory_bytes: u64) {
        let Some(index) = self.run_index(at) else {
            return;
        };
        if self
            .last_scope
            .is_some_and(|(previous_at, _)| at < previous_at)
        {
            // The wall clock stepped back; keep the minute being built.
            return;
        }
        if let Some((previous_at, previous_usec)) = self.last_scope
            && let (Some(wall), Some(cpu)) = (
                positive_usec(at - previous_at),
                cpu_usec.checked_sub(previous_usec),
            )
        {
            let mut cursor = previous_at;
            let mut covered = 0;
            while cursor < at {
                let Some(segment_index) = self.run_index(cursor) else {
                    break;
                };
                let boundary = self.started_at
                    + TimeDelta::minutes(i64::from(segment_index - self.first_minute) + 1);
                let end = at.min(boundary);
                // Use a common origin for truncated microsecond durations so
                // sub-microsecond rounding cannot lose time at a boundary.
                let next_covered = positive_usec(end - previous_at).unwrap_or(0);
                // Wall coverage uses the run origin across samples too, so
                // a fully covered minute always has exactly 60 seconds.
                let duration = positive_usec(end - self.started_at).unwrap_or(0)
                    - positive_usec(cursor - self.started_at).unwrap_or(0);
                let minute = self.run_minute(segment_index);
                minute.cpu_usec = minute.cpu_usec.saturating_add(interval_share(
                    cpu,
                    covered,
                    next_covered,
                    wall,
                ));
                minute.wall_usec = minute.wall_usec.saturating_add(duration);
                covered = next_covered;
                cursor = end;
            }
        }
        let minute = self.run_minute(index);
        minute.memory_max = minute.memory_max.max(memory_bytes);
        minute.samples += 1;
        self.last_scope = Some((at, cpu_usec));
    }

    fn run_index(&self, at: DateTime<Utc>) -> Option<u32> {
        let elapsed = (at - self.started_at).num_microseconds()?;
        let minute = u32::try_from(elapsed.checked_div(60_000_000)?).ok()?;
        (elapsed >= 0).then(|| self.first_minute.saturating_add(minute))
    }

    fn run_minute(&mut self, index: u32) -> &mut RunMinute {
        if self.run.as_ref().is_some_and(|minute| index > minute.index)
            && let Some(done) = self.run.take()
        {
            self.write_run_minute(done);
        }
        self.run.get_or_insert(RunMinute {
            index,
            cpu_usec: 0,
            wall_usec: 0,
            memory_max: 0,
            samples: 0,
        })
    }

    fn sample_host(&mut self, at: DateTime<Utc>, reading: HostUsageReading) {
        let Ok(minute_at) = at.duration_trunc(MINUTE) else {
            return;
        };
        if self
            .last_host
            .is_some_and(|(previous_at, _)| at < previous_at)
        {
            return;
        }
        if let Some((previous_at, previous)) = self.last_host
            && let (Some(wall), Some((busy, total))) =
                (positive_usec(at - previous_at), reading.cpu_delta(previous))
        {
            let mut cursor = previous_at;
            let mut covered = 0;
            while cursor < at {
                let Ok(segment_at) = cursor.duration_trunc(MINUTE) else {
                    break;
                };
                let end = at.min(segment_at + MINUTE);
                // Use a common origin for truncated microsecond durations so
                // sub-microsecond rounding cannot lose time at a boundary.
                let next_covered = positive_usec(end - previous_at).unwrap_or(0);
                let minute = self.host_minute(segment_at, reading, cursor == segment_at);
                let busy_share = interval_share(busy, covered, next_covered, wall);
                // Split idle separately so integer rounding cannot produce a
                // segment with more busy ticks than total ticks.
                let idle_share = interval_share(total - busy, covered, next_covered, wall);
                minute.total_ticks += busy_share + idle_share;
                minute.busy_ticks += busy_share;
                covered = next_covered;
                cursor = end;
            }
        }
        // The point reading belongs to the minute containing `at`, while the
        // preceding counter interval has already been credited and flushed.
        let whole = self
            .last_host
            .is_some_and(|(previous_at, _)| previous_at <= minute_at);
        let minute = self.host_minute(minute_at, reading, whole);
        minute.cpu_count = minute.cpu_count.max(reading.cpu_count);
        minute.memory_max = minute.memory_max.max(reading.memory_used_bytes);
        minute.memory_total = minute.memory_total.max(reading.memory_total_bytes);
        minute.samples += 1;
        self.last_host = Some((at, reading));
    }

    fn host_minute(
        &mut self,
        at: DateTime<Utc>,
        reading: HostUsageReading,
        whole: bool,
    ) -> &mut HostMinute {
        if self.host.as_ref().is_some_and(|minute| at > minute.at)
            && let Some(done) = self.host.take()
            && done.whole
        {
            self.offer_host_minute(done);
        }
        self.host.get_or_insert(HostMinute {
            at,
            whole,
            busy_ticks: 0,
            total_ticks: 0,
            cpu_count: reading.cpu_count,
            memory_max: 0,
            memory_total: reading.memory_total_bytes,
            samples: 0,
        })
    }

    fn write_run_minute(&self, minute: RunMinute) {
        let offset = TimeDelta::minutes(i64::from(minute.index - self.first_minute));
        let row = RunResourceMinute {
            workspace_id: self.workspace_id.clone(),
            run_id: self.run_id.clone(),
            minute_index: minute.index,
            minute_started_at: self.started_at + offset,
            task_id: self.class.task_id.clone(),
            work_class: self.class.work_class.clone(),
            complexity: self.class.complexity.clone(),
            resumed: self.class.resumed,
            delivery_admitted_at: Some(self.class.admitted_at),
            cpu_cores: if minute.wall_usec > 0 {
                minute.cpu_usec as f64 / minute.wall_usec as f64
            } else {
                0.0
            },
            cpu_accounting: if cfg!(target_os = "macos") {
                Some(orbit_store::contracts::CPU_ACCOUNTING_MACOS.to_string())
            } else if cfg!(target_os = "linux") {
                Some(orbit_store::contracts::CPU_ACCOUNTING_LINUX.to_string())
            } else {
                None
            },
            memory_bytes: minute.memory_max,
            samples: minute.samples,
        };
        if let Err(error) = self.store.record_run_resource_minute(&row) {
            tracing::debug!(
                target: "orbit.core.job_run",
                run_id = self.run_id,
                error = %error,
                "could not record a run resource minute",
            );
        }
    }

    fn offer_host_minute(&self, minute: HostMinute) {
        if minute.total_ticks == 0 {
            return;
        }
        let row = HostResourceMinute {
            minute_at: minute.at,
            cpu_cores: minute.busy_ticks as f64 / minute.total_ticks as f64
                * f64::from(minute.cpu_count),
            cpu_count: minute.cpu_count,
            memory_bytes: minute.memory_max,
            memory_total_bytes: minute.memory_total,
            samples: minute.samples,
            recorded_by_run_id: Some(self.run_id.clone()),
        };
        if let Err(error) = self.store.offer_host_resource_minute(&row) {
            tracing::debug!(
                target: "orbit.core.job_run",
                run_id = self.run_id,
                error = %error,
                "could not record a host resource minute",
            );
        }
    }
}

/// The sampling thread. Dropping it takes a last sample, writes the final
/// minute and joins the thread.
pub(crate) struct ResourceSamplerThread {
    stop: Option<mpsc::SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl ResourceSamplerThread {
    pub(crate) fn spawn(mut sampler: RunResourceSampler) -> Option<Self> {
        // Nothing is ever sent: dropping the sender is the stop signal.
        let (stop, stopped) = mpsc::sync_channel::<()>(1);
        let thread = thread::Builder::new()
            .name("orbit-resource-sampler".into())
            .spawn(move || {
                sampler.sample(Utc::now());
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(SAMPLE_INTERVAL) {
                    sampler.sample(Utc::now());
                }
                sampler.finish(Utc::now());
            })
            .ok()?;
        Some(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for ResourceSamplerThread {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl OrbitRuntime {
    /// Start sampling this worker for `run`, or `None` when there is nothing
    /// to sample or the run is not a delivery. Linux samples the worker's
    /// scope and skips a worker that has none. macOS has no scopes and
    /// samples the worker's process tree. `adopted` marks a worker that took
    /// the run over mid-flight.
    pub(super) fn start_resource_sampler(
        &self,
        run: &JobRun,
        adopted: bool,
    ) -> Option<ResourceSamplerThread> {
        #[cfg(target_os = "linux")]
        {
            self.start_resource_sampler_in(WorkerScopeCgroup::of_current_process(), run, adopted)
        }
        #[cfg(target_os = "macos")]
        {
            self.start_process_tree_sampler(run, adopted)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (run, adopted);
            None
        }
    }

    /// Sample `run` from this process's tree. `None` when the run is not a
    /// delivery. An unreadable tree skips samples; it does not stop the run.
    #[cfg(target_os = "macos")]
    fn start_process_tree_sampler(
        &self,
        run: &JobRun,
        adopted: bool,
    ) -> Option<ResourceSamplerThread> {
        let class = self.delivery_resource_class(run, adopted)?;
        let store: Arc<dyn ResourceUsageStoreBackend> = Arc::new(self.sqlite_store().ok()?);
        let workspace_id = self.workspace_id().ok()?;
        ResourceSamplerThread::spawn(RunResourceSampler::from_process_tree(
            store,
            || orbit_common::process::ancestry::process_tree(std::process::id()),
            PathBuf::from("/proc"),
            workspace_id,
            run.run_id.clone(),
            class,
            Utc::now(),
        ))
    }

    /// [`Self::start_resource_sampler`] for the scope this worker resolved.
    /// Linux tests inject a fixture scope. A missing scope records nothing,
    /// including on macOS, where production uses the process tree instead.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn start_resource_sampler_in(
        &self,
        scope: Option<WorkerScopeCgroup>,
        run: &JobRun,
        adopted: bool,
    ) -> Option<ResourceSamplerThread> {
        let scope = scope?;
        let class = self.delivery_resource_class(run, adopted)?;
        let store: Arc<dyn ResourceUsageStoreBackend> = Arc::new(self.sqlite_store().ok()?);
        let workspace_id = self.workspace_id().ok()?;
        ResourceSamplerThread::spawn(RunResourceSampler::new(
            store,
            scope,
            PathBuf::from("/proc"),
            workspace_id,
            run.run_id.clone(),
            class,
            Utc::now(),
        ))
    }
}
