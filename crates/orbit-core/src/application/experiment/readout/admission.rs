//! Whether the latest admitted delivery actually received its enrolled arm.
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{ClaimInspection, JobRunQuery};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct ClaimTreatment {
    pub(super) at: DateTime<Utc>,
    delivered: Option<bool>,
    pub(super) captured_memory: Option<super::model::TaskMemoryDelivery>,
}

impl OrbitRuntime {
    /// Owner-issued captures of each of `tasks`' latest claim, joined to the
    /// actual crew reported at settlement.
    ///
    /// One claim inspection and one admission boundary serve the whole
    /// readout, and only the latest claim of an enrolled task has its receipt
    /// read: a receipt lookup per claim, each re-inspecting every claim, made
    /// this quadratic in the workspace's claims [ORB-22445].
    pub(super) fn latest_claim_experiment_treatments(
        &self,
        experiment: &str,
        tasks: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, ClaimTreatment>, OrbitError> {
        let mut claims: BTreeMap<String, (DateTime<Utc>, ClaimInspection)> = BTreeMap::new();
        for inspection in self.inspect_execution_claims()? {
            if !tasks.contains(&inspection.claim.task_id) {
                continue;
            }
            let Ok(at) = inspection.created_at.parse::<DateTime<Utc>>() else {
                continue;
            };
            if claims
                .get(&inspection.claim.task_id)
                .is_some_and(|(seen, _)| *seen >= at)
            {
                continue;
            }
            claims.insert(inspection.claim.task_id.clone(), (at, inspection));
        }
        if claims.is_empty() {
            return Ok(BTreeMap::new());
        }
        // An unreadable receipt leaves the claim's treatment unknown, as it
        // did when each receipt opened its own boundary.
        let boundary = self.admission_boundary().ok();
        let mut latest: BTreeMap<String, ClaimTreatment> = BTreeMap::new();
        for (task, (at, inspection)) in claims {
            let capture = boundary
                .as_ref()
                .and_then(|boundary| Self::admitted_claim_receipt(boundary, &inspection.claim).ok())
                .and_then(|receipt| receipt.experiments);
            let entry = capture.as_ref().and_then(|capture| {
                capture
                    .assignments
                    .iter()
                    .find(|entry| entry.experiment == experiment)
            });
            let captured_memory = entry
                .and_then(|entry| entry.memory.as_ref())
                .filter(|memory| memory.enabled)
                .and_then(|memory| memory.deliveries.first())
                .map(|delivery| super::model::TaskMemoryDelivery {
                    week: delivery
                        .revised_at
                        .as_deref()
                        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                        .map(|at| at.format("%G-W%V").to_string()),
                    digest: delivery.digest.clone(),
                    model: delivery.model.clone(),
                    model_revision: delivery.block.revision.model,
                    global_revision: delivery.block.revision.global,
                    renderer_version: delivery.renderer_version,
                });
            let delivered = match entry {
                Some(entry) if !entry.live => Some(false),
                Some(entry) => {
                    let overlay: Value =
                        serde_json::from_str(&entry.overlay_json).map_err(|error| {
                            OrbitError::Store(format!("invalid experiment overlay: {error}"))
                        })?;
                    if let Some(pool) = overlay["crew"]["pool"].as_array() {
                        inspection
                            .settlement
                            .as_ref()
                            .and_then(|settlement| settlement.crew.as_deref())
                            .map(|crew| {
                                pool.iter()
                                    .any(|member| member["crew"].as_str() == Some(crew))
                            })
                    } else {
                        Some(true)
                    }
                }
                None => None,
            };
            latest.insert(
                task,
                ClaimTreatment {
                    at,
                    delivered,
                    captured_memory,
                },
            );
        }
        Ok(latest)
    }

    /// Read execution evidence rather than inferring treatment from a task's
    /// current crew, which an operator may have changed after admission.
    pub(super) fn admitted_experiment_treatment(
        &self,
        task: &str,
        experiment: &str,
        claim: Option<&ClaimTreatment>,
    ) -> Result<Option<bool>, OrbitError> {
        let mut runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            task_id: Some(task.into()),
            include_steps: false,
            ..Default::default()
        })?;
        runs.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.run_id.cmp(&left.run_id))
        });
        for run in runs {
            if let Some(claim) = claim.filter(|claim| claim.at > run.created_at) {
                return Ok(claim.delivered);
            }
            // An unreadable checkpoint leaves this run's state evidence
            // unavailable rather than failing the readout; the run input
            // below is still consulted.
            let state = self.read_run_state(&run.run_id).ok().flatten();
            let selection = state
                .as_ref()
                .and_then(|state| state.crew_selection.as_ref())
                .or_else(|| {
                    run.input
                        .as_ref()
                        .and_then(|input| input.get("crew_selection"))
                });
            let capture = run
                .input
                .as_ref()
                .and_then(|input| input.get("experiment_capture"))
                .or_else(|| {
                    state
                        .as_ref()
                        .and_then(|state| state.initial_input.get("experiment_capture"))
                });
            let entry = capture
                .and_then(|capture| capture.get("assignments"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|entry| entry["experiment"].as_str() == Some(experiment));
            let Some(entry) = entry else {
                continue;
            };
            if entry["live"] != true {
                return Ok(Some(false));
            }
            let crew_overlay = entry["overlay_json"]
                .as_str()
                .map(serde_json::from_str::<Value>)
                .transpose()
                .map_err(|error| OrbitError::Store(format!("invalid experiment overlay: {error}")))?
                .is_some_and(|overlay| !overlay["crew"].is_null());
            if !crew_overlay {
                return Ok(Some(true));
            }
            let expected = format!(
                "experiment:{experiment}:{}",
                entry["arm"].as_str().unwrap_or_default()
            );
            return Ok(Some(selection.is_some_and(|selection| {
                selection["source"]
                    .as_str()
                    .is_some_and(|source| source.split(';').next() == Some(expected.as_str()))
                    && selection["experiment_overlay_digest"] == entry["digest"]
                    && selection["deviation"].is_null()
            })));
        }
        // Old runs without admission evidence cannot prove a crew treatment.
        Ok(claim.and_then(|claim| claim.delivered))
    }
}
