//! Workspace capability-gap ledger and proposal suppression.

use crate::OrbitRuntime;
use crate::application::task::TaskEvolutionPort;
use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::EvolutionStoreBackend;
use orbit_types::record::{EvolutionOccurrence, EvolutionRecord, EvolutionState};
use orbit_types::task::{TaskComment, TaskStatus};
use orbit_types::workflow::{ResidentComment, ResidentCostEvent};
use serde_json::{Value, json};

fn invalid(message: impl Into<String>) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}

fn reopen_if_due(record: &mut EvolutionRecord, now: DateTime<Utc>) {
    if record.state != EvolutionState::Declined {
        return;
    }
    let totals = record.evolution_totals(Some(now - Duration::days(i64::from(record.window_days))));
    let cost = totals.minutes.saturating_add(totals.slot_minutes);
    // A zero-cost decline must gain real cost, not immediately satisfy 0 >= 0.
    let doubled = cost > 0 && u128::from(cost) >= u128::from(record.cost_at_decline) * 2;
    if record.declined_until.is_some_and(|until| now >= until) || doubled {
        record.state = EvolutionState::Open;
        record.proposal_task = None;
        record.declined_until = None;
    }
}
fn decline(record: &mut EvolutionRecord, now: DateTime<Utc>, days: u32) {
    let totals = record.evolution_totals(Some(now - Duration::days(i64::from(record.window_days))));
    record.cost_at_decline = totals.minutes.saturating_add(totals.slot_minutes);
    record.declined_until = Some(now + Duration::days(i64::from(days)));
    record.state = EvolutionState::Declined;
}
fn view(mut record: EvolutionRecord, since: Option<DateTime<Utc>>) -> Result<Value, OrbitError> {
    reopen_if_due(&mut record, Utc::now());
    let totals = record.evolution_totals(since);
    let mut value = serde_json::to_value(record).map_err(|error| invalid(error.to_string()))?;
    let fields = serde_json::to_value(totals).map_err(|error| invalid(error.to_string()))?;
    if let (Some(value), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
        value.extend(fields.clone());
    }
    Ok(value)
}

impl TaskEvolutionPort for OrbitRuntime {
    fn project_task_checkpoint_costs(&self, comments: &[TaskComment]) -> Result<(), OrbitError> {
        self.ingest_checkpoint_costs(comments)
    }
}

impl OrbitRuntime {
    fn evolution_store(&self) -> Result<std::sync::Arc<dyn EvolutionStoreBackend>, OrbitError> {
        Ok(orbit_store::compose::evolution_store(self.sqlite_store()?))
    }
    /// Persist checkpoint costs after the assignment comment commits. The source
    /// key makes replay safe; the comment remains the canonical recovery source.
    pub(crate) fn ingest_checkpoint_costs(
        &self,
        comments: &[TaskComment],
    ) -> Result<(), OrbitError> {
        let mut events = Vec::new();
        for comment in comments {
            if let Ok(ResidentComment::Checkpoint(checkpoint)) =
                ResidentComment::parse(&comment.message)
            {
                for (index, cost) in checkpoint.costs.iter().enumerate() {
                    events.push(EvolutionOccurrence {
                        at: comment.at,
                        mandate: Some(checkpoint.mandate.clone()),
                        cycle: Some(checkpoint.cycle.clone()),
                        source_key: format!(
                            "checkpoint:{}:{}:{index}",
                            checkpoint.mandate, checkpoint.cycle
                        ),
                        cost: cost.clone(),
                    });
                }
            }
        }
        if !events.is_empty() {
            self.ensure_coordination_task_write_permitted()?;
            self.refresh_evolution_proposals()?;
            self.evolution_store()?
                .record_evolution_costs(&self.workspace_id()?, &events)?;
        }
        Ok(())
    }

    /// Repair projection after a checkpoint append whose post-commit ingestion
    /// was interrupted. Safe to call repeatedly, including on a duplicate write.
    pub fn reconcile_evolution_checkpoint(&self, task_id: &str) -> Result<(), OrbitError> {
        self.ingest_checkpoint_costs(&self.get_task_comments(task_id)?)
    }

