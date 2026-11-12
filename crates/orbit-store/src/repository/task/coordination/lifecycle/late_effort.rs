//! A follower's late report of a settled leaf's reasoning effort
//! [ORB-24115] and implementing model [ORB-24150].

use chrono::Utc;
use orbit_common::OrbitError;

use super::super::TaskCommitBoundary;
use super::codec::{CLAIM, HANDOFF_SETTLEMENT, LATE_LEAF_CREW, LATE_LEAF_EFFORT, LATE_LEAF_MODEL};
use super::{decode, invalid, row};
use crate::contracts::*;

/// The task history event a claim's pull writes; its note names the claim.
const PULLED_BY_EVENT: &str = "pulled_by";

impl TaskCommitBoundary {
    /// Record `effort` (`None` for the provider default) and, when given,
    /// `model` as what the settled claim `claim_id`'s leaf ran its
    /// implementing agent at and on, as `machine_id` reports it after the
    /// fact.
    ///
    /// Effort and model are each kept in their own immutable row; crew is kept
    /// with effort or in a companion row when enriching a prior crewless
    /// report. Nothing changes the claim, task, run or settlement record:
    /// inspection fills absent settlement attribution from these rows. The
    /// report is refused unless the claim is settled, ran on `machine_id` and
    /// was bound to `run_id` there. A claim the owner settled before it kept
    /// settlement records is accepted only when the task's history also names
    /// the claim as pulled; its reports are the only attribution it has. A
    /// value the claim already holds (on its settlement or from an earlier
    /// report) replays; a different one keeps the held value and reports the
    /// conflict, and a conflict on either writes neither.
    pub fn record_late_leaf_report(
        &self,
        claim_id: &str,
        machine_id: &str,
        run_id: &str,
        effort: Option<String>,
        crew: Option<String>,
        model: Option<String>,
    ) -> Result<LateLeafReportWrite, OrbitError> {
        self.with_admission(|| {
            let claim: ExecutionClaim = self
                .store
                .task_coordination_row(&self.workspace_id, CLAIM, claim_id)?
                .map(|row| decode(&row.payload_json))
                .transpose()?
                .ok_or_else(|| invalid("unknown claim"))?;
            if claim.executed_on.machine_id != machine_id {
                return Err(OrbitError::PolicyDenied(
                    "not_settling_host: only the machine that ran and settled this claim's leaf \
                     may report its effort"
                        .into(),
                ));
            }
            if claim.phase.is_unsettled() {
                return Err(invalid(
                    "a late effort report is recorded only for a settled claim",
                ));
            }
            let state = self.claim_state(claim.clone())?;
            let bound = state
                .bound_run
                .as_ref()
                .is_some_and(|bound| bound.run_id == run_id && bound.machine_id == machine_id);
            if !bound {
                return Err(OrbitError::PolicyDenied(
                    "not_settling_host: the claim was not bound to this leaf run on its \
                     executing machine"
                        .into(),
                ));
            }
            let handoff = self
                .store
                .task_coordination_row(&self.workspace_id, HANDOFF_SETTLEMENT, claim_id)?
                .map(|row| decode::<ClaimSettlementRecord>(&row.payload_json))
                .transpose()?;
            let settlement = handoff.or(state.settlement);
            if settlement.is_none() && !self.history_names_pull(&claim)? {
                return Err(OrbitError::PolicyDenied(
                    "not_settling_host: the claim holds no settlement record and its task's \
                     history does not name it as pulled"
                        .into(),
                ));
            }
            let recorded_at = Utc::now().to_rfc3339();
            let effort_report = LateLeafEffort {
                machine_id: machine_id.to_string(),
                run_id: run_id.to_string(),
                reasoning_effort: effort,
                crew: crew.clone(),
                recorded_at: recorded_at.clone(),
            };
            let existing_effort = self.late_row::<LateLeafEffort>(LATE_LEAF_EFFORT, claim_id)?;
            let held_effort = settlement
                .as_ref()
                .and_then(|settlement| settlement.reasoning_effort.clone());
            let effort_write = match held_effort {
                Some(held) if held == effort_report.reasoning_effort => {
                    LateLeafEffortWrite::Replayed
                }
                Some(held) => LateLeafEffortWrite::Conflict { held },
                None => match existing_effort.as_ref() {
                    Some(existing) if existing.same_fact(&effort_report) => {
                        LateLeafEffortWrite::Replayed
                    }
                    Some(existing) => LateLeafEffortWrite::Conflict {
                        held: existing.reasoning_effort.clone(),
                    },
                    None => LateLeafEffortWrite::Recorded,
                },
            };
            let existing_crew = self.late_row::<LateLeafCrew>(LATE_LEAF_CREW, claim_id)?;
            let settlement_crew = settlement
                .as_ref()
                .and_then(|settlement| settlement.crew.as_ref());
            let crew_enrichment = crew.as_ref().and_then(|crew| {
                (existing_crew.is_none()
                    && settlement_crew.is_none()
                    && existing_effort
                        .as_ref()
                        .is_none_or(|existing| existing.crew.is_none())
                    && effort_write != LateLeafEffortWrite::Recorded)
                    .then(|| LateLeafCrew {
                        machine_id: machine_id.to_string(),
                        run_id: run_id.to_string(),
                        reasoning_effort: effort_report.reasoning_effort.clone(),
                        crew: crew.clone(),
                        recorded_at: recorded_at.clone(),
                    })
            });
            let model_report = model.map(|crew_model| LateLeafModel {
                machine_id: machine_id.to_string(),
                run_id: run_id.to_string(),
                crew_model,
                recorded_at,
            });
            let model_write = match &model_report {
                None => None,
                Some(report) => Some(
                    match settlement
                        .as_ref()
                        .and_then(|settlement| settlement.crew_model.clone())
                    {
                        Some(held) if held == report.crew_model => LateLeafModelWrite::Replayed,
                        Some(held) => LateLeafModelWrite::Conflict { held },
                        None => match self.late_row::<LateLeafModel>(LATE_LEAF_MODEL, claim_id)? {
                            Some(existing) if existing.same_fact(report) => {
                                LateLeafModelWrite::Replayed
                            }
                            Some(existing) => LateLeafModelWrite::Conflict {
                                held: existing.crew_model,
                            },
                            None => LateLeafModelWrite::Recorded,
                        },
                    },
                ),
            };
            let conflict = matches!(effort_write, LateLeafEffortWrite::Conflict { .. })
                || matches!(model_write, Some(LateLeafModelWrite::Conflict { .. }));
            if !conflict {
                if effort_write == LateLeafEffortWrite::Recorded {
                    self.store.insert_task_coordination_row(
                        &self.workspace_id,
                        &row(LATE_LEAF_EFFORT, claim_id, &effort_report)?,
                        None,
                    )?;
                }
                if let (Some(LateLeafModelWrite::Recorded), Some(report)) =
                    (&model_write, &model_report)
                {
                    self.store.insert_task_coordination_row(
                        &self.workspace_id,
                        &row(LATE_LEAF_MODEL, claim_id, report)?,
                        None,
                    )?;
                }
                if let Some(report) = &crew_enrichment {
                    self.store.insert_task_coordination_row(
                        &self.workspace_id,
                        &row(LATE_LEAF_CREW, claim_id, report)?,
                        None,
                    )?;
                }
            }
            Ok(LateLeafReportWrite {
                effort: effort_write,
                model: model_write,
            })
        })
    }

