//! Candidate selection for one pull admission [ORB-14724].
//!
//! The admission section holds the host-wide exclusive commit boundary, so
//! every task write on the host waits while it runs. Candidates are therefore
//! selected before it, from the generated task index under the shared
//! boundary, and the section re-reads only what one decision rests on: the
//! in-flight tasks whose footprints can conflict, and a surviving candidate
//! with its dependencies. Every rule that admits or defers a candidate lives in
//! [`Screen`], which judges the selection and the section's re-read alike.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::selector::canonical_selector_in_workspace;
use orbit_types::task::{
    Task, TaskStatus, automatic_dispatch_cmp, satisfy_completed_archived_dependencies,
};

use super::TaskCommitBoundary;
use super::admission::{canonical_footprint, os_unavailable, overlaps};
use super::lifecycle::DrainReleases;
use crate::contracts::*;
use crate::repository::task::v2::{TaskV2Store, task_history_from_events};

fn is_in_flight(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::InProgress | TaskStatus::Review)
}

/// What one admission selects before its exclusive section [ORB-14724].
pub(super) struct AdmissionSnapshot {
    /// `backlog` tasks, in automatic dispatch order.
    pub(super) backlog: Vec<Task>,
    /// Ids of `in-progress` and `review` tasks.
    pub(super) in_flight: BTreeSet<String>,
    /// The status of each backlog task's dependencies.
    pub(super) statuses: BTreeMap<String, TaskStatus>,
    /// Whether the generated index was proven fresh when selecting.
    pub(super) indexed: bool,
}

/// The in-flight tasks an admission section read, with each one's canonical
/// footprint.
pub(super) struct InFlight {
    pub(super) tasks: Vec<Task>,
    pub(super) footprints: Vec<(String, Vec<String>)>,
}

/// The facts that rule a backlog candidate in or out, read inside the
/// admission section apart from the caller's holds.
pub(super) struct Screen<'a> {
    pub(super) identity: &'a AdmissionIdentity,
    pub(super) request: &'a AdmissionRequest,
    pub(super) repo_root: &'a Path,
    pub(super) admission_holds: &'a BTreeMap<String, String>,
    pub(super) held: &'a BTreeMap<String, String>,
    pub(super) drain_released: &'a DrainReleases,
    pub(super) claims: &'a [ExecutionClaim],
    /// Each in-flight task's id and canonical footprint.
    pub(super) in_flight: &'a [(String, Vec<String>)],
    pub(super) reservations: &'a [ActiveTaskReservation],
}

