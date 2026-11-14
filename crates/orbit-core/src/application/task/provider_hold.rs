//! Admission under a provider failure hold [ORB-14266].
//!
//! Run finalization holds a task whose run its provider failed
//! (`runtime::task::provider_hold`). While the hold stands, the task's crew is
//! drawn from the crews it does not exclude: its own crew or pool first, then
//! the other enabled members of its purpose pool, then its complexity pool,
//! then the workspace default. When the hold excludes every one, the local
//! drain's backlog snapshot defers the task until the hold's `not_before`.
//!
//! [ORB-14697] A usage-limit hold on a task whose crew is its explicit choice
//! follows `workflow.provider_limit_explicit_crews`: under `wait` it does not
//! redirect the task, which waits out the hold instead.
//!
//! [ORB-16379] Local admission reads the task's hold as this machine sees
//! it, so an unavailable hold placed on another host excludes here only the
//! crews it carries from an earlier standing hold. It also widens the hold by
//! this host's live crew outages, so one failed login keeps every task off
//! that crew here until the outage lapses. Pull admission
//! reads the hold as the executor's machine sees it and leaves the owner's
//! outages out: they are not the executor's.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_config::ProviderLimitExplicitCrews;
use orbit_types::task::{CrewSource, Task, TaskHistoryEntry};
use orbit_types::workflow::{ProviderCrewOutage, ProviderFailureClass, ProviderFailureHold};

use crate::OrbitRuntime;
use crate::application::contracts::CrewCandidate;
use crate::application::task::{CrewPools, TaskJobPort};
use crate::runtime::task::provider_hold::{same_provider, standing_provider_hold_from_history};

/// One admission snapshot's provider-outage read and the widened holds it
/// produced. Nested snapshots share the outer read.
struct ProviderPassState {
    outages: Vec<ProviderCrewOutage>,
    holds: BTreeMap<String, Option<ProviderFailureHold>>,
}

thread_local! {
    static PROVIDER_PASS_DEPTH: Cell<u32> = const { Cell::new(0) };
    static PROVIDER_PASS: RefCell<Option<ProviderPassState>> = const { RefCell::new(None) };
}

/// Keeps one `provider_crew_outages` query for every hold check in a backlog
/// snapshot. Drop it when the snapshot returns; later admission work must
/// not reuse a hold cached for that population.
#[must_use = "dropping the guard ends the admission provider pass"]
pub(crate) struct AdmissionProviderPass;

impl AdmissionProviderPass {
    /// Read this host's crew outages once. A nested snapshot reuses the
    /// outer read and does not clear it on the way out.
    pub(crate) fn enter(runtime: &OrbitRuntime) -> Self {
        let depth = PROVIDER_PASS_DEPTH.with(|depth| {
            let next = depth.get().saturating_add(1);
            depth.set(next);
            next
        });
        if depth == 1 {
            let outages = runtime
                .provider_crew_outages(Utc::now())
                .unwrap_or_else(|error| {
                    tracing::warn!("could not read this host's provider outages: {error}");
                    Vec::new()
                });
            PROVIDER_PASS.with(|state| {
                *state.borrow_mut() = Some(ProviderPassState {
                    outages,
                    holds: BTreeMap::new(),
                });
            });
        }
        Self
    }
}

impl Drop for AdmissionProviderPass {
    fn drop(&mut self) {
        PROVIDER_PASS_DEPTH.with(|depth| {
            let next = depth.get().saturating_sub(1);
            depth.set(next);
            if next == 0 {
                PROVIDER_PASS.with(|state| *state.borrow_mut() = None);
            }
        });
    }
}

fn cached_admission_hold(id: &str) -> Option<Option<ProviderFailureHold>> {
    if PROVIDER_PASS_DEPTH.with(|depth| depth.get()) == 0 {
        return None;
    }
    PROVIDER_PASS.with(|state| {
        state
            .borrow()
            .as_ref()
            .and_then(|pass| pass.holds.get(id).cloned())
    })
}

