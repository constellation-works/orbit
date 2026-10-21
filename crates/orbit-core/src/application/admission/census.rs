//! The executor census pull and owner-local admission order against
//! [ORB-17304].
//!
//! Each follower's census row is upserted by the store with its admission
//! receipt. The owner declares its own on each pass of its local drain. Both
//! orders read the live rows and sort with one comparator, so for the same
//! census they agree on which work only the requester can run. The census
//! only orders: a census that cannot be read gives the canonical order.

use std::borrow::Borrow;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_store::contracts::{
    ExecutorCapability, ExecutorCensus, ExecutorCensusEntry, census_dispatch_order,
};
use orbit_types::task::{AdmissionAffinityReach, ExecutorOsLimit, Task};

use crate::OrbitRuntime;

/// How one requester orders its candidates: the live census, the
/// `workflow.admission_affinity` reach, and the crew `host_os` limits the
/// eligibility predicates read.
#[derive(Debug, Clone, Default)]
pub(crate) struct DispatchOrder {
    census: Option<ExecutorCensus>,
    reach: AdmissionAffinityReach,
    limits: BTreeMap<String, ExecutorOsLimit>,
}

impl DispatchOrder {
    /// Sort `tasks` into this requester's dispatch order. `expiring` names
    /// the backlog tasks whose frozen batch nears its deadline.
    pub(crate) fn sort<T: Borrow<Task>>(
        &self,
        tasks: &mut [T],
        expiring: &BTreeMap<String, DateTime<Utc>>,
    ) {
        census_dispatch_order(
            tasks,
            |id| expiring.contains_key(id),
            self.census.as_ref(),
            self.reach,
            &self.limits,
        );
    }
}

impl OrbitRuntime {
    /// The live census `requester` orders against, with its own entry
    /// replaced by `capability`, or `None` when ordering is off or the
    /// census cannot be read.
    pub(crate) fn executor_census(
        &self,
        requester: &str,
        capability: ExecutorCapability,
    ) -> Option<ExecutorCensus> {
        if self.admission_affinity() == AdmissionAffinityReach::Off {
            return None;
        }
        let live = self.live_census()?;
        Some(ExecutorCensus::new(requester, capability, live))
    }

    /// The owner-local dispatch order: the owner is the requester, declared
    /// by its own live census row. Without one, as when no local drain is
    /// running, the order is canonical.
    pub(crate) fn owner_dispatch_order(
        &self,
        limits: &BTreeMap<String, ExecutorOsLimit>,
    ) -> DispatchOrder {
        let reach = self.admission_affinity();
        let census = (reach != AdmissionAffinityReach::Off)
            .then(|| self.automation_machine_identity())
            .flatten()
            .and_then(|owner| {
                let live = self.live_census()?;
                let own = live
                    .iter()
                    .find(|entry| entry.machine_id == owner)?
                    .capability
                    .clone();
                Some(ExecutorCensus::new(owner, own, live))
            });
        DispatchOrder {
            census,
            reach,
            limits: limits.clone(),
        }
    }

    /// The live census rows. The owner's own row counts only while the
    /// local drain that declared it is still running, since the owner holds
    /// no claim that would keep it live; a follower's drain is remote, so
    /// its row ages out by `seen_at` and its claims alone. `None`, with a
    /// warning, when the census cannot be read.
    fn live_census(&self) -> Option<Vec<ExecutorCensusEntry>> {
        let mut live = match self.stores().tasks().live_executor_census(Utc::now()) {
            Ok(live) => live,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.admission",
                    %error,
                    "could not read the executor census; admission keeps its canonical order"
                );
                return None;
            }
        };
        if let Some(owner) = self.automation_machine_identity() {
            live.retain(|entry| entry.machine_id != owner || self.drain_running(&entry.run_id));
        }
        Some(live)
    }

    fn drain_running(&self, run_id: &str) -> bool {
        matches!(
            self.stores().jobs().get_job_run(run_id),
            Ok(Some(run)) if !run.state.is_terminal()
        )
    }

    /// Upsert the owner's own census row for its local drain `run_id`: this
    /// host's OS and verified browser kit, and the crews the drain's window
    /// can run (its preflight less provider-limit exclusions). Best effort:
    /// a row that cannot be written leaves the last one, which ages out.
    pub(crate) fn record_owner_census(&self, run_id: &str) {
        if self.admission_affinity() == AdmissionAffinityReach::Off {
            return;
        }
        let Some(owner) = self.automation_machine_identity() else {
            return;
        };
        let crews = match crate::application::pull_drain::crew_window(self, run_id) {
            Ok(window) => Some(window.capability()),
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.admission",
                    run_id,
                    %error,
                    "could not read the local drain's crew window; its census row declares no crew restriction"
                );
                None
            }
        };
        let entry = ExecutorCensusEntry {
            machine_id: owner.to_string(),
            capability: ExecutorCapability {
                os: self.host_os(),
                crews,
                browser_kit: self.browser_kit_verified(),
            },
            run_id: run_id.to_string(),
            seen_at: Utc::now(),
        };
        if let Err(error) = self.stores().tasks().record_executor_census(&entry) {
            tracing::warn!(
                target: "orbit.core.admission",
                run_id,
                %error,
                "could not record the owner's executor census row"
            );
        }
    }
}
