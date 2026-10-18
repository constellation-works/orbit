//! Building profiles from the stored per-run minutes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_config::AutoConcurrencySettings;
use orbit_store::contracts::{
    CPU_ACCOUNTING_LINUX, CPU_ACCOUNTING_MACOS, CPU_ACCOUNTING_MACOS_OWN_ONLY,
    CPU_ACCOUNTING_MACOS_TREE_ONLY, DeliveryRunResources,
};
use orbit_types::workflow::{AUTO_TASK_TAG_PREFIX, auto_task_builds_by_design};

use super::{
    BUILD_LIKE_CORES, BUILD_LIKE_MINUTES, BaseClass, BuildLikeDelivery, ProfileDelivery,
    ProfileKey, ProfileMinute, ProfilePeak, ProfileQuantile, ProfileSet, ResourceProfile,
};

/// `alive` below this ends the horizon: the 90th-percentile duration.
const HORIZON_ALIVE: f64 = 0.1;
/// Memory is read at the 90th percentile, because too much memory kills a
/// process where too much CPU only slows everyone down.
const MEMORY_PERCENTILE: (usize, usize) = (9, 10);

/// The `[machine.auto_concurrency]` bounds a profile build reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProfileBounds {
    pub(crate) window: usize,
    pub(crate) min_deliveries: usize,
    pub(crate) horizon_cap_minutes: u32,
}

impl From<&AutoConcurrencySettings> for ProfileBounds {
    fn from(settings: &AutoConcurrencySettings) -> Self {
        Self {
            window: usize::try_from(settings.profile_window).unwrap_or(usize::MAX),
            min_deliveries: usize::try_from(settings.min_profile_deliveries).unwrap_or(usize::MAX),
            horizon_cap_minutes: settings.horizon_cap_minutes,
        }
    }
}

/// One delivery: the runs of one task sharing an admission time.
struct Delivery {
    workspace_id: String,
    task_id: String,
    work_class: String,
    complexity: Option<String>,
    admitted_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    live: bool,
    resumed: bool,
    untrusted_cpu: bool,
    run_ids: Vec<String>,
    /// Summed cores and memory per minute since admission.
    cpu: Vec<f64>,
    memory: Vec<u64>,
}

impl Delivery {
    fn minutes(&self) -> usize {
        self.cpu.len()
    }

    fn template(&self) -> Option<&str> {
        self.work_class.strip_prefix(AUTO_TASK_TAG_PREFIX)
    }

    fn finished(&self) -> DateTime<Utc> {
        self.finished_at.unwrap_or(self.admitted_at)
    }
}

/// The task ids of every template delivery among `runs`, whose
/// `no-diff-expected` tags decide each template's base class.
pub(crate) fn template_task_ids(runs: &[DeliveryRunResources]) -> BTreeSet<String> {
    runs.iter()
        .filter(|run| run.work_class.starts_with(AUTO_TASK_TAG_PREFIX))
        .map(|run| run.task_id.clone())
        .collect()
}

