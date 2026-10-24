//! The live executor census pull and owner-local admission order against
//! [ORB-17304].
//!
//! Each executor of a workspace declares what it can run: a follower on each
//! admission request, the owner on each pass of its local drain. The owner
//! keeps the latest declaration per machine and orders candidates so that
//! work only the requesting executor can run leads. Capability stays a
//! pass/fail filter for admission; the census only orders.

use std::borrow::Borrow;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_types::task::{
    AdmissionAffinityReach, DispatchAffinity, ExecutorOsLimit, HostOs, NEUTRAL_PRESSURE_MILLI,
    Task, TaskOsRequirement,
};
use serde::{Deserialize, Serialize};

use super::{AdmissionCrewCapability, AdmissionRequest};

/// How long a census entry stays live after its executor last declared it:
/// ten minutes, well beyond a drain's idle poll. An executor holding a
/// claimed or running claim stays live past it, so one whose slots are full
/// and that stops pulling is still counted.
pub const EXECUTOR_CENSUS_LIVENESS_SECONDS: i64 = 600;

/// What one executor declares it can run: the capability predicates the
/// admission screen applies. The load-forecast fit (`admissible`) is left
/// out, since it changes every pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutorCapability {
    /// The executor's host OS; absent outside the `os:` namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<HostOs>,
    /// The crews it can run; absent places no crew restriction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crews: Option<AdmissionCrewCapability>,
    /// Whether it has a verified headless browser kit for `ui` work.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub browser_kit: bool,
}

impl ExecutorCapability {
    /// The capability a pull request declares.
    #[must_use]
    pub fn of_request(request: &AdmissionRequest) -> Self {
        Self {
            os: request.os,
            crews: request.crews.clone(),
            browser_kit: request.browser_kit,
        }
    }

    /// Whether this executor passes every capability predicate for `task`:
    /// its effective OS requirement, its browser kit and its crew.
    #[must_use]
    pub fn runs(&self, task: &Task, limits: &BTreeMap<String, ExecutorOsLimit>) -> bool {
        os_unavailable(self.os, task, limits).is_none()
            && browser_kit_unavailable(self.browser_kit, task).is_none()
            && self
                .crews
                .as_ref()
                .is_none_or(|crews| crews.unrunnable_reason(task.crew.as_deref()).is_none())
    }
}

/// Why `task`'s effective OS requirement excludes an executor on `os`, or
/// `None` when it admits it.
///
/// The requirement is the task's `os:` tags intersected with its assigned
/// crew's executor `host_os` (`limits`). An empty `limits` map is the tag
/// requirement alone.
#[must_use]
pub fn os_unavailable(
    os: Option<HostOs>,
    task: &Task,
    limits: &BTreeMap<String, ExecutorOsLimit>,
) -> Option<String> {
    let runs = os.map_or("an undeclared OS", HostOs::as_str);
    let wait = match TaskOsRequirement::for_crew(&task.tags, task.crew.as_deref(), limits) {
        Ok(requirement) => requirement.unsatisfied_reason(os)?,
        Err(reason) => reason,
    };
    Some(format!("{wait}; the executor runs {runs}"))
}

/// Why a `ui` task excludes an executor, or `None` when it admits it: the
/// executor declares no verified browser kit.
#[must_use]
pub fn browser_kit_unavailable(browser_kit: bool, task: &Task) -> Option<String> {
    (!browser_kit && orbit_types::task::requires_browser_kit(&task.tags)).then(|| {
        format!(
            "{}; the executor reports no verified kit",
            orbit_types::task::browser_kit_wait()
        )
    })
}

/// One executor's latest declaration, as the owner persists it per
/// workspace and machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorCensusEntry {
    pub machine_id: String,
    pub capability: ExecutorCapability,
    /// The drain run that declared it.
    pub run_id: String,
    pub seen_at: DateTime<Utc>,
}

impl ExecutorCensusEntry {
    /// Whether the entry was declared within the liveness window before
    /// `now`.
    #[must_use]
    pub fn fresh(&self, now: DateTime<Utc>) -> bool {
        now.signed_duration_since(self.seen_at).num_seconds() < EXECUTOR_CENSUS_LIVENESS_SECONDS
    }
}

/// The live executors one admission orders its candidates against, the
/// requesting executor among them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorCensus {
    requester: String,
    executors: BTreeMap<String, ExecutorCapability>,
}

/// How a census classes the admitted task, as the receipt records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionAffinityClass {
    /// Only the requesting executor can run it.
    Exclusive,
    /// Some, but not every, live executor can run it.
    Shared,
    /// Every live executor can run it.
    Fungible,
}

/// The ordering facts behind an admitted task: its affinity class, how many
/// live executors could run it, and the census it was ordered against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionAffinity {
    pub class: AdmissionAffinityClass,
    pub eligible_executors: usize,
    /// The `workflow.admission_affinity` reach the order used.
    pub reach: AdmissionAffinityReach,
    /// Machine ids of the live executors in the census, the requester
    /// among them.
    pub census: Vec<String>,
}

impl ExecutorCensus {
    /// The requester with `capability`, and every other live executor in
    /// `others`. An entry of the requester's own machine among `others` is
    /// replaced by `capability`.
    #[must_use]
    pub fn new(
        requester: &str,
        capability: ExecutorCapability,
        others: impl IntoIterator<Item = ExecutorCensusEntry>,
    ) -> Self {
        let mut executors = others
            .into_iter()
            .map(|entry| (entry.machine_id, entry.capability))
            .collect::<BTreeMap<_, _>>();
        executors.insert(requester.to_string(), capability);
        Self {
            requester: requester.to_string(),
            executors,
        }
    }