impl Screen<'_> {
    /// Whether `task` counts toward the receipt's queue depth: `backlog`,
    /// held by nothing, and every dependency `done`.
    pub(super) fn queued(&self, task: &Task, statuses: &BTreeMap<String, TaskStatus>) -> bool {
        task.status == TaskStatus::Backlog
            && !self.admission_holds.contains_key(&task.id)
            && !self.held.contains_key(&task.id)
            && task
                .dependencies()
                .iter()
                .all(|id| statuses.get(id) == Some(&TaskStatus::Done))
    }

    /// `task`'s canonical footprint when this executor may take it now, or
    /// `None` once `receipt` says why not.
    pub(super) fn footprint(
        &self,
        task: &Task,
        statuses: &BTreeMap<String, TaskStatus>,
        receipt: &mut AdmissionReceipt,
    ) -> Option<Vec<String>> {
        let diagnostic = |reason: String, blocked_by: Vec<String>| AdmissionDiagnostic {
            task_id: task.id.clone(),
            reason,
            blocked_by,
        };
        let request = self.request;
        // Side-effect-only work goes to an executor whose protocol includes
        // the verified NoDiff handoff [ORB-14259]. Admission refuses an older
        // caller revision before this point, so the deferral guards a relaxed
        // schema check.
        if self.identity.is_remote()
            && request.caller_schema < NO_DIFF_HANDOFF_PROTOCOL_SCHEMA
            && task
                .tags
                .iter()
                .any(|tag| tag == orbit_types::task::NO_DIFF_EXPECTED_TAG)
        {
            receipt.deferred_conflicts.push(diagnostic(
                format!(
                    "no-diff-expected work waits for an executor with the NoDiff handoff (protocol revision {NO_DIFF_HANDOFF_PROTOCOL_SCHEMA}); the executor runs revision {}",
                    request.caller_schema
                ),
                Vec::new(),
            ));
            return None;
        }
        let unmet = task
            .dependencies()
            .into_iter()
            .filter(|id| statuses.get(id) != Some(&TaskStatus::Done))
            .collect::<Vec<_>>();
        if !unmet.is_empty() {
            receipt.invalid_candidates.push(diagnostic(
                "dependency is missing or not done".into(),
                unmet,
            ));
            return None;
        }
        if let Some(reason) = self.admission_holds.get(&task.id) {
            receipt
                .deferred_conflicts
                .push(diagnostic(reason.clone(), Vec::new()));
            return None;
        }
        if let Some(why) = self.held.get(&task.id) {
            receipt.deferred_conflicts.push(diagnostic(
                format!("held for a red base: {why}"),
                Vec::new(),
            ));
            return None;
        }
        // A task whose `os:` tags the executor's OS does not satisfy stays
        // for a host that does, rather than being claimed and failed. The
        // tags are read at each admission, so a retag applies to the next.
        if let Some(reason) = os_unavailable(task, request) {
            receipt.os_unavailable.push(diagnostic(reason, Vec::new()));
            return None;
        }
        // A task the executor cannot run stays for the owner or another
        // follower; claiming it would only burn the claim [ORB-13941].
        if let Some(reason) = request
            .crews
            .as_ref()
            .and_then(|crews| crews.unrunnable_reason(task.crew.as_deref()))
        {
            receipt
                .crew_unavailable
                .push(diagnostic(reason, Vec::new()));
            return None;
        }
        // Nor one this drain already gave back for its host's failure, nor
        // any task once a release blamed the host whatever crew runs there
        // [ORB-14257]; they stay for another host or a later drain.
        if let Some((why, release)) = self
            .drain_released
            .tasks
            .get(&task.id)
            .map(|release| ("this drain released it", release))
            .or_else(|| {
                self.drain_released
                    .host
                    .as_ref()
                    .map(|release| ("this drain's host is suppressed for its window", release))
            })
        {
            receipt.crew_unavailable.push(diagnostic(
                format!("{why} ({}): {}", release.class.as_str(), release.reason),
                Vec::new(),
            ));
            return None;
        }
        let footprint = match canonical_footprint(&task.context_files, self.repo_root) {
            Ok(files) => files,
            Err(error) => {
                receipt
                    .invalid_candidates
                    .push(diagnostic(error.to_string(), Vec::new()));
                return None;
            }
        };
        let blocker = self
            .claims
            .iter()
            .find(|claim| {
                claim.phase.protects_footprint()
                    && (claim.task_id == task.id || overlaps(&footprint, &claim.footprint))
            })
            .map(|claim| claim.task_id.as_str())
            .or_else(|| {
                self.in_flight
                    .iter()
                    .find(|(_, files)| overlaps(&footprint, files))
                    .map(|(id, _)| id.as_str())
            })
            .or_else(|| {
                self.reservations
                    .iter()
                    .find(|reservation| overlaps(&footprint, &reservation.files))
                    .map(|reservation| reservation.reservation_id.as_str())
            });
        if let Some(blocker) = blocker {
            receipt.deferred_conflicts.push(diagnostic(
                format!("protected footprint held by {blocker}"),
                vec![blocker.to_string()],
            ));
            return None;
        }
        Some(footprint)
    }
}

impl TaskCommitBoundary {
    /// The partition's backlog and in-flight tasks, read under the ordinary
    /// (shared) boundary, with the statuses the backlog's dependencies have.
    pub(super) fn admission_snapshot(&self) -> Result<AdmissionSnapshot, OrbitError> {
        self.enter_ordinary(|| {
            let translator = TaskV2Store::new(self.registry.clone(), self.workspace_id.clone());
            let filter = TaskIndexFilter {
                statuses: vec![
                    TaskStatus::Backlog,
                    TaskStatus::InProgress,
                    TaskStatus::Review,
                ],
                ..TaskIndexFilter::default()
            };
            let (tasks, indexed) = match translator.indexed_tasks(&filter)? {
                Some(tasks) => (tasks, true),
                None => (translator.tasks_for_index_filter(filter)?, false),
            };
            let known = tasks
                .iter()
                .map(|task| (task.id.clone(), task.status))
                .collect::<BTreeMap<_, _>>();
            let mut backlog = Vec::new();
            let mut in_flight = BTreeSet::new();
            for task in tasks {
                match task.status {
                    TaskStatus::Backlog => backlog.push(task),
                    status if is_in_flight(status) => {
                        in_flight.insert(task.id);
                    }
                    _ => {}
                }
            }
            backlog.sort_by(automatic_dispatch_cmp);
            let dependencies = backlog.iter().flat_map(Task::dependencies).collect();
            let statuses = self.dependency_statuses(dependencies, &known)?;
            Ok(AdmissionSnapshot {
                backlog,
                in_flight,
                statuses,
                indexed,
            })
        })
    }

