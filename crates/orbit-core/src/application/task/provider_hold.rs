//! Admission under a provider failure hold [ORB-14266].
//!
//! Run finalization holds a task whose run its provider failed
//! (`runtime::task::provider_hold`). While the hold stands, the task's crew is
//! drawn from the crews it does not exclude: its own crew or pool first, then
//! its complexity pool, then the workspace default. When the hold excludes
//! every one, the local drain's backlog snapshot defers the task until the
//! hold's `not_before`.
//!
//! [ORB-14697] A usage-limit hold on a task whose crew is its explicit choice
//! follows `workflow.provider_limit_explicit_crews`: under `wait` it does not
//! redirect the task, which waits out the hold instead.
//!
//! [ORB-16323] An unavailable hold names the machine whose login failed and
//! applies only on that machine. While it stands, later draws on that machine
//! also skip the failed crew: another complexity-pool member is drawn, and a
//! task whose pool has none left waits instead of falling through to the
//! workspace default. The task that owns the hold keeps the default fallback.
//! Capacity, limit and refusal holds, and an unavailable hold stored with no
//! machine, stay on the per-task path above.

use std::cell::RefCell;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_config::ProviderLimitExplicitCrews;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{ProviderFailureClass, ProviderFailureHold};

use crate::OrbitRuntime;
use crate::application::job::crew_pools::{CapturedCrewPools, CrewCandidate};

/// One other task's standing unavailable hold on this host.
#[derive(Clone)]
struct HostUnavailable {
    task_id: String,
    hold: ProviderFailureHold,
}

thread_local! {
    static HOST_UNAVAILABLE: RefCell<Option<Vec<HostUnavailable>>> = const { RefCell::new(None) };
}

/// Restores the previous host-hold cache, including after a panic.
struct HostUnavailableScope {
    previous: Option<Vec<HostUnavailable>>,
}

impl Drop for HostUnavailableScope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        HOST_UNAVAILABLE.with(|slot| {
            *slot.borrow_mut() = previous;
        });
    }
}

impl OrbitRuntime {
    /// Cache this host's standing unavailable holds for the caller's scope.
    ///
    /// A backlog snapshot asks admission about every task, and each of those
    /// asks which crews this host must skip. Reading the backlog once per
    /// snapshot keeps that from walking every task's history for every task.
    pub(crate) fn enter_host_unavailable_scope(&self) -> impl Drop {
        let loaded = self.load_host_unavailable();
        let previous = HOST_UNAVAILABLE.with(|slot| slot.borrow_mut().replace(loaded));
        HostUnavailableScope { previous }
    }

