//! The `resident_due` consumer: one member per due mandate.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_automation::AutomationError;
use orbit_automation::members::{MemberAdmission, MemberHost, MemberOutcome, MemberPage};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::Task;
use orbit_types::workflow::RESIDENT_TAG;
use orbit_types::workflow::automation::members::{
    MemberAttempt, MemberBatchEvidence, MemberEvidence, StateMember, bundle_crew,
};
use serde_json::json;

use super::super::Host;
use crate::application::resident::{ResidentDue, ResidentMandateState};

/// Why a mandate is not a member now, or its due state.
enum MandateDue {
    NotMember(String),
    Due(Box<ResidentDue>),
}

impl Host<'_> {
    fn mandate_due(&self, task: Task, now: DateTime<Utc>) -> Result<MandateDue, OrbitError> {
        let history = self.runtime.get_task_history(&task.id)?;
        let view = self.runtime.resident_view(task.clone(), &history)?;
        let (state, reason) = view.state();
        if state != ResidentMandateState::Active {
            // A stopped mandate is never a cycle member, but on a real
            // (non-preview) tick recover its handoff when its live cycle
            // died before finalizing, so it reaches review without
            // waiting for a cycle that will never be admitted.
            if state == ResidentMandateState::Ended && !self.dry_run {
                self.runtime.hand_off_stopped_resident_at(&task.id, now)?;
            }
            return Ok(MandateDue::NotMember(reason.unwrap_or_else(|| {
                format!(
                    "mandate_{}",
                    orbit_common::protocol::serde_string_label(&state)
                        .unwrap_or_else(|| "inactive".to_string())
                )
            })));
        }
        let Ok(mandate) = view.mandate else {
            return Ok(MandateDue::NotMember("mandate_invalid".into()));
        };
        Ok(MandateDue::Due(Box::new(
            self.runtime.resident_due(&task, mandate, &history, now)?,
        )))
    }

    /// Every active mandate that is due, or due and held by its floor or a
    /// live cycle. Mandates are few, so one page holds them all.
    pub(super) fn observe_resident_page(
        &self,
        now: DateTime<Utc>,
    ) -> Result<MemberPage, AutomationError> {
        self.resident_now.set(Some(now));
        let (_, source) = self.head(&self.trigger.branch)?;
        let statuses = self.observed_statuses();
        let mut candidates = Vec::new();
        let mut withheld = BTreeMap::new();
        for task in self.runtime.list_task_metadata()? {
            if !task.tags.iter().any(|tag| tag == RESIDENT_TAG) || !statuses.contains(&task.status)
            {
                continue;
            }
            let task = self.runtime.get_task(&task.id)?;
            let id = task.id.to_string();
            let due = match self.mandate_due(task, now)? {
                MandateDue::NotMember(_) => continue,
                MandateDue::Due(due) => due,
            };
            if let Some(reason) = &due.withheld {
                withheld.insert(id, reason.clone());
                continue;
            }
            if due.reasons.is_empty() {
                continue;
            }
            candidates.push(StateMember {
                key: id.clone(),
                task_ids: vec![id.clone()],
                fingerprint: due.fingerprint(),
                source: source.clone(),
                evidence: json!({"task_id": id, "reasons": due.reasons, "digest": due.digest}),
                // Due as soon as the floor allows: the wait began when the
                // last cycle started.
                first_seen: due.episode_start,
                changed_at: due.episode_start,
                crew: bundle_crew(Some(&due.mandate.crew)),
            });
        }
        Ok(MemberPage {
            candidates,
            withheld,
            next: None,
        })
    }

    /// Admit only a mandate still due for the same reason, with no live
    /// cycle and its floor elapsed.
    pub(super) fn resident_admission(
        &self,
        member: &StateMember,
    ) -> Result<MemberAdmission, AutomationError> {
        let task = match self.runtime.get_task(&member.key) {
            Ok(task) => task,
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Task,
                ..
            }) => return Ok(MemberAdmission::Retire("task_deleted".into())),
            Err(error) => return Err(error.into()),
        };
        let now = self.resident_now.get().unwrap_or_else(Utc::now);
        let due = match self.mandate_due(task, now)? {
            MandateDue::NotMember(reason) => return Ok(MemberAdmission::Retire(reason)),
            MandateDue::Due(due) => due,
        };
        if let Some(reason) = due.withheld {
            return Ok(MemberAdmission::Withhold(reason));
        }
        if due.reasons.is_empty() {
            return Ok(MemberAdmission::Retire("not_due".into()));
        }
        if due.fingerprint() != member.fingerprint {
            return Ok(MemberAdmission::Retire("material_changed".into()));
        }
        Ok(MemberAdmission::Admit)
    }

    /// A cycle run is applied once it stopped after starting its cycle
    /// (finalize or the next observe closes the cycle either way); a run
    /// that stopped without one failed and retries.
    pub(super) fn resident_outcome(
        &self,
        attempt: &MemberAttempt,
        action_id: &str,
        stopped: bool,
    ) -> Result<MemberOutcome, AutomationError> {
        if !stopped {
            return Ok(MemberOutcome::Pending);
        }
        let mut applied = Vec::new();
        let mut failed = BTreeMap::new();
        let mut superseded = BTreeMap::new();
        let cycle = crate::application::resident::resident_cycle_id(action_id);
        for member in attempt.members() {
            let history = match self.runtime.get_task_history(&member.key) {
                Ok(history) => history,
                Err(OrbitError::NotFound {
                    kind: NotFoundKind::Task,
                    ..
                }) => {
                    superseded.insert(member.key.clone(), "task_deleted".into());
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let started = history.iter().any(|entry| {
                entry.event == orbit_types::workflow::RESIDENT_CYCLE_GRANTED_EVENT
                    && entry
                        .note
                        .as_deref()
                        .and_then(|note| serde_json::from_str::<serde_json::Value>(note).ok())
                        .is_some_and(|grant| grant["cycle"] == cycle.as_str())
            });
            if !started {
                failed.insert(member.key.clone(), "stopped_without_cycle".into());
                continue;
            }
            applied.push(MemberEvidence {
                action_id: action_id.into(),
                attempt_id: attempt.id.clone(),
                member_key: member.key.clone(),
                input_fingerprint: member.fingerprint.clone(),
                resulting_fingerprint: member.fingerprint.clone(),
                ready: true,
                result: json!({"task_id": member.key, "cycle": cycle}),
            });
        }
        Ok(MemberOutcome::Settled(MemberBatchEvidence {
            action_id: action_id.into(),
            attempt_id: attempt.id.clone(),
            applied,
            failed,
            superseded,
        }))
    }
}
