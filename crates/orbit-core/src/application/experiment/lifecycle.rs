//! The experiment lifecycle: `draft → running ⇄ paused → concluded |
//! abandoned`. Definitions are editable only in draft; a started experiment
//! changes only through events. Every write appends one event with its
//! actor, time and reason, and is refused if another write landed since the
//! experiment was read.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::{
    ExperimentArmRow, ExperimentEventRow, ExperimentRecord, ExperimentRow,
    ExperimentStoreBackend as _, ExperimentWrite, ExperimentWriteOutcome,
};
use serde_json::{Value, json};

use super::definition::{
    ArmDefinition, ExperimentOverlay, parse_arms, parse_guardrails, parse_population,
    validate_handle, validate_metric,
};
use super::view::ExperimentView;
use crate::OrbitRuntime;

const DRAFT: &str = "draft";
const RUNNING: &str = "running";
const PAUSED: &str = "paused";
const CONCLUDED: &str = "concluded";
const ABANDONED: &str = "abandoned";

/// Definition fields for create and update; `None` leaves a field as it is
/// (create starts from an empty definition).
#[derive(Debug, Clone, Default)]
pub struct ExperimentDefinitionInput {
    pub name: Option<String>,
    pub hypothesis: Option<String>,
    pub arms: Option<Value>,
    pub population: Option<Value>,
    pub primary_metric: Option<String>,
    pub guardrails: Option<Value>,
    pub min_sample: Option<usize>,
    pub max_days: Option<usize>,
    pub mode: Option<String>,
}

/// A lifecycle event on an existing experiment.
#[derive(Debug, Clone)]
pub enum ExperimentChange {
    Start,
    Pause,
    Resume,
    /// New weights for the named arms, and arms to retire.
    Reweight {
        weights: Option<Value>,
        retire: Vec<String>,
    },
    /// The chosen arm, or none.
    Conclude {
        arm: Option<String>,
    },
    Abandon,
}

/// Who made a write and why.
#[derive(Debug, Clone)]
pub struct ExperimentActor {
    pub actor: String,
    pub reason: Option<String>,
}

impl OrbitRuntime {
    /// Define a new draft experiment named `name`.
    pub fn create_experiment(
        &self,
        name: &str,
        input: ExperimentDefinitionInput,
        by: ExperimentActor,
    ) -> Result<ExperimentView, OrbitError> {
        validate_experiment_name(name)?;
        let now = Utc::now();
        let mut experiment = ExperimentRow {
            workspace_id: self.workspace_id()?,
            id: String::new(),
            name: name.to_string(),
            hypothesis: String::new(),
            population_json: "{}".into(),
            primary_metric: None,
            guardrails_json: "{}".into(),
            min_sample_per_arm: None,
            max_days: None,
            salt: random_hex(16)?,
            mode: "shadow".into(),
            status: DRAFT.into(),
            segment: 0,
            concluded_arm: None,
            cloned_from: None,
            created_by: by.actor.clone(),
            created_at: now,
            updated_at: now,
            revision: 1,
        };
        let mut arms = Vec::new();
        apply_definition(&mut experiment, &mut arms, input)?;
        let detail = json!({
            "arms": arms.iter().map(|arm| arm.name.clone()).collect::<Vec<_>>(),
            "mode": experiment.mode,
        });
        self.insert_experiment(experiment, arms, "created", detail, by)
    }

    /// Edit a draft experiment's definition.
    pub fn update_experiment(
        &self,
        id_or_name: &str,
        input: ExperimentDefinitionInput,
        by: ExperimentActor,
    ) -> Result<ExperimentView, OrbitError> {
        let record = self.experiment_record(id_or_name)?;
        let current = &record.experiment;
        if current.status != DRAFT {
            return Err(OrbitError::InvalidInput(format!(
                "experiment `{}` is {}: its arms, overlays, population, metrics and sample rules are editable only in draft. Reweight or retire arms with reweight, or clone it into a new draft",
                current.name, current.status
            )));
        }
        let fields = input_fields(&input);
        if let Some(name) = &input.name {
            validate_experiment_name(name)?;
        }
        let mut experiment = current.clone();
        let mut arms = record.arms.clone();
        apply_definition(&mut experiment, &mut arms, input)?;
        let detail = json!({ "fields": fields });
        self.write_change(&record, experiment, Some(arms), "updated", detail, by, None)
    }

