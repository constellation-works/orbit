use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_automation::AutomationError;
use orbit_automation::members::{MemberHost, MemberPage};
use orbit_store::contracts::TaskListFilter;
use orbit_types::task::{TaskStatus, TaskType};
use orbit_types::workflow::automation::members::{StateMember, StateTriggerKind, bundle_crew};
use serde_json::json;

use super::super::super::preparation;
use super::super::Host;
use super::host::{already_preparing, already_triaging, already_verifying, incident_crew};
use crate::application::task::escalation_triage_steps::PilotWarningCandidacy;
use crate::application::task::{TriageCandidacy, finding_fingerprint, needs_verification};

impl Host<'_> {
    pub(super) fn observe_page(
        &self,
        after: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<MemberPage, AutomationError> {
        let scan_before = after
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| AutomationError::Evidence(format!("invalid task continuation: {e}")))?;

        let filter = TaskListFilter {
            scan_before,
            statuses: Some(self.observed_statuses()),
            task_type: (self.trigger.kind == StateTriggerKind::FindingUnverified)
                .then_some(TaskType::Bug),
            ..Default::default()
        };
        // With a shared cache, one snapshot of the workspace's observed
        // statuses serves every routine in the request: each page (statuses,
        // type, cursor, limit, canonical order) is derived from it in
        // memory, exactly as the store's in-memory selection derives it.
        // Every observed status is in the snapshot's status set.
        const SNAPSHOT_STATUSES: [TaskStatus; 4] = [
            TaskStatus::Proposed,
            TaskStatus::Backlog,
            TaskStatus::Someday,
            TaskStatus::Blocked,
        ];
        let tasks = match self.inspection_cache {
            Some(cache)
                if filter
                    .statuses
                    .as_ref()
                    .is_some_and(|values| values.iter().all(|s| SNAPSHOT_STATUSES.contains(s))) =>
            {
                let snapshot = cache.task_snapshot(
                    &self.runtime.shared_root().display().to_string(),
                    || {
                        Ok(self
                            .runtime
                            .task_candidates(
                                &TaskListFilter {
                                    statuses: Some(SNAPSHOT_STATUSES.to_vec()),
                                    ..Default::default()
                                },
                                usize::MAX,
                            )?
                            .items)
                    },
                )?;
                let mut items: Vec<_> = snapshot
                    .into_iter()
                    .filter(|task| {
                        filter
                            .statuses
                            .as_ref()
                            .is_none_or(|values| values.contains(&task.status))
                            && filter.task_type.is_none_or(|value| task.task_type == value)
                    })
                    .collect();
                let total_without_cursor = items.len();
                if let Some((at, id)) = &filter.scan_before {
                    items.retain(|task| {
                        task.created_at < *at || (task.created_at == *at && task.id > *id)
                    });
                }
                let total = items.len();
                items.truncate(50);
                orbit_store::contracts::TaskCandidates {
                    items,
                    total,
                    total_without_cursor,
                }
            }
            _ => self.runtime.task_candidates(&filter, 50)?,
        };

        let next = if tasks.total > tasks.items.len() {
            tasks
                .items
                .last()
                .map(|last| serde_json::to_string(&(last.created_at, &last.id)))
                .transpose()
                .map_err(|e| AutomationError::Evidence(e.to_string()))?
        } else {
            None
        };

        let (_, source) = self.head(&self.trigger.branch)?;

        let mut candidates = Vec::new();
        let mut incident_inventory = None;
        let mut withheld = BTreeMap::new();

        let active_preparations = match self.trigger.kind {
            StateTriggerKind::PreparationEligible => match self.inspection_cache {
                Some(cache) => cache.preparations("task_pilot", || {
                    preparation::active_task_pilot_preparations(self.runtime)
                })?,
                None => preparation::active_task_pilot_preparations(self.runtime)?,
            },
            StateTriggerKind::FindingUnverified => match self.inspection_cache {
                Some(cache) => cache.preparations("finding_verification", || {
                    preparation::active_finding_verifications(self.runtime)
                })?,
                None => preparation::active_finding_verifications(self.runtime)?,
            },
            StateTriggerKind::PilotWarningsHeld | StateTriggerKind::BlockEscalated => {
                match self.inspection_cache {
                    Some(cache) => cache.preparations("escalation_triage", || {
                        preparation::active_escalation_triages(self.runtime)
                    })?,
                    None => preparation::active_escalation_triages(self.runtime)?,
                }
            }
            StateTriggerKind::ExecutionFailed => BTreeMap::new(),
        };
        // One scan for the page. Admission of a member retained from an
        // earlier page reads this cache instead of scanning again.
        let provider_hold = (self.trigger.kind == StateTriggerKind::PreparationEligible)
            .then(|| self.pilot_provider_withhold(now))
            .flatten();

        for envelope in tasks.items {
            let task = match self.inspection_cache {
                Some(cache) => cache.task(&envelope.id, || self.runtime.get_task(&envelope.id))?,
                None => self.runtime.get_task(&envelope.id)?,
            };

            match self.trigger.kind {
                StateTriggerKind::PreparationEligible => {
                    if !orbit_automation::members::preparation::eligible(&task, self.eligibility())
                    {
                        withheld.insert(task.id, "task_ineligible".into());
                        continue;
                    }
                    if let Some(run_ids) = active_preparations.get(&task.id) {
                        withheld.insert(task.id, already_preparing(run_ids));
                        continue;
                    }
                    if let Some(reason) = &provider_hold {
                        withheld.insert(task.id, reason.clone());
                        continue;
                    }
                    let fingerprint = match self.fingerprint(&task, &source.commit) {
                        Ok(fingerprint) => fingerprint,
                        Err(error) => {
                            withheld.insert(task.id, error.to_string());
                            continue;
                        }
                    };
                    candidates.push(StateMember {
                        key: task.id.clone(),
                        task_ids: vec![task.id.clone()],
                        fingerprint,
                        source: source.clone(),
                        evidence: json!({"task_id":task.id}),
                        first_seen: now,
                        changed_at: now,
                        crew: bundle_crew(task.crew.as_deref()),
                    });
                }
                StateTriggerKind::FindingUnverified => {
                    // Only a finding still owed a verdict is a member; a
                    // confirmed one, or another bug, is not withheld either.
                    let history = self.runtime.get_task_history(&task.id)?;
                    if !needs_verification(&task, &history) {
                        continue;
                    }
                    if let Some(run_ids) = active_preparations.get(&task.id) {
                        withheld.insert(task.id, already_verifying(run_ids));
                        continue;
                    }
                    candidates.push(StateMember {
                        key: task.id.clone(),
                        task_ids: vec![task.id.clone()],
                        fingerprint: finding_fingerprint(&task),
                        source: source.clone(),
                        evidence: json!({"task_id":task.id}),
                        first_seen: now,
                        changed_at: now,
                        crew: bundle_crew(task.crew.as_deref()),
                    });
                }
                StateTriggerKind::PilotWarningsHeld => {
                    // Only an assessment that still holds on warnings and
                    // awaits triage is a member, keyed by its receipt so
                    // each assessment is read once.
                    let PilotWarningCandidacy::Qualifies(held) =
                        self.runtime.pilot_warning_candidacy_of(&task)?
                    else {
                        continue;
                    };
                    if let Some(run_ids) = active_preparations.get(&task.id) {
                        withheld.insert(task.id, already_triaging(run_ids));
                        continue;
                    }
                    candidates.push(StateMember {
                        key: task.id.clone(),
                        task_ids: vec![task.id.clone()],
                        evidence: json!({"task_id":task.id, "operation_id":held.operation_id}),
                        fingerprint: held.operation_id,
                        source: source.clone(),
                        first_seen: now,
                        changed_at: now,
                        crew: bundle_crew(task.crew.as_deref()),
                    });
                }
                StateTriggerKind::BlockEscalated => {
                    // Only an episode the shared triage rule qualifies is a
                    // member, keyed by the task and fingerprinted by the
                    // episode, so each episode is read once and a fresh
                    // block is a new member.
                    let TriageCandidacy::Qualifies(episode) =
                        self.runtime.block_triage_candidacy_of(&task, now)?
                    else {
                        continue;
                    };
                    if let Some(run_ids) = active_preparations.get(&task.id) {
                        withheld.insert(task.id, already_triaging(run_ids));
                        continue;
                    }
                    candidates.push(StateMember {
                        key: task.id.clone(),
                        task_ids: vec![task.id.clone()],
                        evidence: json!({
                            "task_id": task.id,
                            "episode": episode.key(),
                            "basis": episode.basis,
                        }),
                        fingerprint: episode.key(),
                        source: source.clone(),
                        first_seen: now,
                        changed_at: now,
                        crew: bundle_crew(task.crew.as_deref()),
                    });
                }
                StateTriggerKind::ExecutionFailed => {
                    match super::super::super::incidents::observe(self.runtime, &task) {
                        Ok((key, evidence)) => {
                            if candidates
                                .iter()
                                .any(|other: &StateMember| other.key == key)
                            {
                                continue;
                            }

                            // The full cohort is hydrated at most once per page
                            // and reused across later admissions in this Host
                            // after a freshness check.
                            if incident_inventory.is_none() {
                                incident_inventory =
                                    Some(self.incidents.borrow_mut().inventory(self.runtime)?);
                            }

                            let task_ids = incident_inventory
                                .as_ref()
                                .and_then(|inventory| inventory.get(&key))
                                .cloned()
                                .unwrap_or_default();

                            if task_ids.len() > 50 {
                                withheld.insert(task.id, "incident_member_budget".into());
                                continue;
                            }

                            if task_ids.is_empty() {
                                withheld.insert(task.id, "incident_recovery_pending".into());
                                continue;
                            }

                            // An incident's task_ids span whatever cohort
                            // diagnose grouped, not one task's stored crew,
                            // so the member's own bundle must agree before
                            // it can carry a crew identity at all [ORB-12796].
                            let crew = match incident_crew(self.runtime, &task_ids) {
                                Ok(Some(crew)) => crew,
                                Ok(None) => {
                                    withheld.insert(task.id, "incident_mixed_crew".into());
                                    continue;
                                }
                                Err(error) => {
                                    withheld.insert(task.id, error.to_string());
                                    continue;
                                }
                            };

                            candidates.push(StateMember {
                                fingerprint: key.clone(),
                                key,
                                task_ids,
                                source: source.clone(),
                                evidence,
                                first_seen: now,
                                changed_at: now,
                                crew,
                            });
                        }
                        Err(error) => {
                            withheld.insert(task.id, error.to_string());
                        }
                    }
                }
            }
        }

        Ok(MemberPage {
            candidates,
            withheld,
            next,
        })
    }
}