/// Build every profile from `runs`. `no_diff_tasks` names the template tasks
/// tagged `no-diff-expected`.
pub(crate) fn build_profiles(
    runs: Vec<DeliveryRunResources>,
    no_diff_tasks: &BTreeSet<String>,
    bounds: ProfileBounds,
    now: DateTime<Utc>,
) -> ProfileSet {
    let mut by_key: BTreeMap<ProfileKey, Vec<Delivery>> = BTreeMap::new();
    for delivery in deliveries(runs)
        .into_iter()
        .filter(|delivery| !delivery.live && !delivery.resumed && !delivery.untrusted_cpu)
    {
        let complexity = if delivery.template().is_some() {
            None
        } else {
            delivery.complexity.clone()
        };
        let key = ProfileKey {
            workspace_id: delivery.workspace_id.clone(),
            work_class: delivery.work_class.clone(),
            complexity,
        };
        by_key.entry(key).or_default().push(delivery);
    }
    for deliveries in by_key.values_mut() {
        deliveries.sort_by(|a, b| {
            b.finished()
                .cmp(&a.finished())
                .then(b.admitted_at.cmp(&a.admitted_at))
        });
    }

    let mut profiles = Vec::new();
    let mut by_class: BTreeMap<ProfileKey, Vec<&Delivery>> = BTreeMap::new();
    for (key, deliveries) in &by_key {
        let base = match key.template() {
            Some(_) => {
                if deliveries
                    .first()
                    .is_some_and(|newest| no_diff_tasks.contains(&newest.task_id))
                {
                    BaseClass::NoDiff
                } else {
                    BaseClass::Change
                }
            }
            None => base_of(&key.work_class),
        };
        by_class
            .entry(ProfileKey {
                workspace_id: key.workspace_id.clone(),
                work_class: base.as_str().to_string(),
                complexity: None,
            })
            .or_default()
            .extend(deliveries.iter());
        let window: Vec<&Delivery> = deliveries.iter().take(bounds.window).collect();
        profiles.push(Arc::new(profile(key.clone(), base, &window, bounds)));
    }
    let class_profiles = by_class
        .into_iter()
        .map(|(key, mut deliveries)| {
            deliveries.sort_by(|a, b| {
                b.finished()
                    .cmp(&a.finished())
                    .then(b.admitted_at.cmp(&a.admitted_at))
            });
            deliveries.truncate(bounds.window);
            let base = base_of(&key.work_class);
            Arc::new(profile(key, base, &deliveries, bounds))
        })
        .collect();
    ProfileSet {
        built_at: now,
        profiles,
        class_profiles,
    }
}

fn base_of(work_class: &str) -> BaseClass {
    if work_class == BaseClass::NoDiff.as_str() {
        BaseClass::NoDiff
    } else {
        BaseClass::Change
    }
}

/// Group runs into deliveries and sum their minutes on the admission clock.
fn deliveries(runs: Vec<DeliveryRunResources>) -> Vec<Delivery> {
    let mut grouped: BTreeMap<(String, String, DateTime<Utc>), Delivery> = BTreeMap::new();
    for run in runs {
        let delivery = grouped
            .entry((
                run.workspace_id.clone(),
                run.task_id.clone(),
                run.delivery_admitted_at,
            ))
            .or_insert_with(|| Delivery {
                workspace_id: run.workspace_id.clone(),
                task_id: run.task_id.clone(),
                work_class: run.work_class.clone(),
                complexity: run.complexity.clone(),
                admitted_at: run.delivery_admitted_at,
                finished_at: None,
                live: false,
                resumed: false,
                untrusted_cpu: false,
                run_ids: Vec::new(),
                cpu: Vec::new(),
                memory: Vec::new(),
            });
        delivery.resumed |= run.resumed;
        // Exclude the whole delivery, including any good sibling runs: dropping
        // just bad minutes would make a partial series understate its load.
        delivery.untrusted_cpu |=
            run.minutes
                .iter()
                .any(|minute| match minute.cpu_accounting.as_deref() {
                    Some(
                        CPU_ACCOUNTING_LINUX
                        | CPU_ACCOUNTING_MACOS
                        | CPU_ACCOUNTING_MACOS_TREE_ONLY,
                    ) => false,
                    // Missed reaped descendants and under-measured the load.
                    Some(CPU_ACCOUNTING_MACOS_OWN_ONLY) => true,
                    None => !cfg!(target_os = "linux"),
                    Some(_) => true,
                });
        // A run whose `job_runs` row is gone cannot say it finished.
        match run.finished_at {
            Some(at) if run.job_id.is_some() => {
                delivery.finished_at = Some(delivery.finished_at.map_or(at, |seen| seen.max(at)));
            }
            _ => delivery.live = true,
        }
        delivery.run_ids.push(run.run_id);
        for minute in run.minutes {
            let Some(offset) = minutes_between(delivery.admitted_at, minute.minute_started_at)
            else {
                continue;
            };
            if delivery.cpu.len() <= offset {
                delivery.cpu.resize(offset + 1, 0.0);
                delivery.memory.resize(offset + 1, 0);
            }
            delivery.cpu[offset] += minute.cpu_cores;
            delivery.memory[offset] = delivery.memory[offset].saturating_add(minute.memory_bytes);
        }
    }
    grouped
        .into_values()
        .map(|mut delivery| {
            // Alive until the last run ended, sampled or not.
            let ran = delivery
                .finished_at
                .and_then(|finished| {
                    let seconds = (finished - delivery.admitted_at).num_seconds();
                    u64::try_from(seconds)
                        .ok()
                        .and_then(|seconds| usize::try_from(seconds.div_ceil(60)).ok())
                })
                .unwrap_or(0);
            let minutes = delivery.cpu.len().max(ran);
            delivery.cpu.resize(minutes, 0.0);
            delivery.memory.resize(minutes, 0);
            delivery.run_ids.sort();
            delivery
        })
        .collect()
}

