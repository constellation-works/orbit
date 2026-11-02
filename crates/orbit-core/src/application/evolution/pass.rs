//! Bounded evolution proposals. The agent drafts data; only this applier can
//! file it, always proposed. A workspace fence and durable record tags recover
//! an interrupted create/evidence/link sequence without another task.
use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::{Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
use orbit_config::{ConfigRoots, load_effective_config};
use orbit_types::record::EvolutionState;
use orbit_types::task::{TaskArtifact, TaskStatus, TaskType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

const PASS_JOB: &str = "evolution_pass_pipeline";

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct EvolutionPassSettings {
    pub(super) window_days: u32,
    pub(super) min_occurrences: u32,
    pub(super) min_distinct_cycles: u32,
    pub(super) min_distinct_mandates: u32,
    pub(super) min_cost_minutes: u32,
    pub(super) max_proposals: u32,
    pub(super) decline_suppress_days: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvolutionDraft {
    record_id: String,
    title: String,
    description: String,
    acceptance_criteria: Vec<String>,
    #[serde(default)]
    duplicate_task: Option<String>,
}

fn cost(row: &Value) -> u64 {
    row["minutes"]
        .as_u64()
        .unwrap_or_default()
        .saturating_add(row["slot_minutes"].as_u64().unwrap_or_default())
}

impl EvolutionPassSettings {
    fn qualifies(&self, row: &Value) -> bool {
        row["state"] == "open"
            && row["occurrence_count"].as_u64().unwrap_or_default()
                >= u64::from(self.min_occurrences)
            && row["cycle_count"].as_u64().unwrap_or_default()
                >= u64::from(self.min_distinct_cycles)
            && row["mandate_count"].as_u64().unwrap_or_default()
                >= u64::from(self.min_distinct_mandates)
            && cost(row) >= u64::from(self.min_cost_minutes)
    }
}

impl OrbitRuntime {
    pub(super) fn evolution_pass_settings(&self) -> Result<EvolutionPassSettings, OrbitError> {
        let effective =
            load_effective_config(&ConfigRoots::new(self.global_root(), self.shared_root()))?;
        let mut values = serde_json::Map::new();
        for name in [
            "window_days",
            "min_occurrences",
            "min_distinct_cycles",
            "min_distinct_mandates",
            "min_cost_minutes",
            "max_proposals",
            "decline_suppress_days",
        ] {
            values.insert(
                name.into(),
                effective
                    .value_for(&format!("resident.evolution.{name}"))
                    .ok_or_else(|| OrbitError::Execution("missing evolution setting".into()))?,
            );
        }
        serde_json::from_value(Value::Object(values))
            .map_err(|error| OrbitError::Execution(error.to_string()))
    }

    fn evolution_ranking_path(&self, run_id: &str) -> Result<PathBuf, OrbitError> {
        if self.show_job_run_observed(run_id)?.job_id != PASS_JOB {
            return Err(OrbitError::PolicyDenied(
                "evolution steps require their owning evolution pass run".into(),
            ));
        }
        Ok(self
            .paths()
            .job_runs_dir
            .join(run_id)
            .join("artifacts/evolution-ranking.json"))
    }

    /// Rank eligible window costs and record this pass even when no agent runs.
    /// Re-entering the same run reuses its frozen ranking.
    pub fn rank_evolution_pass(&self, run_id: &str) -> Result<Value, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let path = self.evolution_ranking_path(run_id)?;
        with_exclusive_file_lock(&path, "evolution ranking", || {
            if path.exists() {
                return serde_json::from_str(&std::fs::read_to_string(&path)?)
                    .map_err(|error| OrbitError::Execution(error.to_string()));
            }
            let settings = self.evolution_pass_settings()?;
            let now = Utc::now();
            let since = now - Duration::days(i64::from(settings.window_days));
            let rows = self.list_evolution_records(Some(EvolutionState::Open), Some(since))?;
            let records: Vec<Value> = rows["records"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|row| settings.qualifies(row))
                .take(settings.max_proposals as usize)
                .cloned()
                .map(|mut row| {
                    if let Some(events) = row["occurrences"].as_array_mut() {
                        events.retain(|event| {
                            event["at"]
                                .as_str()
                                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                                .is_some_and(|at| at >= since)
                        });
                    }
                    row["summed_cost_minutes"] = json!(cost(&row));
                    row
                })
                .collect();
            let ranking = json!({"schema_version": 1, "run_id": run_id, "workspace": self.workspace_id()?, "ran_at": now, "since": since, "settings": settings, "record_count": records.len(), "records": records});
            atomic_write_text(
                &path,
                &serde_json::to_string_pretty(&ranking)
                    .map_err(|error| OrbitError::Execution(error.to_string()))?,
            )?;
            Ok(ranking)
        })
    }

    /// Validate advisory agent drafts against this run's frozen ranking and
    /// current eligibility. Never accepts a status, grant, dispatch or build.
    pub fn apply_evolution_pass(&self, run_id: &str, drafts: Value) -> Result<Value, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let path = self.evolution_ranking_path(run_id)?;
        let ranking: Value = serde_json::from_str(&std::fs::read_to_string(path)?)
            .map_err(|error| OrbitError::Execution(error.to_string()))?;
        let settings: EvolutionPassSettings =
            serde_json::from_value(ranking["settings"].clone())
                .map_err(|error| OrbitError::Execution(error.to_string()))?;
        let drafts: Vec<EvolutionDraft> = serde_json::from_value(drafts).map_err(|error| {
            OrbitError::InvalidInput(format!("invalid evolution drafts: {error}"))
        })?;
        if drafts.len() > settings.max_proposals as usize {
            return Err(OrbitError::InvalidInput(
                "evolution drafts exceed max_proposals".into(),
            ));
        }
        let mut seen = BTreeSet::new();
        for draft in &drafts {
            if !seen.insert(&draft.record_id)
                || !ranking["records"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|row| row["id"] == draft.record_id)
                || draft.title.trim().is_empty()
                || draft.description.trim().is_empty()
                || draft.acceptance_criteria.is_empty()
                || draft
                    .acceptance_criteria
                    .iter()
                    .any(|criterion| criterion.trim().is_empty())
            {
                return Err(OrbitError::InvalidInput("each evolution draft must name one ranked record and contain a title, description and acceptance criteria".into()));
            }
        }
        let fence = self.paths().state_dir.join("evolution-pass");
        with_exclusive_file_lock(&fence, "evolution proposals", || {
            let mut proposals = Vec::new();
            for draft in &drafts {
                let current = self.show_evolution_record(&draft.record_id)?;
                if current["state"] == "proposed" {
                    continue;
                }
                let since = Utc::now() - Duration::days(i64::from(settings.window_days));
                let fresh = self.list_evolution_records(Some(EvolutionState::Open), Some(since))?;
                if !fresh["records"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|row| row["id"] == draft.record_id && settings.qualifies(row))
                {
                    continue;
                }
                let tag = format!("evolution-record:{}", draft.record_id);
                let tasks = self.list_task_metadata()?;
                let existing = tasks.iter().find(|task| {
                    task.tags.contains(&tag)
                        && !matches!(
                            task.status,
                            TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
                        )
                });
                let duplicate = draft.duplicate_task.as_deref().map(|id| {
                    tasks.iter().find(|task| task.id.as_str() == id && !matches!(task.status, TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected))
                        .ok_or_else(|| OrbitError::InvalidInput("duplicate proposal must be an existing non-terminal task in the owning workspace".into()))
                }).transpose()?;
                let (task_id, reused) = if let Some(task) = existing.or(duplicate) {
                    (task.id.to_string(), true)
                } else {
                    let task = self.add_task(TaskAddParams {
                        title: draft.title.clone(),
                        description: draft.description.clone(),
                        acceptance_criteria: draft.acceptance_criteria.clone(),
                        tags: vec!["evolution".into(), tag],
                        status: Some(TaskStatus::Proposed),
                        task_type: Some(TaskType::Feature),
                        system_created: true,
                        ..Default::default()
                    })?;
                    (task.id.to_string(), false)
                };
                let evidence = ranking["records"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|row| row["id"] == draft.record_id)
                    .cloned()
                    .ok_or_else(|| OrbitError::Execution("ranked record disappeared".into()))?;
                let evidence = json!({"schema_version": 1, "run_id": run_id, "workspace": ranking["workspace"], "since": ranking["since"], "record_id": draft.record_id, "cycles": evidence["distinct_cycles"], "occurrence_count": evidence["occurrence_count"], "mandate_count": evidence["mandate_count"], "minutes": evidence["minutes"], "slot_minutes": evidence["slot_minutes"], "summed_cost_minutes": evidence["summed_cost_minutes"], "occurrences": evidence["occurrences"]});
                let artifact_path = if reused {
                    format!("evolution/{}/evolution-evidence.json", draft.record_id)
                } else {
                    "evolution-evidence.json".into()
                };
                self.update_task_as_system(
                    &task_id,
                    TaskUpdateParams {
                        upsert_artifacts: vec![TaskArtifact {
                            path: artifact_path,
                            content: serde_json::to_vec_pretty(&evidence)
                                .map_err(|error| OrbitError::Execution(error.to_string()))?,
                            media_type: "application/json".into(),
                            created_by: None,
                        }],
                        comment: reused.then(|| {
                            format!("Evolution evidence for {}: {}", draft.record_id, evidence)
                        }),
                        ..Default::default()
                    },
                    Some(run_id.into()),
                )?;
                self.propose_evolution_record(&draft.record_id, &task_id)?;
                proposals.push(
                    json!({"record_id": draft.record_id, "task_id": task_id, "reused": reused}),
                );
            }
            Ok(json!({"count": proposals.len(), "proposals": proposals}))
        })
    }
}