    /// The crews `task` may be admitted to under a standing hold, or `None`
    /// when the hold excludes every crew it could run as. A hold that excludes
    /// no crew at all leaves nothing to narrow, so it is `None` as well.
    /// `candidates` and `source` are the draw without the hold; the fallbacks
    /// are the task's complexity pool, then the workspace default.
    pub(crate) fn provider_held_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        hold: &ProviderFailureHold,
        candidates: &[CrewCandidate],
        source: &str,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        if hold.excluded_crews.is_empty() {
            return Ok(None);
        }
        let permitted = |candidates: &[CrewCandidate]| -> Vec<CrewCandidate> {
            candidates
                .iter()
                .filter(|candidate| !hold.excludes(&candidate.crew.name))
                .cloned()
                .collect()
        };
        let drawable = |candidates: &[CrewCandidate]| candidates.iter().any(|c| c.weight > 0);
        let held = |source: &str| {
            format!(
                "{source}; provider hold excludes {} until {}",
                hold.excluded_crews.join(", "),
                hold.not_before.to_rfc3339()
            )
        };
        let own = permitted(candidates);
        if drawable(&own) {
            return Ok(Some((own, held(source))));
        }
        if hold.class == ProviderFailureClass::Limit
            && self.context.settings().provider_limit().explicit_crews
                == ProviderLimitExplicitCrews::Wait
            && self.explicit_task_crew(task)?
        {
            return Ok(None);
        }
        if let Some((pool, pool_source)) =
            self.complexity_pool_candidates(task.complexity, pools)?
        {
            let pool = permitted(&pool);
            if drawable(&pool) {
                return Ok(Some((pool, held(&pool_source))));
            }
        }
        if let Ok(default) = self.resolve_crew_for_task(None, None)
            && !hold.excludes(&default.name)
        {
            return Ok(Some((
                vec![CrewCandidate {
                    crew: default,
                    weight: 1,
                }],
                held("default"),
            )));
        }
        Ok(None)
    }

    /// Why `task` waits out a provider failure hold at this admission:
    /// `Some` when every crew it could run as is excluded, `None` when no
    /// hold applies or another crew may take it.
    ///
    /// The task's own hold is consulted first, including its fallback to the
    /// workspace default. Other tasks' unavailable holds on this machine are
    /// then applied to that draw: a remaining complexity-pool crew is taken,
    /// and a pool with none left waits. Those host holds do not fall through
    /// to the default, so a dead login is not handed the workspace default
    /// crew when that crew is the one that just failed.
    pub(crate) fn provider_backoff_deferral(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
    ) -> Result<Option<String>, OrbitError> {
        let machine = self.automation_machine_identity();
        let own = self
            .admission_provider_hold(task)
            .filter(|hold| hold.applies_on(machine));
        // A hold that excludes no crew has no draw to narrow, so it defers
        // whatever the task would otherwise draw, even when that draw fails.
        if let Some(hold) = &own
            && hold.excluded_crews.is_empty()
        {
            return Ok(Some(own_deferral(hold)));
        }
        let host = self.host_unavailable_except(&task.id);
        if own.is_none() && host.is_empty() {
            return Ok(None);
        }
        let (candidates, source) = self.unheld_task_crew_candidates(task, pools)?;
        let (candidates, source) = if let Some(hold) = &own {
            match self.provider_held_candidates(task, pools, hold, &candidates, &source)? {
                Some(draw) => draw,
                None => return Ok(Some(own_deferral(hold))),
            }
        } else {
            (candidates, source)
        };
        if host.is_empty() {
            return Ok(None);
        }
        match self.without_host_unavailable(task, pools, &host, &candidates, &source)? {
            Some(_) => Ok(None),
            None => Ok(Some(host_deferral(&host))),
        }
    }

    /// The crews `task` is drawn from before this host's provider limits:
    /// its unheld draw, narrowed by a standing provider hold when the hold
    /// leaves any crew, then by other tasks' unavailable holds on this host.
    ///
    /// When either exclusion leaves no crew, the draw stays as it was before
    /// that exclusion, so an operator's explicit ship still runs.
    /// [`Self::provider_backoff_deferral`] reports the wait to the drain.
    pub(crate) fn held_crew_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
    ) -> Result<(Vec<CrewCandidate>, String, Option<ProviderFailureHold>), OrbitError> {
        let machine = self.automation_machine_identity();
        let (candidates, source) = self.unheld_task_crew_candidates(task, pools)?;
        let own = self
            .admission_provider_hold(task)
            .filter(|hold| hold.applies_on(machine));
        let (candidates, source) = if let Some(hold) = &own {
            self.provider_held_candidates(task, pools, hold, &candidates, &source)?
                .unwrap_or((candidates, source))
        } else {
            (candidates, source)
        };
        let host = self.host_unavailable_except(&task.id);
        if let Some((narrowed, narrowed_source)) =
            self.without_host_unavailable(task, pools, &host, &candidates, &source)?
        {
            return Ok((narrowed, narrowed_source, own));
        }
        Ok((candidates, source, own))
    }

    /// Standing unavailable holds on this machine, other than `task_id`.
    ///
    /// A host with no automation identity has no host scope: holds it places
    /// carry no machine and keep today's per-task behavior. An unreadable
    /// backlog is no host exclusion, matching an unreadable hold.
    fn host_unavailable_except(&self, task_id: &str) -> Vec<ProviderFailureHold> {
        let cached = HOST_UNAVAILABLE.with(|slot| slot.borrow().clone());
        let holds = cached.unwrap_or_else(|| self.load_host_unavailable());
        holds
            .into_iter()
            .filter(|row| row.task_id != task_id)
            .map(|row| row.hold)
            .collect()
    }

    fn load_host_unavailable(&self) -> Vec<HostUnavailable> {
        let Some(machine) = self.automation_machine_identity() else {
            return Vec::new();
        };
        let now = Utc::now();
        let tasks =
            match self.list_tasks_filtered(Some(TaskStatus::Backlog), None, None, None, None, None)
            {
                Ok(tasks) => tasks,
                Err(error) => {
                    tracing::warn!(
                        "could not list backlog for host provider-unavailable holds: {error}"
                    );
                    return Vec::new();
                }
            };
        let mut holds = Vec::new();
        for task in tasks {
            let Some(hold) = self.admission_provider_hold(&task) else {
                continue;
            };
            if hold.class == ProviderFailureClass::Unavailable
                && hold.machine.as_deref() == Some(machine)
                && hold.stands_at(now)
            {
                holds.push(HostUnavailable {
                    task_id: task.id,
                    hold,
                });
            }
        }
        holds
    }

    /// `candidates` with this host's failed crews removed, or the task's
    /// complexity pool when that is what still has a member. `None` when the
    /// pool has no remaining member. Never falls through to the workspace
    /// default: that default is often the crew whose login just failed.
    fn without_host_unavailable(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        host: &[ProviderFailureHold],
        candidates: &[CrewCandidate],
        source: &str,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        if host.is_empty() {
            return Ok(Some((candidates.to_vec(), source.to_string())));
        }
        let blocked = |crew: &str| host.iter().any(|hold| hold.excludes(crew));
        let permitted = |candidates: &[CrewCandidate]| {
            candidates
                .iter()
                .filter(|candidate| !blocked(&candidate.crew.name))
                .cloned()
                .collect::<Vec<_>>()
        };
        let drawable = |candidates: &[CrewCandidate]| candidates.iter().any(|c| c.weight > 0);
        let (_, crews, not_before) = host_hold_facts(host);
        let annotate = |drawn_from: &str| {
            format!(
                "{drawn_from}; provider unavailable on this host excludes {crews} until {not_before}"
            )
        };
        let own = permitted(candidates);
        if drawable(&own) {
            let source = if own.len() == candidates.len() {
                source.to_string()
            } else {
                annotate(source)
            };
            return Ok(Some((own, source)));
        }
        // A draw that names none of the failed crews stays as it was, even
        // when it has no positive-weight member. Only a draw this host
        // actually narrowed looks for another pool member.
        if own.len() == candidates.len() {
            return Ok(Some((candidates.to_vec(), source.to_string())));
        }
        if let Some((pool, pool_source)) =
            self.complexity_pool_candidates(task.complexity, pools)?
        {
            let pool = permitted(&pool);
            if drawable(&pool) {
                return Ok(Some((pool, annotate(&pool_source))));
            }
        }
        Ok(None)
    }
}