/// Whole minutes from `from` to `at`, or `None` when `at` is earlier.
fn minutes_between(from: DateTime<Utc>, at: DateTime<Utc>) -> Option<usize> {
    usize::try_from((at - from).num_seconds().div_euclid(60)).ok()
}

fn profile(
    key: ProfileKey,
    base_class: BaseClass,
    window: &[&Delivery],
    bounds: ProfileBounds,
) -> ResourceProfile {
    let longest = window.iter().map(|d| d.minutes()).max().unwrap_or(0);
    let count = window.len();
    let series: Vec<ProfileMinute> = (0..longest)
        .map(|minute| {
            let alive: Vec<&&Delivery> = window.iter().filter(|d| d.minutes() > minute).collect();
            let cpu: Vec<f64> = alive.iter().map(|d| d.cpu[minute]).collect();
            let mut memory: Vec<u64> = alive.iter().map(|d| d.memory[minute]).collect();
            memory.sort_unstable();
            let memory_f: Vec<f64> = memory.iter().map(|bytes| *bytes as f64).collect();
            ProfileMinute {
                minute: u32::try_from(minute).unwrap_or(u32::MAX),
                alive: alive.len() as f64 / count as f64,
                cpu_cores: mean(&cpu),
                cpu_stddev: stddev(&cpu),
                memory_bytes: percentile(&memory, MEMORY_PERCENTILE),
                memory_mean_bytes: mean(&memory_f),
                memory_stddev_bytes: stddev(&memory_f),
            }
        })
        .collect();
    let horizon = series
        .iter()
        .position(|minute| minute.alive < HORIZON_ALIVE)
        .unwrap_or(series.len());
    let horizon = u32::try_from(horizon)
        .unwrap_or(u32::MAX)
        .min(bounds.horizon_cap_minutes);
    let before_horizon = &series[..series.len().min(horizon as usize)];
    let peak_cpu_cores = before_horizon.iter().fold(
        ProfilePeak {
            minute: 0,
            value: 0.0,
        },
        |peak, minute| {
            if minute.cpu_cores > peak.value {
                ProfilePeak {
                    minute: minute.minute,
                    value: minute.cpu_cores,
                }
            } else {
                peak
            }
        },
    );
    let peak_memory_bytes = before_horizon.iter().fold(
        ProfilePeak {
            minute: 0,
            value: 0,
        },
        |peak, minute| {
            if minute.memory_bytes > peak.value {
                ProfilePeak {
                    minute: minute.minute,
                    value: minute.memory_bytes,
                }
            } else {
                peak
            }
        },
    );
    let expected_core_minutes = before_horizon
        .iter()
        .map(|minute| minute.cpu_cores * minute.alive)
        .sum();
    let cpu_spread = ratio(
        before_horizon.iter().map(|minute| minute.cpu_stddev).sum(),
        before_horizon.iter().map(|minute| minute.cpu_cores).sum(),
    );
    let memory_spread = ratio(
        before_horizon
            .iter()
            .map(|minute| minute.memory_stddev_bytes)
            .sum(),
        before_horizon
            .iter()
            .map(|minute| minute.memory_mean_bytes)
            .sum(),
    );
    let build_like = if base_class == BaseClass::NoDiff {
        window
            .iter()
            .filter(|d| !d.template().is_some_and(auto_task_builds_by_design))
            .filter_map(|d| build_like(d))
            .collect()
    } else {
        Vec::new()
    };
    let (delivery_peak_cpu, delivery_peak_memory_bytes) = delivery_peak_quantiles(window);
    ResourceProfile {
        sparse: count < bounds.min_deliveries,
        key,
        base_class,
        delivery_count: count,
        horizon_minutes: horizon,
        peak_cpu_cores,
        peak_memory_bytes,
        delivery_peak_cpu,
        delivery_peak_memory_bytes,
        expected_core_minutes,
        cpu_spread,
        memory_spread,
        build_like,
        deliveries: window
            .iter()
            .map(|d| ProfileDelivery {
                task_id: d.task_id.clone(),
                admitted_at: d.admitted_at,
                finished_at: d.finished(),
                minutes: u32::try_from(d.minutes()).unwrap_or(u32::MAX),
                run_ids: d.run_ids.clone(),
            })
            .collect(),
        series,
    }
}

