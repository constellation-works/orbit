//! Read projections of condition state for explain, `routine list`,
//! `auto-task list` and the dashboard: ownership, arm states and holds, the
//! stall, recent fires with their upstream chain, and `self` (§9, §11).

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use orbit_automation::condition::{ConditionCarrier, self_facts};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::{
    ConditionArm, ConditionFire, ConditionPass, ConditionSkip, ConditionStall, ConditionState,
    ConditionSubject,
};
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule, RoutineDefinition};
use serde::Serialize;

use super::{ConditionDefinition, condition_consumer, condition_refusal};
use crate::OrbitRuntime;

/// Most fires and subjects one status lists.
const LISTED: usize = 20;

/// One fire as surfaces show it.
#[derive(Debug, Clone, Serialize)]
pub struct ConditionFireView {
    pub fire_id: String,
    pub subject: String,
    pub revision: String,
    pub depth: u32,
    pub state: &'static str,
    pub observed_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_fire_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Upstream fires, nearest first, back to one with no origin.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<ConditionChainLink>,
}

impl ConditionFireView {
    fn of(fire: &ConditionFire) -> Self {
        Self {
            fire_id: fire.fire_id.clone(),
            subject: fire.subject.clone(),
            revision: fire.revision.clone(),
            depth: fire.depth,
            state: fire.state.as_str(),
            observed_at: fire.observed_at,
            action_id: fire.action_id.clone(),
            acknowledged_at: fire.acknowledged_at,
            parent_fire_id: fire.parent_fire_id.clone(),
            detail: fire.detail.clone(),
            chain: Vec::new(),
        }
    }
}

/// One upstream fire.
#[derive(Debug, Clone, Serialize)]
pub struct ConditionChainLink {
    pub fire_id: String,
    /// `routine:<name>` or `auto-task:<name>`; absent when the record is gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// The fire record is past retention or was forgotten.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub missing: bool,
}

/// One tracked subject.
#[derive(Debug, Clone, Serialize)]
pub struct ConditionSubjectView {
    pub key: String,
    pub arm: &'static str,
    pub arm_sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub true_since: Option<DateTime<Utc>>,
    /// Minutes of `for_minutes` still to hold before a fire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold_remaining_minutes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub baselined: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<DateTime<Utc>>,
}

impl ConditionSubjectView {
    pub(crate) fn from_subject(
        key: &str,
        subject: &ConditionSubject,
        hold: Duration,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            key: key.to_string(),
            arm: subject.arm.as_str(),
            arm_sequence: subject.arm_sequence,
            true_since: subject.true_since,
            hold_remaining_minutes: subject.true_since.map(|since| {
                (hold - now.signed_duration_since(since))
                    .num_minutes()
                    .max(0)
            }),
            deferred: subject.deferred.clone(),
            baselined: subject.baselined,
            last_fired_at: subject.last_fired_at,
        }
    }
}

/// `self` as the next pass reads it.
#[derive(Debug, Clone, Serialize)]
pub struct ConditionSelfView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fired: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_minted: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_minted_task: Option<String>,
    pub fires_24h: i64,
}

/// A condition definition's durable state, projected.
#[derive(Debug, Clone, Serialize)]
pub struct ConditionStatus {
    pub consumer: String,
    /// `owned_here`, or why this checkout never fires it.
    pub ownership: &'static str,
    /// Whether any pass recorded state yet.
    pub tracked: bool,
    /// Whether the latest pass saw the definition enabled.
    pub enabled_seen: bool,
    /// Whether the stored edge memory belongs to the current definition; a
    /// changed definition baselines on its next pass.
    pub epoch_current: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stall: Option<ConditionStall>,
    pub armed: usize,
    pub fired: usize,
    pub holding: usize,
    pub waiting: usize,
    pub pending_intents: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_pass: Option<ConditionPass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fire: Option<ConditionFireView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_skip: Option<ConditionSkip>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub skip_counts: BTreeMap<String, u64>,
    #[serde(rename = "self")]
    pub self_view: ConditionSelfView,
    /// Newest first, with each fire's upstream chain.
    pub recent_fires: Vec<ConditionFireView>,
    /// Holding, waiting and fired subjects first.
    pub subjects: Vec<ConditionSubjectView>,
}