    /// Copy an experiment's definition into a new draft named `name`.
    pub fn clone_experiment(
        &self,
        id_or_name: &str,
        name: &str,
        by: ExperimentActor,
    ) -> Result<ExperimentView, OrbitError> {
        validate_experiment_name(name)?;
        let source = self.experiment_record(id_or_name)?;
        let now = Utc::now();
        let experiment = ExperimentRow {
            id: String::new(),
            name: name.to_string(),
            salt: random_hex(16)?,
            status: DRAFT.into(),
            segment: 0,
            concluded_arm: None,
            cloned_from: Some(source.experiment.id.clone()),
            created_by: by.actor.clone(),
            created_at: now,
            updated_at: now,
            revision: 1,
            ..source.experiment.clone()
        };
        let arms = source
            .arms
            .iter()
            .filter(|arm| arm.retired_segment.is_none())
            .enumerate()
            .map(|(position, arm)| ExperimentArmRow {
                position: position_u32(position),
                retired_segment: None,
                ..arm.clone()
            })
            .collect::<Vec<_>>();
        let detail = json!({
            "cloned_from": source.experiment.id,
            "arms": arms.iter().map(|arm| arm.name.clone()).collect::<Vec<_>>(),
        });
        self.insert_experiment(experiment, arms, "cloned", detail, by)
    }

    /// Apply one lifecycle event to an experiment.
    pub fn change_experiment(
        &self,
        id_or_name: &str,
        change: ExperimentChange,
        by: ExperimentActor,
    ) -> Result<ExperimentView, OrbitError> {
        let record = self.experiment_record(id_or_name)?;
        let current = &record.experiment;
        let mut experiment = current.clone();
        let mut arms = None;
        let mut running_limit = None;
        let (kind, detail) = match change {
            ExperimentChange::Start => {
                require_status(current, &[DRAFT], "start")?;
                let settings = self.experiment_settings();
                if !settings.enabled {
                    return Err(OrbitError::PolicyDenied(format!(
                        "experiment `{}` cannot start: experiments are disabled in this workspace (`experiments.enabled` is false)",
                        current.name
                    )));
                }
                let overlays = validate_startable(&record, &settings.config_keys)?;
                self.validate_experiment_overlays(&current.name, &overlays)?;
                self.validate_experiment_crews(&current.name, &overlays)?;
                running_limit = Some(settings.max_running);
                experiment.status = RUNNING.into();
                experiment.segment = 1;
                (
                    "started",
                    json!({ "segment": 1, "weights": weights(&record.arms) }),
                )
            }
            ExperimentChange::Pause => {
                require_status(current, &[RUNNING], "pause")?;
                experiment.status = PAUSED.into();
                ("paused", json!({}))
            }
            ExperimentChange::Resume => {
                require_status(current, &[PAUSED], "resume")?;
                running_limit = Some(self.experiment_settings().max_running);
                experiment.status = RUNNING.into();
                ("resumed", json!({}))
            }
            ExperimentChange::Reweight {
                weights: new,
                retire,
            } => {
                require_status(current, &[RUNNING, PAUSED], "reweight")?;
                let (next_arms, detail) = reweight_arms(&record, new, &retire)?;
                experiment.segment = current.segment + 1;
                arms = Some(next_arms);
                let kind = if detail["weights_changed"] == Value::Bool(true) {
                    "reweighted"
                } else {
                    "arm_retired"
                };
                (kind, detail)
            }
            ExperimentChange::Conclude { arm } => {
                require_status(current, &[RUNNING, PAUSED], "conclude")?;
                if let Some(chosen) = &arm
                    && !record.arms.iter().any(|known| &known.name == chosen)
                {
                    return Err(OrbitError::InvalidInput(format!(
                        "experiment `{}` has no arm `{chosen}`; its arms are {}",
                        current.name,
                        arm_names(&record.arms)
                    )));
                }
                // Snapshot the readout before the status flip, and file any
                // promotion task before the event write. The event is
                // append-only, so a failed write retries onto the same task.
                let readout = self.experiment_readout(&current.id, None)?;
                let readout_json = serde_json::to_value(&readout).map_err(|error| {
                    OrbitError::Execution(format!("experiment readout is not JSON: {error}"))
                })?;
                let promotion_task = match &arm {
                    Some(chosen) => {
                        Some(self.file_experiment_promotion(&record, chosen, &readout)?)
                    }
                    None => None,
                };
                experiment.status = CONCLUDED.into();
                experiment.concluded_arm = arm.clone();
                let mut detail = json!({ "arm": arm, "readout": readout_json });
                if let Some(task_id) = promotion_task {
                    detail["promotion_task"] = json!(task_id);
                }
                ("concluded", detail)
            }
            ExperimentChange::Abandon => {
                require_status(current, &[DRAFT, RUNNING, PAUSED], "abandon")?;
                experiment.status = ABANDONED.into();
                ("abandoned", json!({}))
            }
        };
        self.write_change(&record, experiment, arms, kind, detail, by, running_limit)
    }