    fn late_row<T: serde::de::DeserializeOwned>(
        &self,
        kind: &str,
        claim_id: &str,
    ) -> Result<Option<T>, OrbitError> {
        self.store
            .task_coordination_row(&self.workspace_id, kind, claim_id)?
            .map(|row| decode::<T>(&row.payload_json))
            .transpose()
    }

    /// Whether the claim's task history records a pull that names this
    /// claim: the owner's own record that the claim's machine pulled it.
    fn history_names_pull(&self, claim: &ExecutionClaim) -> Result<bool, OrbitError> {
        let bundle = self.bundle_store.read_bundle_lightweight(&claim.task_id)?;
        Ok(bundle.events.iter().any(|event| {
            event.event_type == PULLED_BY_EVENT
                && event
                    .note
                    .as_deref()
                    .and_then(|note| serde_json::from_str::<serde_json::Value>(note).ok())
                    .and_then(|note| {
                        note.get("claim_id")
                            .and_then(serde_json::Value::as_str)
                            .map(|id| id == claim.claim_id)
                    })
                    .unwrap_or(false)
        }))
    }
}

/// Projection only: keep the claim's late effort report and fill a
/// settlement that holds no reported effort from it. A settlement that holds
/// one keeps it; the store never records a disagreeing report.
pub(super) fn attach_late_leaf_effort(
    state: &mut ClaimInspection,
    payload: &str,
) -> Result<(), OrbitError> {
    let report: LateLeafEffort = decode(payload)?;
    if let Some(settlement) = state.settlement.as_mut()
        && settlement.reasoning_effort.is_none()
    {
        settlement.reasoning_effort = Some(report.reasoning_effort.clone());
    }
    state.late_leaf_effort = Some(report);
    Ok(())
}

/// Project the crew a newer follower added to a late effort already held by
/// the settlement or an earlier crewless report.
pub(super) fn attach_late_leaf_crew(
    state: &mut ClaimInspection,
    payload: &str,
) -> Result<(), OrbitError> {
    let report: LateLeafCrew = decode(payload)?;
    if let Some(effort) = state.late_leaf_effort.as_mut() {
        if effort.machine_id != report.machine_id
            || effort.run_id != report.run_id
            || effort.reasoning_effort != report.reasoning_effort
            || effort
                .crew
                .as_ref()
                .is_some_and(|crew| crew != &report.crew)
        {
            return Err(invalid("inconsistent late crew attribution"));
        }
        effort.crew = Some(report.crew.clone());
    } else {
        state.late_leaf_effort = Some(LateLeafEffort {
            machine_id: report.machine_id.clone(),
            run_id: report.run_id.clone(),
            reasoning_effort: report.reasoning_effort.clone(),
            crew: Some(report.crew.clone()),
            recorded_at: report.recorded_at.clone(),
        });
    }
    if let Some(settlement) = state.settlement.as_mut()
        && settlement.crew.is_none()
    {
        settlement.crew = Some(report.crew);
    }
    Ok(())
}

/// Projection only: keep the claim's late model report and fill a
/// settlement that holds no model from it.
pub(super) fn attach_late_leaf_model(
    state: &mut ClaimInspection,
    payload: &str,
) -> Result<(), OrbitError> {
    let report: LateLeafModel = decode(payload)?;
    if let Some(settlement) = state.settlement.as_mut()
        && settlement.crew_model.is_none()
    {
        settlement.crew_model = Some(report.crew_model.clone());
    }
    state.late_leaf_model = Some(report);
    Ok(())
}