fn pass_outages() -> Option<Vec<ProviderCrewOutage>> {
    if PROVIDER_PASS_DEPTH.with(|depth| depth.get()) == 0 {
        return None;
    }
    PROVIDER_PASS.with(|state| state.borrow().as_ref().map(|pass| pass.outages.clone()))
}

/// Widen `hold` by `outages`. A hold that already excludes no crew is
/// returned unchanged: it defers the task, so outages must not narrow it.
fn widen_with_outages(
    runtime: &OrbitRuntime,
    hold: Option<ProviderFailureHold>,
    outages: &[ProviderCrewOutage],
) -> Option<ProviderFailureHold> {
    if hold
        .as_ref()
        .is_some_and(|hold| hold.excluded_crews.is_empty())
    {
        return hold;
    }
    // A crew of the same name configured on another provider is not the
    // one whose login failed.
    let registry = runtime.context.settings().crews();
    let mut matching = outages.iter().filter(|outage| {
        outage.provider.is_none()
            || orbit_types::identity::find_crew(&outage.crew, registry).is_none_or(|crew| {
                same_provider(Some(&crew.assignment.provider), outage.provider.as_deref())
            })
    });
    let Some(latest) = matching.next() else {
        return hold;
    };
    let mut hold = hold.unwrap_or_else(|| ProviderFailureHold {
        class: ProviderFailureClass::Unavailable,
        provider: latest.provider.clone(),
        account: None,
        excluded_crews: Vec::new(),
        machine: None,
        carried_crews: Vec::new(),
        not_before: latest.until,
        run_id: latest.run_id.clone(),
    });
    for outage in std::iter::once(latest).chain(matching) {
        hold.excluded_crews.push(outage.crew.clone());
        hold.not_before = hold.not_before.max(outage.until);
    }
    hold.excluded_crews.sort();
    hold.excluded_crews.dedup();
    Some(hold)
}

impl OrbitRuntime {
    /// The hold this host's local admission draws `task` under: its standing
    /// hold as this machine sees it, widened by this host's crew outages. A
    /// hold that excludes no crew already defers the task, so it is not
    /// widened. Outages alone make an unavailable hold naming the latest
    /// outage's run. An unreadable outage store is logged and adds nothing.
    pub(crate) fn admission_provider_hold(&self, task: &Task) -> Option<ProviderFailureHold> {
        if let Some(hold) = cached_admission_hold(&task.id) {
            return hold;
        }
        let hold = self.provider_hold_on(task, self.automation_machine_identity());
        if let Some(outages) = pass_outages() {
            return widen_with_outages(self, hold, &outages);
        }
        if hold
            .as_ref()
            .is_some_and(|hold| hold.excluded_crews.is_empty())
        {
            return hold;
        }
        let outages = self
            .provider_crew_outages(Utc::now())
            .unwrap_or_else(|error| {
                tracing::warn!("could not read this host's provider outages: {error}");
                Vec::new()
            });
        widen_with_outages(self, hold, &outages)
    }

    /// Cache the widened hold for `id` from history the snapshot already
    /// read. A hold that excludes no crew is not widened. Outside a pass
    /// this records nothing.
    pub(crate) fn remember_admission_provider_holds<'a>(
        &self,
        rows: impl Iterator<Item = (&'a str, &'a Task, &'a [TaskHistoryEntry])>,
    ) {
        let Some(outages) = pass_outages() else {
            return;
        };
        let now = Utc::now();
        let machine = self.automation_machine_identity();
        let mut remembered = BTreeMap::new();
        for (id, task, history) in rows {
            let standing = standing_provider_hold_from_history(task, history, now)
                .and_then(|hold| hold.on_machine(machine));
            remembered.insert(id.to_string(), widen_with_outages(self, standing, &outages));
        }
        PROVIDER_PASS.with(|state| {
            if let Some(pass) = state.borrow_mut().as_mut() {
                pass.holds.extend(remembered);
            }
        });
    }