    /// Machine ids of the live executors, in order.
    #[must_use]
    pub fn machine_ids(&self) -> Vec<String> {
        self.executors.keys().cloned().collect()
    }

    /// `(eligible executors, whether the requester is among them)`.
    fn eligible(&self, task: &Task, limits: &BTreeMap<String, ExecutorOsLimit>) -> (usize, bool) {
        let mut eligible = 0;
        let mut requester = false;
        for (machine, capability) in &self.executors {
            if capability.runs(task, limits) {
                eligible += 1;
                requester |= *machine == self.requester;
            }
        }
        (eligible, requester)
    }

    /// `task`'s standing for the requester. With the requester alone in the
    /// census every task shares the default standing, which never reorders.
    #[must_use]
    pub fn affinity(
        &self,
        task: &Task,
        limits: &BTreeMap<String, ExecutorOsLimit>,
    ) -> DispatchAffinity {
        if self.executors.len() < 2 {
            return DispatchAffinity::default();
        }
        match self.eligible(task, limits) {
            (eligible, true) => DispatchAffinity {
                exclusive: eligible == 1,
                eligible,
                pressure_milli: 0,
            },
            (_, false) => DispatchAffinity {
                exclusive: false,
                eligible: usize::MAX,
                pressure_milli: 0,
            },
        }
    }

    /// The highest weekly usage pressure the requester reported for a
    /// member of `task`'s crew pool [ORB-17773], in milli-units. The pool
    /// is the one the task's crew source names — `pool:<tier>` or
    /// `purpose:<purpose>` — else its complexity tier's. A requester that
    /// reported no pressure, a pool the owner did not supply, and a member
    /// it did not name all read neutral, so pressure orders nothing.
    #[must_use]
    pub fn requester_pressure_milli(
        &self,
        task: &Task,
        pools: &BTreeMap<String, Vec<String>>,
    ) -> u64 {
        let Some(crews) = self
            .executors
            .get(&self.requester)
            .and_then(|capability| capability.crews.as_ref())
            .filter(|crews| crews.has_pressure())
        else {
            return NEUTRAL_PRESSURE_MILLI;
        };
        let key = match task.crew_source.as_deref() {
            Some(source) if source.starts_with("pool:") => {
                source.strip_prefix("pool:").map(ToOwned::to_owned)
            }
            Some(source) if source.starts_with("purpose:") => Some(source.to_string()),
            _ => task
                .complexity
                .map(|complexity| complexity.as_str().to_string()),
        };
        let Some(members) = key.and_then(|key| pools.get(&key)) else {
            return NEUTRAL_PRESSURE_MILLI;
        };
        members
            .iter()
            .map(|crew| crews.pressure_milli_for(crew))
            .max()
            .unwrap_or(NEUTRAL_PRESSURE_MILLI)
    }

    /// What the receipt records for an admitted `task`.
    #[must_use]
    pub fn record(
        &self,
        task: &Task,
        limits: &BTreeMap<String, ExecutorOsLimit>,
        reach: AdmissionAffinityReach,
    ) -> AdmissionAffinity {
        let (eligible, requester) = self.eligible(task, limits);
        let class = if eligible == self.executors.len() {
            AdmissionAffinityClass::Fungible
        } else if eligible == 1 && requester {
            AdmissionAffinityClass::Exclusive
        } else {
            AdmissionAffinityClass::Shared
        };
        AdmissionAffinity {
            class,
            eligible_executors: eligible,
            reach,
            census: self.machine_ids(),
        }
    }
}

/// Sort owned or borrowed tasks into automatic dispatch order for `census`'s
/// requester [ORB-17304]: the one order pull admission and owner-local
/// dispatch share, so the two cannot disagree for the same census.
///
/// Without a census, or with `reach` off, this is the canonical band,
/// priority, expiry, age and ID order. The requester's reported usage
/// pressure [ORB-17773] rides its census capability and scores each task
/// by `pools` (see [`ExecutorCensus::requester_pressure_milli`]); it is
/// the lowest key above age, under every reach, and a requester that
/// reported none — owner-local dispatch included — orders as before.
pub fn census_dispatch_order<T: Borrow<Task>>(
    tasks: &mut [T],
    expiring: impl Fn(&str) -> bool,
    census: Option<&ExecutorCensus>,
    reach: AdmissionAffinityReach,
    limits: &BTreeMap<String, ExecutorOsLimit>,
    pools: &BTreeMap<String, Vec<String>>,
) {
    let affinities = match census {
        Some(census) => tasks
            .iter()
            .map(|task| {
                let task = task.borrow();
                let mut affinity = if reach != AdmissionAffinityReach::Off {
                    census.affinity(task, limits)
                } else {
                    DispatchAffinity::default()
                };
                affinity.pressure_milli = census.requester_pressure_milli(task, pools);
                (task.id.clone(), affinity)
            })
            .collect::<BTreeMap<_, _>>(),
        _ => BTreeMap::new(),
    };
    let affinity = |task: &Task| affinities.get(&task.id).copied().unwrap_or_default();
    tasks.sort_by(|left, right| {
        let (left, right) = (left.borrow(), right.borrow());
        orbit_types::task::automatic_dispatch_cmp_with_affinity(
            (left, expiring(&left.id), affinity(left)),
            (right, expiring(&right.id), affinity(right)),
            reach,
        )
    });
}