    /// The status of each of `dependencies`: from `known`, or else read from
    /// the bundle of whichever workspace partition holds it. A completed
    /// archived dependency reads as `done`; an id no partition holds is
    /// absent.
    pub(super) fn dependency_statuses(
        &self,
        dependencies: BTreeSet<String>,
        known: &BTreeMap<String, TaskStatus>,
    ) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        let mut statuses = BTreeMap::new();
        let mut archived_histories = BTreeMap::new();
        for dependency in &dependencies {
            if let Some(status) = known.get(dependency) {
                statuses.insert(dependency.clone(), *status);
                continue;
            }
            let Some(binding) = self.registry.find_task_binding(dependency)? else {
                continue;
            };
            let bundle = if binding.partition_id == self.workspace_id {
                self.bundle_store.read_bundle_if_settled(dependency)?
            } else {
                // Inside the admission section the host lock excludes every
                // partition's ordinary writers, so this cannot move there.
                let owner = TaskCommitBoundary {
                    store: self.store.clone(),
                    registry: self.registry.clone(),
                    bundle_store: crate::repository::task::v2_bundle::TaskBundleStoreV2::new(
                        self.registry.clone(),
                        binding.partition_id.clone(),
                    ),
                    workspace_id: binding.partition_id.clone(),
                    partition_dir: self
                        .registry
                        .workspace_partition_dir(&binding.partition_id)?,
                };
                owner.verify_journal_binding()?;
                owner.recover_if_pending()?;
                match owner.bundle_store.read_bundle_lightweight(dependency) {
                    Ok(bundle) => Some(bundle),
                    Err(OrbitError::NotFound { .. }) => None,
                    Err(error) => return Err(error),
                }
            };
            let Some(bundle) = bundle else {
                continue;
            };
            statuses.insert(dependency.clone(), bundle.envelope.status);
            if bundle.envelope.status == TaskStatus::Archived {
                archived_histories
                    .insert(dependency.clone(), task_history_from_events(bundle.events));
            }
        }
        // Project dependency satisfaction from the snapshots read here,
        // without changing stored statuses. An absent or incomplete
        // completion history keeps the archived dead end.
        let Ok(()) = satisfy_completed_archived_dependencies::<std::convert::Infallible>(
            &mut statuses,
            dependencies,
            |id| Ok(archived_histories.remove(id)),
        );
        Ok(statuses)
    }

    /// The in-flight tasks as the admission section reads them, with each
    /// one's canonical footprint computed once for every candidate.
    ///
    /// Every transition publishes its index row inside the boundary, so with
    /// the exclusive section held the index names the in-flight tasks; the
    /// selection's own are added in case a row update failed. When selection
    /// could not prove the index fresh, every bundle is listed instead.
    pub(super) fn in_flight_locked(
        &self,
        snapshot: &AdmissionSnapshot,
        repo_root: &Path,
    ) -> Result<InFlight, OrbitError> {
        let bundles = if snapshot.indexed {
            let mut ids = self
                .registry
                .indexed_task_ids_filtered(
                    &self.workspace_id,
                    &TaskIndexFilter {
                        statuses: vec![TaskStatus::InProgress, TaskStatus::Review],
                        ..TaskIndexFilter::default()
                    },
                )?
                .into_iter()
                .collect::<BTreeSet<_>>();
            ids.extend(snapshot.in_flight.iter().cloned());
            let mut bundles = Vec::with_capacity(ids.len());
            for id in &ids {
                bundles.extend(self.bundle_store.read_bundle_if_settled(id)?);
            }
            bundles
        } else {
            self.bundle_store.list_bundles()?
        };
        let translator = TaskV2Store::new(self.registry.clone(), self.workspace_id.clone());
        let tasks = bundles
            .into_iter()
            .filter(|bundle| is_in_flight(bundle.envelope.status))
            .map(|bundle| translator.task_from_bundle(bundle))
            .collect::<Result<Vec<_>, _>>()?;
        let footprints = tasks
            .iter()
            .map(|task| {
                let files = task
                    .context_files
                    .iter()
                    .filter_map(|file| canonical_selector_in_workspace(file, repo_root).ok())
                    .collect();
                (task.id.clone(), files)
            })
            .collect();
        Ok(InFlight { tasks, footprints })
    }
}

/// A hook run between an admission's candidate selection and its exclusive
/// section, so a test can change a candidate in that gap.
#[cfg(test)]
pub(crate) mod after_selection {
    use std::cell::RefCell;

    thread_local! {
        static HOOK: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    /// Run `hook` once, in the next admission on this thread.
    pub(crate) fn set(hook: impl FnOnce() + 'static) {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    pub(in super::super) fn run() {
        if let Some(hook) = HOOK.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
    }
}