/// A delivery's first stretch above [`BUILD_LIKE_CORES`] lasting
/// [`BUILD_LIKE_MINUTES`].
fn build_like(delivery: &Delivery) -> Option<BuildLikeDelivery> {
    let from = delivery
        .cpu
        .windows(BUILD_LIKE_MINUTES)
        .position(|stretch| stretch.iter().all(|cores| *cores > BUILD_LIKE_CORES))?;
    Some(BuildLikeDelivery {
        task_id: delivery.task_id.clone(),
        admitted_at: delivery.admitted_at,
        run_ids: delivery.run_ids.clone(),
        from_minute: u32::try_from(from).unwrap_or(u32::MAX),
        peak_cpu_cores: delivery.cpu.iter().copied().fold(0.0, f64::max),
    })
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// Population standard deviation.
fn stddev(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mean = mean(values);
    (values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / values.len() as f64)
        .sqrt()
}

/// Nearest-rank p50 and p90 of each delivery's highest single minute.
///
/// The peak is that delivery's own max, over its whole series, so two
/// deliveries that spike in different minutes keep both spikes. The series
/// peaks above are means and per-minute percentiles, which blur them.
fn delivery_peak_quantiles(window: &[&Delivery]) -> (ProfileQuantile<f64>, ProfileQuantile<u64>) {
    let mut cpu: Vec<f64> = window
        .iter()
        .map(|delivery| delivery.cpu.iter().copied().fold(0.0, f64::max))
        .collect();
    let mut memory: Vec<u64> = window
        .iter()
        .map(|delivery| delivery.memory.iter().copied().max().unwrap_or(0))
        .collect();
    cpu.sort_by(f64::total_cmp);
    memory.sort_unstable();
    (
        ProfileQuantile {
            p50: ranked(&cpu, (1, 2)),
            p90: ranked(&cpu, (9, 10)),
        },
        ProfileQuantile {
            p50: ranked(&memory, (1, 2)),
            p90: ranked(&memory, (9, 10)),
        },
    )
}

/// Nearest-rank percentile `numerator / denominator` of sorted `values`.
fn percentile(sorted: &[u64], fraction: (usize, usize)) -> u64 {
    ranked(sorted, fraction)
}

fn ranked<T: Copy + Default>(sorted: &[T], fraction: (usize, usize)) -> T {
    quantile_index(sorted.len(), fraction).map_or(T::default(), |index| sorted[index])
}

/// Index of the nearest-rank percentile `numerator/denominator` in a sorted slice.
fn quantile_index(len: usize, (numerator, denominator): (usize, usize)) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let rank = (len * numerator).div_ceil(denominator);
    Some(rank.clamp(1, len) - 1)
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}