    /// The experiment whose id, or else whose name, is `id_or_name`.
    pub fn experiment_view(&self, id_or_name: &str) -> Result<ExperimentView, OrbitError> {
        ExperimentView::from_record(self.experiment_record(id_or_name)?)
    }

    /// The workspace's experiments, oldest first.
    pub fn experiment_views(
        &self,
        status: Option<&str>,
    ) -> Result<Vec<ExperimentView>, OrbitError> {
        if let Some(status) = status
            && ![DRAFT, RUNNING, PAUSED, CONCLUDED, ABANDONED].contains(&status)
        {
            return Err(OrbitError::InvalidInput(format!(
                "unknown experiment status `{status}`; expected draft, running, paused, concluded or abandoned"
            )));
        }
        self.sqlite_store()?
            .experiment_records(&self.workspace_id()?, status)?
            .into_iter()
            .map(ExperimentView::from_record)
            .collect()
    }

    fn experiment_record(&self, id_or_name: &str) -> Result<ExperimentRecord, OrbitError> {
        self.sqlite_store()?
            .experiment_record(&self.workspace_id()?, id_or_name)?
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("no experiment `{id_or_name}` in this workspace"))
            })
    }

    fn insert_experiment(
        &self,
        mut experiment: ExperimentRow,
        arms: Vec<ExperimentArmRow>,
        kind: &str,
        detail: Value,
        by: ExperimentActor,
    ) -> Result<ExperimentView, OrbitError> {
        // A fresh random id; a collision reads as stale and draws again.
        for _ in 0..3 {
            experiment.id = format!("exp-{}", random_hex(8)?);
            let event = event_row(&experiment, None, kind, detail.clone(), &by);
            let write = ExperimentWrite {
                experiment: experiment.clone(),
                arms: Some(arms.clone()),
                event,
                expected_revision: None,
                running_limit: None,
            };
            match self.sqlite_store()?.write_experiment(&write)? {
                ExperimentWriteOutcome::Written => return self.experiment_view(&experiment.id),
                ExperimentWriteOutcome::NameTaken => return Err(name_taken(&experiment.name)),
                ExperimentWriteOutcome::Stale
                | ExperimentWriteOutcome::RunningLimitReached { .. } => {}
            }
        }
        Err(OrbitError::Execution(
            "could not allocate an unused experiment id".into(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn write_change(
        &self,
        record: &ExperimentRecord,
        mut experiment: ExperimentRow,
        arms: Option<Vec<ExperimentArmRow>>,
        kind: &str,
        detail: Value,
        by: ExperimentActor,
        running_limit: Option<u32>,
    ) -> Result<ExperimentView, OrbitError> {
        let current = &record.experiment;
        experiment.revision = current.revision + 1;
        experiment.updated_at = Utc::now();
        let event = event_row(&experiment, Some(&current.status), kind, detail, &by);
        let write = ExperimentWrite {
            experiment,
            arms,
            event,
            expected_revision: Some(current.revision),
            running_limit,
        };
        match self.sqlite_store()?.write_experiment(&write)? {
            ExperimentWriteOutcome::Written => self.experiment_view(&current.id),
            ExperimentWriteOutcome::NameTaken => Err(name_taken(&write.experiment.name)),
            ExperimentWriteOutcome::Stale => Err(OrbitError::InvalidInput(format!(
                "experiment `{}` changed while this write was prepared; show it and retry",
                current.name
            ))),
            ExperimentWriteOutcome::RunningLimitReached { running } => {
                Err(OrbitError::PolicyDenied(format!(
                    "experiment `{}` cannot run: {running} experiment(s) already running and `experiments.max_running` is {}. Conclude, pause or abandon one first",
                    current.name,
                    running_limit.unwrap_or(running),
                )))
            }
        }
    }
}

fn validate_experiment_name(name: &str) -> Result<(), OrbitError> {
    validate_handle("experiment name", name)?;
    if name.starts_with("exp-") {
        return Err(OrbitError::InvalidInput(format!(
            "experiment name `{name}` must not start with `exp-`, which experiment ids use"
        )));
    }
    Ok(())
}

fn name_taken(name: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "an experiment named `{name}` already exists in this workspace"
    ))
}

fn require_status(
    experiment: &ExperimentRow,
    allowed: &[&str],
    action: &str,
) -> Result<(), OrbitError> {
    if allowed.contains(&experiment.status.as_str()) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "cannot {action} experiment `{}`: it is {}, and only a {} experiment can {action}",
        experiment.name,
        experiment.status,
        allowed.join(" or "),
    )))
}