/// Why the task's own hold defers it. The wording is what readiness reports.
fn own_deferral(hold: &ProviderFailureHold) -> String {
    let excluded = if hold.excluded_crews.is_empty() {
        "the run resolved no crew to exclude".to_string()
    } else {
        format!(
            "every crew the task may run as ({}) is excluded",
            hold.excluded_crews.join(", ")
        )
    };
    format!(
        "run {} ended with {}; {excluded} until {}",
        hold.run_id,
        hold.class.as_str(),
        hold.not_before.to_rfc3339()
    )
}

/// Why this host will not draw the crews `holds` exclude.
fn host_deferral(holds: &[ProviderFailureHold]) -> String {
    let (runs, crews, not_before) = host_hold_facts(holds);
    format!(
        "run {runs} ended with provider_unavailable on this host; crews {crews} are excluded until {not_before}"
    )
}

/// Sorted run ids, sorted crews, and the latest `not_before` across `holds`.
fn host_hold_facts(holds: &[ProviderFailureHold]) -> (String, String, String) {
    let mut runs = holds
        .iter()
        .map(|hold| hold.run_id.as_str())
        .collect::<Vec<_>>();
    runs.sort_unstable();
    runs.dedup();
    let mut crews = holds
        .iter()
        .flat_map(|hold| hold.excluded_crews.iter().map(String::as_str))
        .collect::<Vec<_>>();
    crews.sort_unstable();
    crews.dedup();
    let not_before = holds
        .iter()
        .map(|hold| hold.not_before)
        .max()
        .map(|at| at.to_rfc3339())
        .unwrap_or_default();
    (runs.join(", "), crews.join(", "), not_before)
}
