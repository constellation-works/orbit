//! The JSON shape an experiment is shown in.

use orbit_common::OrbitError;
use orbit_store::contracts::{ExperimentArmRow, ExperimentEventRow, ExperimentRecord};
use serde::Serialize;
use serde_json::Value;

/// One experiment as `show`, `list` and every write return it.
#[derive(Debug, Clone, Serialize)]
pub struct ExperimentView {
    pub id: String,
    pub name: String,
    pub status: String,
    pub mode: String,
    pub segment: u32,
    pub hypothesis: String,
    pub population: Value,
    pub primary_metric: Option<String>,
    pub guardrails: Value,
    pub min_sample: Option<u32>,
    pub max_days: Option<u32>,
    pub salt: String,
    pub concluded_arm: Option<String>,
    pub cloned_from: Option<String>,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u32,
    pub arms: Vec<ExperimentArmView>,
    /// Every event in order; empty in a list, which does not load them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<ExperimentEventView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExperimentArmView {
    pub name: String,
    pub control: bool,
    pub weight: u32,
    pub overlay: Value,
    pub overlay_digest: String,
    /// The segment the arm was retired at, when retired.
    pub retired_segment: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExperimentEventView {
    pub seq: u32,
    pub kind: String,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub segment: u32,
    pub actor: String,
    pub at: String,
    pub reason: Option<String>,
    pub detail: Value,
}

impl ExperimentView {
    pub(crate) fn from_record(record: ExperimentRecord) -> Result<Self, OrbitError> {
        let ExperimentRecord {
            experiment,
            arms,
            events,
        } = record;
        Ok(Self {
            population: stored_json("population", &experiment.population_json)?,
            guardrails: stored_json("guardrails", &experiment.guardrails_json)?,
            arms: arms.into_iter().map(arm_view).collect::<Result<_, _>>()?,
            events: events
                .into_iter()
                .map(event_view)
                .collect::<Result<_, _>>()?,
            id: experiment.id,
            name: experiment.name,
            status: experiment.status,
            mode: experiment.mode,
            segment: experiment.segment,
            hypothesis: experiment.hypothesis,
            primary_metric: experiment.primary_metric,
            min_sample: experiment.min_sample_per_arm,
            max_days: experiment.max_days,
            salt: experiment.salt,
            concluded_arm: experiment.concluded_arm,
            cloned_from: experiment.cloned_from,
            created_by: experiment.created_by,
            created_at: experiment.created_at.to_rfc3339(),
            updated_at: experiment.updated_at.to_rfc3339(),
            revision: experiment.revision,
        })
    }
}

fn arm_view(arm: ExperimentArmRow) -> Result<ExperimentArmView, OrbitError> {
    Ok(ExperimentArmView {
        overlay: stored_json("overlay", &arm.overlay_json)?,
        name: arm.name,
        control: arm.control,
        weight: arm.weight,
        overlay_digest: arm.overlay_digest,
        retired_segment: arm.retired_segment,
    })
}

fn event_view(event: ExperimentEventRow) -> Result<ExperimentEventView, OrbitError> {
    Ok(ExperimentEventView {
        detail: stored_json("event detail", &event.detail_json)?,
        seq: event.seq,
        kind: event.kind,
        from_status: event.from_status,
        to_status: event.to_status,
        segment: event.segment,
        actor: event.actor,
        at: event.at.to_rfc3339(),
        reason: event.reason,
    })
}

fn stored_json(what: &str, raw: &str) -> Result<Value, OrbitError> {
    serde_json::from_str(raw).map_err(|error| {
        OrbitError::Store(format!("stored experiment {what} is not JSON: {error}"))
    })
}