/// The input fields an update sets, in declaration order.
fn input_fields(input: &ExperimentDefinitionInput) -> Vec<&'static str> {
    [
        ("name", input.name.is_some()),
        ("hypothesis", input.hypothesis.is_some()),
        ("arms", input.arms.is_some()),
        ("population", input.population.is_some()),
        ("primary_metric", input.primary_metric.is_some()),
        ("guardrails", input.guardrails.is_some()),
        ("min_sample", input.min_sample.is_some()),
        ("max_days", input.max_days.is_some()),
        ("mode", input.mode.is_some()),
    ]
    .into_iter()
    .filter_map(|(field, set)| set.then_some(field))
    .collect()
}

fn apply_definition(
    experiment: &mut ExperimentRow,
    arms: &mut Vec<ExperimentArmRow>,
    input: ExperimentDefinitionInput,
) -> Result<(), OrbitError> {
    if let Some(name) = input.name {
        experiment.name = name;
    }
    if let Some(hypothesis) = input.hypothesis {
        experiment.hypothesis = hypothesis;
    }
    if let Some(value) = input.arms {
        *arms = parse_arms(value)?
            .into_iter()
            .enumerate()
            .map(|(position, arm)| arm_row(position, arm))
            .collect();
    }
    if let Some(value) = input.population {
        experiment.population_json = parse_population(value)?;
    }
    if let Some(metric) = input.primary_metric {
        validate_metric("primary_metric", &metric)?;
        experiment.primary_metric = Some(metric);
    }
    if let Some(value) = input.guardrails {
        experiment.guardrails_json = parse_guardrails(value)?;
    }
    if let Some(min_sample) = input.min_sample {
        experiment.min_sample_per_arm = Some(positive("min_sample", min_sample)?);
    }
    if let Some(max_days) = input.max_days {
        experiment.max_days = Some(positive("max_days", max_days)?);
    }
    if let Some(mode) = input.mode {
        if !matches!(mode.as_str(), "shadow" | "live") {
            return Err(OrbitError::InvalidInput(format!(
                "unknown experiment mode `{mode}`; expected shadow or live"
            )));
        }
        experiment.mode = mode;
    }
    Ok(())
}