/// The status of one condition definition, read-only.
pub(crate) fn condition_status(
    runtime: &OrbitRuntime,
    definition: &ConditionDefinition<'_>,
    now: DateTime<Utc>,
) -> Result<ConditionStatus, OrbitError> {
    let consumer = condition_consumer(runtime, definition.carrier, definition.name)?;
    let state = runtime.automation_store()?.condition_state(&consumer)?;
    let epoch = definition.epoch()?;
    let ownership = condition_refusal(runtime).unwrap_or("owned_here");
    let Some(state) = state else {
        return Ok(ConditionStatus {
            consumer,
            ownership,
            tracked: false,
            enabled_seen: false,
            epoch_current: false,
            stall: None,
            armed: 0,
            fired: 0,
            holding: 0,
            waiting: 0,
            pending_intents: 0,
            last_pass: None,
            last_fire: None,
            last_skip: None,
            skip_counts: BTreeMap::new(),
            self_view: self_view(None, now),
            recent_fires: Vec::new(),
            subjects: Vec::new(),
        });
    };
    let hold = Duration::minutes(i64::try_from(definition.condition.for_minutes).unwrap_or(0));
    let index = fire_index(runtime)?;
    let mut subjects = state
        .subjects
        .iter()
        .map(|(key, subject)| ConditionSubjectView::from_subject(key, subject, hold, now))
        .collect::<Vec<_>>();
    subjects.sort_by_key(|subject| {
        (
            subject.true_since.is_none(),
            subject.deferred.is_none(),
            subject.arm != "fired" || subject.baselined,
            subject.key.clone(),
        )
    });
    let recent_fires = state
        .fires
        .iter()
        .rev()
        .take(LISTED)
        .map(|fire| {
            let mut view = ConditionFireView::of(fire);
            view.chain = chain_from(&index, fire);
            view
        })
        .collect::<Vec<_>>();
    Ok(ConditionStatus {
        consumer,
        ownership,
        tracked: true,
        enabled_seen: state.enabled,
        epoch_current: state.epoch == epoch,
        stall: state.stall.clone(),
        armed: count(&state, hold, |arm, holding| {
            arm == ConditionArm::Armed && !holding
        }),
        fired: count(&state, hold, |arm, _| arm == ConditionArm::Fired),
        holding: count(&state, hold, |arm, holding| {
            arm == ConditionArm::Armed && holding
        }),
        waiting: state
            .subjects
            .values()
            .filter(|subject| subject.deferred.is_some())
            .count(),
        pending_intents: state
            .fires
            .iter()
            .filter(|fire| fire.state.as_str() == "intent")
            .count(),
        last_pass: state.last_pass.at.map(|_| state.last_pass.clone()),
        last_fire: recent_fires.first().cloned(),
        last_skip: state.last_skip.clone(),
        skip_counts: state.skip_counts.clone(),
        self_view: self_view(Some(&state), now),
        recent_fires,
        subjects: subjects.into_iter().take(LISTED).collect(),
    })
}

/// The compact projection list surfaces carry: ownership, counts, the
/// stall, the last pass, and the last fire and skip with its reason.
#[derive(Debug, Clone, Serialize)]
pub struct ConditionListing {
    pub ownership: &'static str,
    pub tracked: bool,
    pub armed: usize,
    pub holding: usize,
    pub waiting: usize,
    pub fired: usize,
    pub pending_intents: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stall: Option<ConditionStall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_pass: Option<ConditionPass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fire: Option<ConditionFireView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_skip: Option<ConditionSkip>,
}

impl From<ConditionStatus> for ConditionListing {
    fn from(status: ConditionStatus) -> Self {
        Self {
            ownership: status.ownership,
            tracked: status.tracked,
            armed: status.armed,
            holding: status.holding,
            waiting: status.waiting,
            fired: status.fired,
            pending_intents: status.pending_intents,
            stall: status.stall,
            last_pass: status.last_pass,
            last_fire: status.last_fire,
            last_skip: status.last_skip,
        }
    }
}

impl OrbitRuntime {
    /// The list projection of a routine's condition, `None` for any other
    /// trigger.
    pub fn routine_condition_listing(
        &self,
        definition: &RoutineDefinition,
    ) -> Result<Option<ConditionListing>, OrbitError> {
        ConditionDefinition::of_routine(definition)
            .map(|condition| condition_status(self, &condition, Utc::now()).map(Into::into))
            .transpose()
    }