    /// The crews `task` may be admitted to under a standing hold, or `None`
    /// when the hold excludes every crew it could run as. A hold that excludes
    /// no crew at all leaves nothing to narrow, so it is `None` as well.
    /// `candidates` and `source` are the draw without the hold; the fallbacks
    /// are the task's purpose pool, then its complexity pool, then the
    /// workspace default.
    pub(in crate::application) fn provider_held_candidates(
        &self,
        task: &Task,
        pools: &CrewPools,
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
            && self.provider_limit_policy().explicit_crews == ProviderLimitExplicitCrews::Wait
            && self.explicit_task_crew(task)?
        {
            return Ok(None);
        }
        // A collapsed `purpose:` or `type:` assignment names only the chosen
        // member. Retry the pool's other enabled members before the
        // complexity pool.
        if matches!(
            CrewSource::parse(source),
            CrewSource::Purpose(_) | CrewSource::Type { .. }
        ) && let Some((pool, pool_source)) = self.assigned_pool_candidates(task)?
        {
            let pool = permitted(&pool);
            if drawable(&pool) {
                return Ok(Some((pool, held(&pool_source))));
            }
        }
        if let Some((pool, pool_source)) = self.complexity_pool_members(task.complexity, pools)? {
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
                    auto_effort: None,
                }],
                held("default"),
            )));
        }
        Ok(None)
    }

    /// Why `task` waits out a provider failure hold at this admission:
    /// `Some` when the hold excludes every crew it could run as, `None` when
    /// no hold stands or another crew may take it.
    pub(in crate::application) fn provider_backoff_deferral(
        &self,
        task: &Task,
        pools: &CrewPools,
    ) -> Result<Option<String>, OrbitError> {
        let Some(hold) = self.admission_provider_hold(task) else {
            return Ok(None);
        };
        // A hold that excludes no crew has no draw to narrow, so it defers
        // whatever the task would otherwise draw, even when that draw fails.
        if !hold.excluded_crews.is_empty() {
            let (candidates, source) = self.crew_candidates_before_holds(task, pools)?;
            if self
                .provider_held_candidates(task, pools, &hold, &candidates, &source)?
                .is_some()
            {
                return Ok(None);
            }
        }
        let excluded = if hold.excluded_crews.is_empty() {
            "the run resolved no crew to exclude".to_string()
        } else {
            format!(
                "every crew the task may run as ({}) is excluded",
                hold.excluded_crews.join(", ")
            )
        };
        Ok(Some(format!(
            "run {} ended with {}; {excluded} until {}",
            hold.run_id,
            hold.class.as_str(),
            hold.not_before.to_rfc3339()
        )))
    }

    /// The crews `task` is drawn from before this host's provider limits:
    /// its unheld draw, narrowed by a standing provider hold when the hold
    /// leaves any crew, and that hold.
    pub(in crate::application) fn held_crew_candidates(
        &self,
        task: &Task,
        pools: &CrewPools,
    ) -> Result<(Vec<CrewCandidate>, String, Option<ProviderFailureHold>), OrbitError> {
        self.held_crew_candidates_for(task, pools, false)
    }

    /// Candidate routing for an owner-issued claimed leaf. Its snapshot is
    /// authoritative for assignment provenance, while follower-local holds
    /// still narrow the normal current pool.
    pub(in crate::application) fn held_claimed_leaf_crew_candidates(
        &self,
        task: &Task,
        pools: &CrewPools,
    ) -> Result<(Vec<CrewCandidate>, String, Option<ProviderFailureHold>), OrbitError> {
        self.held_crew_candidates_for(task, pools, true)
    }

    fn held_crew_candidates_for(
        &self,
        task: &Task,
        pools: &CrewPools,
        claimed_leaf: bool,
    ) -> Result<(Vec<CrewCandidate>, String, Option<ProviderFailureHold>), OrbitError> {
        let (candidates, source) = if claimed_leaf {
            self.claimed_leaf_candidates_before_holds(task, pools)?
        } else {
            self.crew_candidates_before_holds(task, pools)?
        };
        let Some(hold) = self.admission_provider_hold(task) else {
            return Ok((candidates, source, None));
        };
        let (candidates, source) = self
            .provider_held_candidates(task, pools, &hold, &candidates, &source)?
            .unwrap_or((candidates, source));
        Ok((candidates, source, Some(hold)))
    }
}