fn positive(field: &str, value: usize) -> Result<u32, OrbitError> {
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| OrbitError::InvalidInput(format!("`{field}` must be a positive integer")))
}

fn arm_row(position: usize, arm: ArmDefinition) -> ExperimentArmRow {
    ExperimentArmRow {
        name: arm.name,
        position: position_u32(position),
        control: arm.control,
        weight: arm.weight,
        overlay_json: arm.overlay_json,
        overlay_digest: arm.overlay_digest,
        retired_segment: None,
    }
}

fn position_u32(position: usize) -> u32 {
    u32::try_from(position).unwrap_or(u32::MAX)
}

/// Whether a draft is complete enough to start, short of the checks that
/// read the repository or the tool registry. Returns each arm's overlay.
fn validate_startable(
    record: &ExperimentRecord,
    config_keys: &[String],
) -> Result<Vec<(String, ExperimentOverlay)>, OrbitError> {
    let name = &record.experiment.name;
    let refuse = |detail: String| {
        OrbitError::InvalidInput(format!("experiment `{name}` cannot start: {detail}"))
    };
    if record.arms.len() < 2 {
        return Err(refuse(format!(
            "it needs a control and at least one treatment arm, and has {}",
            record.arms.len()
        )));
    }
    let controls = record.arms.iter().filter(|arm| arm.control).count();
    if controls != 1 {
        return Err(refuse(format!(
            "exactly one arm must be the control, and {controls} are"
        )));
    }
    if let Some(arm) = record.arms.iter().find(|arm| arm.weight == 0) {
        return Err(refuse(format!("arm `{}` has weight 0", arm.name)));
    }
    if record.experiment.primary_metric.is_none() {
        return Err(refuse("it has no primary metric".into()));
    }
    let mut overlays = Vec::with_capacity(record.arms.len());
    for arm in &record.arms {
        validate_handle("arm name", &arm.name).map_err(|error| refuse(error.to_string()))?;
        let value = serde_json::from_str(&arm.overlay_json).map_err(|error| {
            refuse(format!(
                "arm `{}` has an unreadable overlay: {error}",
                arm.name
            ))
        })?;
        let overlay = ExperimentOverlay::parse(&arm.name, value)
            .map_err(|error| refuse(error.to_string()))?;
        if let Some(config) = &overlay.config {
            for (key, value) in &config.values {
                orbit_config::ExperimentConfigKey::from_config_key(key)
                    .and_then(|key| key.validate_experiment_value(value))
                    .map_err(|error| refuse(format!("arm `{}`: {error}", arm.name)))?;
            }
        }
        if let Some(key) = overlay
            .config_keys()
            .find(|key| !config_keys.iter().any(|allowed| allowed == key))
        {
            return Err(refuse(format!(
                "arm `{}` overrides config key `{key}`, which `experiments.config_keys` does not allow",
                arm.name
            )));
        }
        overlays.push((arm.name.clone(), overlay));
    }
    Ok(overlays)
}