    /// The list projection of an auto-task's condition, `None` for any
    /// other schedule.
    pub fn auto_task_condition_listing(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<Option<ConditionListing>, OrbitError> {
        ConditionDefinition::of_auto_task(definition)
            .map(|condition| condition_status(self, &condition, Utc::now()).map(Into::into))
            .transpose()
    }
}

/// Count subjects by arm and whether they are serving a `for_minutes` hold.
/// A true subject without a hold is an edge a stall or cap is keeping back,
/// not a hold.
fn count(
    state: &ConditionState,
    hold: Duration,
    keep: impl Fn(ConditionArm, bool) -> bool,
) -> usize {
    state
        .subjects
        .values()
        .filter(|subject| {
            let holding = hold > Duration::zero()
                && subject.true_since.is_some()
                && subject.deferred.is_none();
            keep(subject.arm, holding)
        })
        .count()
}

fn self_view(state: Option<&ConditionState>, now: DateTime<Utc>) -> ConditionSelfView {
    let facts = self_facts(state, now);
    ConditionSelfView {
        last_fired: facts.last_fired,
        last_minted: facts.last_minted,
        last_minted_task: facts.last_minted_task,
        fires_24h: facts.fires_24h,
    }
}

/// Every fire this workspace's condition consumers record, by ID.
type FireIndex = HashMap<String, (String, ConditionFire)>;

fn fire_index(runtime: &OrbitRuntime) -> Result<FireIndex, OrbitError> {
    let Some(machine) = runtime.automation_machine_identity() else {
        return Ok(HashMap::new());
    };
    let prefix = format!("{machine}/{}/", runtime.workspace_id()?);
    let store = runtime.automation_store()?;
    let mut index = HashMap::new();
    let mut after: Option<String> = None;
    loop {
        let page = store.condition_states_page(&prefix, after.as_deref(), 100)?;
        let Some(last) = page.last() else {
            break;
        };
        after = Some(last.consumer.clone());
        for state in page {
            let trigger = trigger_of(&state.consumer);
            for fire in state.fires {
                index.insert(fire.fire_id.clone(), (trigger.clone(), fire));
            }
        }
    }
    Ok(index)
}

/// `machine/workspace/kind/name` → `kind:name`.
fn trigger_of(consumer: &str) -> String {
    let mut parts = consumer.rsplitn(3, '/');
    let name = parts.next().unwrap_or_default();
    let kind = parts.next().unwrap_or_default();
    format!("{kind}:{name}")
}

fn chain_from(index: &FireIndex, fire: &ConditionFire) -> Vec<ConditionChainLink> {
    let mut chain = Vec::new();
    let mut next = fire.parent_fire_id.clone();
    // Depth is bounded at five; a few more links show a refused loop.
    while let Some(fire_id) = next.take() {
        if chain.len() > 8
            || chain
                .iter()
                .any(|link: &ConditionChainLink| link.fire_id == fire_id)
        {
            break;
        }
        match index.get(&fire_id) {
            Some((trigger, parent)) => {
                next = parent.parent_fire_id.clone();
                chain.push(ConditionChainLink {
                    fire_id,
                    trigger: Some(trigger.clone()),
                    subject: Some(parent.subject.clone()),
                    depth: Some(parent.depth),
                    action_id: parent.action_id.clone(),
                    missing: false,
                });
            }
            None => chain.push(ConditionChainLink {
                fire_id,
                trigger: None,
                subject: None,
                depth: None,
                action_id: None,
                missing: true,
            }),
        }
    }
    chain
}

/// A routine or auto-task by name that carries a condition.
pub(crate) struct FoundCondition {
    pub(crate) carrier: ConditionCarrier,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) enabled: bool,
    pub(crate) definition: FoundDefinition,
}

pub(crate) enum FoundDefinition {
    Routine(Box<RoutineDefinition>),
    AutoTask(Box<AutoTaskDefinition>),
}

