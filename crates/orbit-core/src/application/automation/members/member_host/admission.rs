use chrono::Utc;
use orbit_automation::AutomationError;
use orbit_automation::members::{MemberAdmission, MemberHost};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::workflow::automation::members::{StateMember, StateTriggerKind};

use super::super::super::preparation;
use super::super::Host;
use super::host::{Escalation, already_preparing, already_triaging, already_verifying};
use crate::application::task::{finding_fingerprint, needs_verification};

impl Host<'_> {
    pub(super) fn admission_for(
        &self,
        member: &StateMember,
    ) -> Result<MemberAdmission, AutomationError> {
        if self.trigger.kind == StateTriggerKind::ExecutionFailed
            && self
                .incidents
                .borrow_mut()
                .inventory(self.runtime)?
                .get(&member.key)
                != Some(&member.task_ids)
        {
            return Ok(MemberAdmission::Retire(
                "incident_membership_or_recovery_changed".into(),
            ));
        }

        // Re-derive the material now: a member whose input moved may not be admitted.
        // Branch head is invariant for this call; resolve it once rather than
        // per task_id; observation and admission share the cached source.
        if self.trigger.kind == StateTriggerKind::PreparationEligible {
            let (_, source) = self.head(&self.trigger.branch)?;
            // Retained pending members may be off the current observation
            // page, or another pilot may have prepared them since observation.
            let active_preparations = self.cached_preparations("task_pilot", || {
                preparation::active_task_pilot_preparations(self.runtime)
            })?;
            for id in &member.task_ids {
                let task = match self.cached_task(id) {
                    Ok(task) => task,
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::Task,
                        ..
                    }) => return Ok(MemberAdmission::Retire("task_deleted".into())),
                    Err(error) => return Err(error.into()),
                };
                if !orbit_automation::members::preparation::eligible(&task, self.eligibility()) {
                    return Ok(MemberAdmission::Retire("task_ineligible".into()));
                }
                if let Some(run_ids) = active_preparations.get(id) {
                    return Ok(MemberAdmission::Withhold(already_preparing(run_ids)));
                }
                if self.fingerprint(&task, &source.commit)? != member.fingerprint {
                    return Ok(MemberAdmission::Retire("material_changed".into()));
                }
                // Pending members can be off the observation page when an
                // explicit pilot settles. Recheck accepted evidence here too,
                // after the active-holder guard, before creating any run.
                if self
                    .runtime
                    .fresh_task_pilot_assessment(id, &member.fingerprint)?
                    .is_some()
                {
                    return Ok(MemberAdmission::Retire("assessment_fresh".into()));
                }
            }
            // A member retained from before the hold started is still pending.
            // Withhold it here, after the active-pilot guard, so the routine
            // does not submit a run the provider would fail.
            if let Some(reason) = self.cached_pilot_provider_withhold() {
                return Ok(MemberAdmission::Withhold(reason));
            }
            return Ok(MemberAdmission::Admit);
        }

        if self.trigger.kind == StateTriggerKind::FindingUnverified {
            let active = self.cached_preparations("finding_verification", || {
                preparation::active_finding_verifications(self.runtime)
            })?;
            for id in &member.task_ids {
                let task = match self.cached_task(id) {
                    Ok(task) => task,
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::Task,
                        ..
                    }) => return Ok(MemberAdmission::Retire("task_deleted".into())),
                    Err(error) => return Err(error.into()),
                };
                // Deletion can also happen between reading the task and its history.
                let history = match self.runtime.get_task_history(id) {
                    Ok(history) => history,
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::Task,
                        ..
                    }) => return Ok(MemberAdmission::Retire("task_deleted".into())),
                    Err(error) => return Err(error.into()),
                };
                if !needs_verification(&task, &history) {
                    return Ok(MemberAdmission::Retire(
                        "finding_verified_or_ineligible".into(),
                    ));
                }
                if let Some(run_ids) = active.get(id) {
                    return Ok(MemberAdmission::Withhold(already_verifying(run_ids)));
                }
                if finding_fingerprint(&task) != member.fingerprint {
                    return Ok(MemberAdmission::Retire("material_changed".into()));
                }
            }
            return Ok(MemberAdmission::Admit);
        }

        if self.trigger.kind == StateTriggerKind::DecisionRequested {
            use orbit_types::task::DecisionRequestState;
            if !self.runtime.decision_channel_enabled() {
                return Ok(MemberAdmission::Retire("decision_channel_disabled".into()));
            }
            for id in &member.task_ids {
                let request = match self.cached_task(id) {
                    Ok(_) => match self.decision_request_of(member)? {
                        Some(request) => request,
                        None => {
                            return Ok(MemberAdmission::Retire("request_superseded".into()));
                        }
                    },
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::Task,
                        ..
                    }) => return Ok(MemberAdmission::Retire("task_deleted".into())),
                    Err(error) => return Err(error.into()),
                };
                match request.state {
                    DecisionRequestState::Open => {}
                    DecisionRequestState::Answered => {
                        return Ok(MemberAdmission::Retire("request_answered".into()));
                    }
                    DecisionRequestState::Superseded => {
                        return Ok(MemberAdmission::Retire("request_superseded".into()));
                    }
                    DecisionRequestState::Expired => {
                        return Ok(MemberAdmission::Retire("request_expired".into()));
                    }
                }
            }
            return Ok(MemberAdmission::Admit);
        }

        if self.is_escalation_triage() {
            let active = self.cached_preparations("escalation_triage", || {
                preparation::active_escalation_triages(self.runtime)
            })?;
            for id in &member.task_ids {
                let escalation = match self
                    .cached_task(id)
                    .and_then(|task| self.escalation_of(&task, Utc::now()))
                {
                    Ok(escalation) => escalation,
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::Task,
                        ..
                    }) => return Ok(MemberAdmission::Retire("task_deleted".into())),
                    Err(error) => return Err(error.into()),
                };
                match escalation {
                    Escalation::Qualifies(fingerprint) if fingerprint == member.fingerprint => {}
                    // A re-pilot wrote another assessment, or the task was
                    // blocked again; that is its own member.
                    Escalation::Qualifies(_) => {
                        return Ok(MemberAdmission::Retire("material_changed".into()));
                    }
                    Escalation::Gone(reason) => return Ok(MemberAdmission::Retire(reason)),
                }
                if let Some(run_ids) = active.get(id) {
                    return Ok(MemberAdmission::Withhold(already_triaging(run_ids)));
                }
            }
            return Ok(MemberAdmission::Admit);
        }

        for id in &member.task_ids {
            let task = self.cached_task(id)?;
            let current = match super::super::super::incidents::observe(self.runtime, &task) {
                Ok((key, _)) => key,
                Err(error) => {
                    return Ok(MemberAdmission::Retire(error.to_string()));
                }
            };

            if current != member.fingerprint {
                return Ok(MemberAdmission::Retire("material_changed".into()));
            }
        }

        Ok(MemberAdmission::Admit)
    }
}