    /// Record one interactive cost using the same validator and ledger as residents.
    pub(crate) fn add_evolution_cost(
        &self,
        cost: ResidentCostEvent,
        mandate: Option<String>,
        cycle: Option<String>,
    ) -> Result<Value, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        cost.validate_cost_event()
            .map_err(|error| invalid(error.to_string()))?;
        if let Some(id) = &mandate {
            self.get_task(id)?;
        }
        if cycle.is_some() && mandate.is_none() {
            return Err(invalid("`cycle` requires `mandate`"));
        }
        if cycle.as_ref().is_some_and(|cycle| cycle.trim().is_empty()) {
            return Err(invalid("`cycle` must not be empty"));
        }
        self.refresh_evolution_proposals()?;
        let event = EvolutionOccurrence {
            at: Utc::now(),
            mandate,
            cycle,
            source_key: format!(
                "interactive:{:016x}",
                getrandom::u64().map_err(|error| OrbitError::Execution(error.to_string()))?
            ),
            cost,
        };
        let records = self
            .sqlite_store()?
            .record_evolution_costs(&self.workspace_id()?, std::slice::from_ref(&event))?;
        let record = records
            .into_iter()
            .next()
            .map(|record| view(record, None))
            .transpose()?;
        Ok(json!({"event": event, "record": record}))
    }
    fn refresh_evolution_proposals(&self) -> Result<(), OrbitError> {
        let store = self.evolution_store()?;
        let workspace = self.workspace_id()?;
        for record in store.evolution_records(&workspace)? {
            let effective = self.evolution_proposal_state(record.clone())?;
            if effective != record {
                store.mutate_evolution(&workspace, &record.id, &|current| {
                    *current = self.evolution_proposal_state(current.clone())?;
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
    /// Workspace-only reads; friction rows cannot enter this query.
    pub(crate) fn list_evolution_records(
        &self,
        state: Option<EvolutionState>,
        since: Option<DateTime<Utc>>,
    ) -> Result<Value, OrbitError> {
        let mut rows = Vec::new();
        for record in self
            .sqlite_store()?
            .evolution_records(&self.workspace_id()?)?
        {
            let row = view(self.evolution_proposal_state(record)?, since)?;
            if state.is_none_or(|state| {
                serde_json::to_value(state).ok().as_ref() == Some(&row["state"])
            }) && (since.is_none() || row["occurrence_count"].as_u64().unwrap_or_default() > 0)
            {
                rows.push(row);
            }
        }
        rows.sort_by(|a, b| {
            let cost = |row: &Value| {
                row["minutes"]
                    .as_u64()
                    .unwrap_or_default()
                    .saturating_add(row["slot_minutes"].as_u64().unwrap_or_default())
            };
            cost(b)
                .cmp(&cost(a))
                .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
        });
        Ok(json!({"count": rows.len(), "records": rows}))
    }
    pub(crate) fn show_evolution_record(&self, id: &str) -> Result<Value, OrbitError> {
        let record = self
            .sqlite_store()?
            .evolution_records(&self.workspace_id()?)?
            .into_iter()
            .find(|record| record.id == id)
            .ok_or_else(|| invalid("evolution record not found in this workspace"))?;
        view(self.evolution_proposal_state(record)?, None)
    }
    // A read derives terminal proposal state from the task's actual transition
    // time; it never starts a fresh suppression window each time it is read.
    fn evolution_proposal_state(
        &self,
        mut record: EvolutionRecord,
    ) -> Result<EvolutionRecord, OrbitError> {
        if record.state == EvolutionState::Proposed
            && let Some(id) = &record.proposal_task
        {
            let task = self.get_task(id)?;
            if task.status == TaskStatus::Done {
                record.state = EvolutionState::Resolved;
            } else if task.status == TaskStatus::Rejected {
                let at = self
                    .get_task_history(id)?
                    .iter()
                    .rev()
                    .find(|entry| entry.to_status == Some(TaskStatus::Rejected))
                    .map(|entry| entry.at)
                    .unwrap_or(task.updated_at);
                let mut baseline = record.clone();
                baseline.window_days = super::EVOLUTION_WINDOW_DAYS;
                baseline.occurrences.retain(|event| event.at <= at);
                decline(&mut baseline, at, super::EVOLUTION_DECLINE_SUPPRESS_DAYS);
                record.window_days = baseline.window_days;
                record.state = baseline.state;
                record.declined_until = baseline.declined_until;
                record.cost_at_decline = baseline.cost_at_decline;
            }
        }
        reopen_if_due(&mut record, Utc::now());
        Ok(record)
    }
    /// Operator decline; the CLI/tool authorization boundary restricts callers.
    pub fn decline_evolution_record(
        &self,
        id: &str,
        days: u32,
    ) -> Result<EvolutionRecord, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        if days == 0 || days > 3650 {
            return Err(invalid("suppression days must be between 1 and 3650"));
        }
        self.evolution_store()?
            .mutate_evolution(&self.workspace_id()?, id, &|record| {
                *record = self.evolution_proposal_state(record.clone())?;
                if record.state == EvolutionState::Resolved {
                    return Err(invalid("resolved evolution records cannot be declined"));
                }
                record.window_days = super::EVOLUTION_WINDOW_DAYS;
                decline(record, Utc::now(), days);
                Ok(())
            })
    }
}