impl FoundCondition {
    pub(crate) fn condition_definition(&self) -> Result<ConditionDefinition<'_>, OrbitError> {
        match &self.definition {
            FoundDefinition::Routine(definition) => ConditionDefinition::of_routine(definition),
            FoundDefinition::AutoTask(definition) => ConditionDefinition::of_auto_task(definition),
        }
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "{} '{}' has no condition",
                self.carrier.kind(),
                self.name
            ))
        })
    }
}

/// A routine or auto-task by that name, with or without a condition.
struct LoadedCarrier {
    carrier: ConditionCarrier,
    name: String,
    path: String,
    enabled: bool,
    definition: FoundDefinition,
    has_condition: bool,
}

impl OrbitRuntime {
    /// Find the named definition among this checkout's routines and
    /// auto-tasks and require that it carries a condition. `selector` is a
    /// name, or `routine:<name>` / `auto-task:<name>`.
    pub(crate) fn condition_definition_for(
        &self,
        selector: &str,
    ) -> Result<FoundCondition, OrbitError> {
        let selector = selector.trim();
        let (kinds, name) = match selector.split_once(':') {
            Some(("routine", name)) => (&[ConditionCarrier::Routine][..], name),
            Some(("auto-task", name)) => (&[ConditionCarrier::AutoTask][..], name),
            _ => (
                &[ConditionCarrier::Routine, ConditionCarrier::AutoTask][..],
                selector,
            ),
        };
        if name.is_empty() {
            return Err(OrbitError::InvalidInput(
                "definition must name a routine or auto-task".into(),
            ));
        }
        let mut found = Vec::new();
        for carrier in kinds {
            if let Some(definition) = self.load_condition_carrier(*carrier, name)? {
                found.push(definition);
            }
        }
        match found.len() {
            0 => Err(OrbitError::InvalidInput(format!(
                "no {} named '{name}' in this workspace",
                match kinds {
                    [only] => only.kind(),
                    _ => "routine or auto-task",
                }
            ))),
            1 => {
                let carrier = found.remove(0);
                if !carrier.has_condition {
                    return Err(OrbitError::InvalidInput(format!(
                        "{} '{}' has no condition; this covers {} triggers only",
                        carrier.carrier.kind(),
                        carrier.name,
                        carrier.carrier.clause()
                    )));
                }
                Ok(FoundCondition {
                    carrier: carrier.carrier,
                    name: carrier.name,
                    path: carrier.path,
                    enabled: carrier.enabled,
                    definition: carrier.definition,
                })
            }
            _ => Err(OrbitError::InvalidInput(format!(
                "both a routine and an auto-task are named '{name}'; pass routine:{name} or \
                 auto-task:{name}"
            ))),
        }
    }

    fn load_condition_carrier(
        &self,
        carrier: ConditionCarrier,
        name: &str,
    ) -> Result<Option<LoadedCarrier>, OrbitError> {
        match carrier {
            ConditionCarrier::Routine => {
                let Some((path, loaded)) =
                    orbit_automation::routines::loader::find_routine_definition(
                        &self.shared_root(),
                        name,
                    )
                else {
                    return Ok(None);
                };
                let definition = loaded.map_err(|error| {
                    OrbitError::InvalidInput(format!(
                        "routine '{name}' does not load ({}): {error}",
                        path.display()
                    ))
                })?;
                Ok(Some(LoadedCarrier {
                    carrier,
                    name: definition.name.clone(),
                    path: path.display().to_string(),
                    enabled: definition.enabled,
                    has_condition: definition.trigger.condition.is_some(),
                    definition: FoundDefinition::Routine(Box::new(definition)),
                }))
            }
            ConditionCarrier::AutoTask => {
                let Some((_, path)) = self.auto_task_read_source(name)? else {
                    return Ok(None);
                };
                let definition = self.auto_task_show(name)?.ok_or_else(|| {
                    OrbitError::InvalidInput(format!("auto-task '{name}' is no longer defined"))
                })?;
                Ok(Some(LoadedCarrier {
                    carrier,
                    name: definition.name.clone(),
                    path: path.display().to_string(),
                    enabled: definition.enabled,
                    has_condition: matches!(
                        definition.schedule,
                        AutoTaskSchedule::Condition { .. }
                    ),
                    definition: FoundDefinition::AutoTask(Box::new(definition)),
                }))
            }
        }
    }
}