fn reweight_arms(
    record: &ExperimentRecord,
    new_weights: Option<Value>,
    retire: &[String],
) -> Result<(Vec<ExperimentArmRow>, Value), OrbitError> {
    let name = &record.experiment.name;
    let next_segment = record.experiment.segment + 1;
    let active = |arm: &ExperimentArmRow| arm.retired_segment.is_none();
    let new_weights: BTreeMap<String, u32> = match new_weights {
        Some(value) => serde_json::from_value(value).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "`weights` must map arm names to positive integers: {error}"
            ))
        })?,
        None => BTreeMap::new(),
    };
    let mut arms = record.arms.clone();
    for (arm, weight) in &new_weights {
        let Some(row) = arms.iter_mut().find(|row| &row.name == arm && active(row)) else {
            return Err(OrbitError::InvalidInput(format!(
                "experiment `{name}` has no active arm `{arm}` to reweight; its active arms are {}",
                arm_names(record.arms.iter().filter(|arm| active(arm)))
            )));
        };
        if *weight == 0 {
            return Err(OrbitError::InvalidInput(format!(
                "arm `{arm}` weight must be greater than 0; retire it instead"
            )));
        }
        row.weight = *weight;
    }
    for arm in retire {
        let Some(row) = arms.iter_mut().find(|row| &row.name == arm && active(row)) else {
            return Err(OrbitError::InvalidInput(format!(
                "experiment `{name}` has no active arm `{arm}` to retire"
            )));
        };
        if row.control {
            return Err(OrbitError::InvalidInput(format!(
                "arm `{arm}` is the control and cannot be retired; conclude or abandon the experiment instead"
            )));
        }
        if new_weights.contains_key(arm) {
            return Err(OrbitError::InvalidInput(format!(
                "arm `{arm}` is both reweighted and retired"
            )));
        }
        row.retired_segment = Some(next_segment);
    }
    if !arms.iter().any(|arm| active(arm) && !arm.control) {
        return Err(OrbitError::InvalidInput(format!(
            "experiment `{name}` must keep at least one active treatment arm; conclude or abandon it instead"
        )));
    }
    let before = weights(&record.arms);
    let after = weights(&arms);
    let mut before_without_retired = before.clone();
    for arm in retire {
        before_without_retired.remove(arm);
    }
    // Removing retired arms changes the active set, not the weights of the
    // arms that remain active. Keep event classification about weight edits.
    let weights_changed = before_without_retired != after;
    if !weights_changed && retire.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "the reweight changes nothing on experiment `{name}`"
        )));
    }
    let detail = json!({
        "segment_from": record.experiment.segment,
        "segment_to": next_segment,
        "weights_before": before,
        "weights_after": after,
        "weights_changed": weights_changed,
        "retired": retire,
    });
    Ok((arms, detail))
}

/// Active arms' weights by name.
fn weights(arms: &[ExperimentArmRow]) -> BTreeMap<String, u32> {
    arms.iter()
        .filter(|arm| arm.retired_segment.is_none())
        .map(|arm| (arm.name.clone(), arm.weight))
        .collect()
}

fn arm_names<'a>(arms: impl IntoIterator<Item = &'a ExperimentArmRow>) -> String {
    let names = arms
        .into_iter()
        .map(|arm| format!("`{}`", arm.name))
        .collect::<Vec<_>>();
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

fn event_row(
    experiment: &ExperimentRow,
    from_status: Option<&str>,
    kind: &str,
    detail: Value,
    by: &ExperimentActor,
) -> ExperimentEventRow {
    ExperimentEventRow {
        seq: experiment.revision,
        kind: kind.into(),
        from_status: from_status.map(str::to_string),
        to_status: Some(experiment.status.clone()),
        segment: experiment.segment,
        actor: by.actor.clone(),
        at: experiment.updated_at,
        reason: by.reason.clone(),
        detail_json: detail.to_string(),
    }
}

fn random_hex(chars: usize) -> Result<String, OrbitError> {
    let mut bytes = vec![0_u8; chars.div_ceil(2)];
    getrandom::fill(&mut bytes)
        .map_err(|error| OrbitError::Execution(format!("draw experiment randomness: {error}")))?;
    let mut hex = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    hex.truncate(chars);
    Ok(hex)
}
