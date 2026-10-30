//! The edge rules (`docs/design/condition-triggers/1_scope.md` §3, §8): one
//! pass over observed subjects moves each subject between armed and fired,
//! applies holds, continuity, cooldown, caps and the storm, chain and
//! oscillation guards, and records new fires as intents. Pure: the caller
//! supplies the observations and commits the result.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_types::workflow::automation::{
    CONDITION_FIRED_RETENTION_DAYS, CONDITION_OSCILLATION_FIRES_PER_HOUR,
    CONDITION_RETAINED_RECORDS, CONDITION_WORKSPACE_SUBJECT, ConditionArm, ConditionFire,
    ConditionFireState, ConditionOnEnable, ConditionOrigin, ConditionSkip, ConditionStall,
    ConditionState, ConditionSubject, ConditionTrigger, MAX_CONDITION_CHAIN_DEPTH,
};
use serde::Serialize;

use super::evaluate::{SelfFacts, Truth};
use crate::AutomationError;
use crate::delivery::definition_epoch;

/// Most subjects a stall or refusal lists.
const STALL_SAMPLE: usize = 20;

/// One subject as this pass observed it.
#[derive(Debug, Clone)]
pub struct SubjectObservation {
    pub key: String,
    pub when: Truth,
    /// The `rearm` expression's value; `None` means the default `!(when)`.
    pub rearm: Option<Truth>,
    /// The observation revision an edge here would freeze.
    pub revision: String,
    /// The subject's fields, passed to the action on a fire.
    pub captured: serde_json::Value,
    /// The fire that created the subject, when one did.
    pub origin: Option<ConditionOrigin>,
}

/// Everything one pass observed.
#[derive(Debug, Clone, Default)]
pub struct ConditionObservation {
    pub subjects: Vec<SubjectObservation>,
    /// Every live subject was observed, so a tracked subject missing here no
    /// longer exists.
    pub complete: bool,
    /// Whether this lap reached the end of the prefilter.
    pub lap_complete: bool,
    /// Where the next lap continues, when this one stopped early.
    pub lap_after: Option<String>,
}

/// The definition-level facts a pass applies.
#[derive(Debug, Clone, Copy)]
pub struct PassSettings<'a> {
    pub trigger: &'a ConditionTrigger,
    pub epoch: &'a str,
    pub now: DateTime<Utc>,
    /// The clock cadence; the continuity limit is `max(3 × cadence, 5 min)`.
    pub cadence_seconds: u64,
}

/// What one pass decided.
#[derive(Debug, Clone)]
pub struct PassOutcome {
    pub next: ConditionState,
    /// Fire IDs this pass recorded as intents.
    pub fired: Vec<String>,
    /// The pass's headline: `fired`, `baselined`, `armed`, `holding`,
    /// `waiting`, or a stall reason.
    pub reason: &'static str,
}

/// The longest observation gap a hold survives.
pub fn continuity_limit(cadence_seconds: u64) -> Duration {
    let three =
        Duration::seconds(i64::try_from(cadence_seconds.saturating_mul(3)).unwrap_or(i64::MAX / 4));
    three.max(Duration::minutes(5))
}

/// The epoch: a digest of what gives an edge its meaning. An edit of any of
/// these baselines the condition again; other edits keep its edge memory.
pub fn condition_epoch<T: Serialize>(
    trigger: &ConditionTrigger,
    action: &T,
) -> Result<String, AutomationError> {
    definition_epoch(&(
        trigger.scope,
        &trigger.when,
        &trigger.rearm,
        trigger.for_minutes,
        action,
    ))
}

/// The durable identity of one edge.
pub fn fire_identity(
    consumer: &str,
    epoch: &str,
    subject: &str,
    revision: &str,
    arm_sequence: u64,
) -> Result<String, AutomationError> {
    definition_epoch(&(consumer, epoch, subject, revision, arm_sequence))
}

/// `self` as the definition's fire records give it.
pub fn self_facts(state: Option<&ConditionState>, now: DateTime<Utc>) -> SelfFacts {
    let Some(state) = state else {
        return SelfFacts::default();
    };
    let admitted = state.fires.iter().filter(|fire| fire.state.admits());
    let last_fired = admitted.clone().map(|fire| fire.observed_at).max();
    let last_minted = state
        .fires
        .iter()
        .filter(|fire| fire.state == ConditionFireState::Acknowledged)
        .filter_map(|fire| Some((fire.acknowledged_at?, fire.action_id.clone()?)))
        .max_by_key(|(at, _)| *at);
    let day_ago = now - Duration::hours(24);
    SelfFacts {
        last_fired,
        last_minted: last_minted.as_ref().map(|(at, _)| *at),
        last_minted_task: last_minted.map(|(_, task)| task),
        fires_24h: i64::try_from(admitted.filter(|fire| fire.observed_at > day_ago).count())
            .unwrap_or(i64::MAX),
    }
}

/// The task IDs this definition's acknowledged fires minted, which its
/// `board` leaves out unless `board_includes_own` (§2).
pub fn own_task_ids(state: Option<&ConditionState>) -> BTreeSet<String> {
    state
        .map(|state| {
            state
                .fires
                .iter()
                .filter(|fire| fire.state == ConditionFireState::Acknowledged)
                .filter_map(|fire| fire.action_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn negate(truth: &Truth) -> Truth {
    match truth {
        Truth::True => Truth::False,
        Truth::False => Truth::True,
        Truth::Unknown(reason) => Truth::Unknown(reason.clone()),
    }
}

/// Counts skips by reason and keeps the latest.
struct Skips<'a> {
    state: &'a mut ConditionState,
    now: DateTime<Utc>,
}

impl Skips<'_> {
    fn record(&mut self, reason: &str, subject: Option<&str>, detail: Option<String>) {
        *self
            .state
            .skip_counts
            .entry(reason.to_string())
            .or_default() += 1;
        self.state.last_skip = Some(ConditionSkip {
            at: self.now,
            reason: reason.to_string(),
            subject: subject.map(str::to_owned),
            detail,
        });
    }
}

/// Run one pass. `state` is the committed state the pass started from (a new
/// consumer passes [`ConditionState::new`]); the caller bumps the generation
/// and commits `next`. A stalled consumer is the caller's to report: this
/// never runs over one.
pub fn advance_pass(
    state: &ConditionState,
    observation: &ConditionObservation,
    settings: PassSettings<'_>,
) -> Result<PassOutcome, AutomationError> {
    let PassSettings {
        trigger,
        epoch,
        now,
        cadence_seconds,
    } = settings;
    let mut next = state.clone();
    prune(&mut next, now);
    let observed: BTreeMap<&str, &SubjectObservation> = observation
        .subjects
        .iter()
        .map(|subject| (subject.key.as_str(), subject))
        .collect();
    let matched = observation
        .subjects
        .iter()
        .filter(|subject| subject.when == Truth::True)
        .collect::<Vec<_>>();
    next.last_pass.at = Some(now);
    next.last_pass.evaluated = observation.subjects.len();
    next.last_pass.matched = matched.len();
    next.last_pass.unknown = observation
        .subjects
        .iter()
        .filter(|subject| matches!(subject.when, Truth::Unknown(_)))
        .count();
    next.last_pass.fired = 0;
    next.last_pass.lap_complete = observation.lap_complete;
    next.lap_after = observation.lap_after.clone();

    let enabling = !state.enabled;
    let epoch_changed = state.epoch != epoch;
    if enabling || epoch_changed {
        // Enable, re-enable and an epoch change baseline (§3). More current
        // matches than `max_matches` refuses the change; the next pass checks
        // again, so the consumer keeps its old epoch and enabled flag.
        if matched.len() > trigger.max_matches as usize {
            let sample = sample(matched.iter().map(|subject| subject.key.as_str()));
            next.stall = Some(ConditionStall {
                reason: "too_many_matches".into(),
                at: now,
                detail: format!(
                    "{} subjects match at {}; max_matches is {}",
                    matched.len(),
                    if epoch_changed && !enabling {
                        "the definition change"
                    } else {
                        "enable"
                    },
                    trigger.max_matches
                ),
                sample,
            });
            next.epoch = state.epoch.clone();
            next.enabled = state.enabled;
            Skips {
                state: &mut next,
                now,
            }
            .record("too_many_matches", None, None);
            return Ok(PassOutcome {
                next,
                fired: Vec::new(),
                reason: "too_many_matches",
            });
        }
        next.enabled = true;
        next.epoch = epoch.to_string();
        next.lap_after = observation.lap_after.clone();
        let fire_existing = trigger.on_enable == ConditionOnEnable::FireExisting;
        for subject in &observation.subjects {
            if !next.subjects.contains_key(&subject.key)
                && subject.key != CONDITION_WORKSPACE_SUBJECT
                && subject.when != Truth::True
            {
                continue;
            }
            if !next.subjects.contains_key(&subject.key)
                && next.subjects.len() >= CONDITION_RETAINED_RECORDS
            {
                // A partial baseline would turn remaining existing matches into
                // new edges on the next pass. Retry the entire baseline instead.
                let mut retry = state.clone();
                prune(&mut retry, now);
                Skips {
                    state: &mut retry,
                    now,
                }
                .record("backpressure", Some(&subject.key), None);
                return Ok(PassOutcome {
                    next: retry,
                    fired: Vec::new(),
                    reason: "backpressure",
                });
            }
            let sequence = next
                .fires
                .iter()
                .filter(|fire| fire.subject == subject.key)
                .map(|fire| fire.arm_sequence)
                .max()
                .map_or(0, |n| n.saturating_add(1));
            let record =
                next.subjects
                    .entry(subject.key.clone())
                    .or_insert_with(|| ConditionSubject {
                        arm_sequence: sequence,
                        ..Default::default()
                    });
            // Each baseline starts a new arm generation, so a later fire of
            // the same subject never reuses an earlier identity.
            record.arm_sequence = record.arm_sequence.saturating_add(1);
            record.true_since = None;
            record.deferred = None;
            record.deferred_since = None;
            record.observed_at = Some(now);
            if subject.when == Truth::True && !fire_existing {
                record.arm = ConditionArm::Fired;
                record.baselined = true;
            } else {
                record.arm = ConditionArm::Armed;
                record.baselined = false;
            }
        }
        if !fire_existing {
            compact(&mut next, observation, now);
            return Ok(PassOutcome {
                next,
                fired: Vec::new(),
                reason: "baselined",
            });
        }
    }

    let gap = continuity_limit(cadence_seconds);
    let hold = Duration::minutes(i64::try_from(trigger.for_minutes).unwrap_or(i64::MAX / 60_000));
    let cooldown =
        Duration::minutes(i64::try_from(trigger.cooldown_minutes).unwrap_or(i64::MAX / 60_000));
    let mut edges = Vec::new();
    let mut holding = 0usize;
    let mut skips = Vec::<(&'static str, String, Option<String>)>::new();
    let mut backpressure = false;
    for subject in &observation.subjects {
        if !next.subjects.contains_key(&subject.key)
            && subject.key != CONDITION_WORKSPACE_SUBJECT
            && subject.when != Truth::True
        {
            if let Truth::Unknown(reason) = &subject.when {
                skips.push(("unknown", subject.key.clone(), Some(reason.clone())));
            }
            continue;
        }
        if next.subjects.len() >= CONDITION_RETAINED_RECORDS
            && !next.subjects.contains_key(&subject.key)
        {
            skips.push(("backpressure", subject.key.clone(), None));
            backpressure = true;
            continue;
        }
        let sequence = next
            .fires
            .iter()
            .filter(|fire| fire.subject == subject.key)
            .map(|fire| fire.arm_sequence)
            .max()
            .map_or(0, |n| n.saturating_add(1));
        let record = next
            .subjects
            .entry(subject.key.clone())
            .or_insert_with(|| ConditionSubject {
                arm_sequence: sequence,
                ..Default::default()
            });
        let continuous = record
            .observed_at
            .is_some_and(|at| now.signed_duration_since(at) <= gap);
        record.observed_at = Some(now);
        match record.arm {
            ConditionArm::Armed => match &subject.when {
                Truth::True => {
                    let since = match record.true_since {
                        Some(since) if continuous => since,
                        _ => now,
                    };
                    record.true_since = Some(since);
                    if now.signed_duration_since(since) < hold {
                        holding += 1;
                        skips.push(("holding", subject.key.clone(), None));
                    } else if let Some(last) = record.last_fired_at
                        && now < last + cooldown
                    {
                        if record.deferred.as_deref() != Some("cooldown") {
                            record.deferred = Some("cooldown".into());
                            record.deferred_since = Some(now);
                        }
                        skips.push(("cooldown", subject.key.clone(), None));
                    } else {
                        edges.push(subject.key.clone());
                    }
                }
                Truth::False => {
                    record.true_since = None;
                    record.deferred = None;
                    record.deferred_since = None;
                }
                Truth::Unknown(reason) => {
                    // Unknown neither fires nor re-arms, and it breaks a hold.
                    record.true_since = None;
                    skips.push(("unknown", subject.key.clone(), Some(reason.clone())));
                }
            },
            ConditionArm::Fired => {
                let rearm = subject
                    .rearm
                    .clone()
                    .unwrap_or_else(|| negate(&subject.when));
                match rearm {
                    Truth::True => {
                        record.arm = ConditionArm::Armed;
                        record.arm_sequence = record.arm_sequence.saturating_add(1);
                        record.true_since = None;
                        record.deferred = None;
                        record.deferred_since = None;
                        record.baselined = false;
                    }
                    Truth::False => {}
                    Truth::Unknown(reason) => {
                        skips.push(("unknown", subject.key.clone(), Some(reason)));
                    }
                }
            }
        }
    }
    if backpressure {
        next.lap_after.clone_from(&state.lap_after);
        next.last_pass.lap_complete = false;
    }
    {
        let mut recorder = Skips {
            state: &mut next,
            now,
        };
        for (reason, subject, detail) in skips {
            recorder.record(reason, Some(&subject), detail);
        }
    }

    // More new edges in one pass than `max_matches` is a storm: nothing
    // fires until an operator clears it (§8).
    if edges.len() > trigger.max_matches as usize {
        next.stall = Some(ConditionStall {
            reason: "match_storm".into(),
            at: now,
            detail: format!(
                "{} new edges in one pass; max_matches is {}",
                edges.len(),
                trigger.max_matches
            ),
            sample: sample(edges.iter().map(String::as_str)),
        });
        Skips {
            state: &mut next,
            now,
        }
        .record("match_storm", None, None);
        compact(&mut next, observation, now);
        return Ok(PassOutcome {
            next,
            fired: Vec::new(),
            reason: "match_storm",
        });
    }

    // Waiting edges go first, oldest first; fire caps never drop or burst.
    edges.sort_by_key(|key| {
        (
            next.subjects
                .get(key)
                .and_then(|record| record.deferred_since)
                .unwrap_or(now),
            key.clone(),
        )
    });
    let hour_ago = now - Duration::hours(1);
    let day_ago = now - Duration::hours(24);
    let mut fired = Vec::new();
    let mut reason = if backpressure {
        "backpressure"
    } else if holding > 0 {
        "holding"
    } else {
        "armed"
    };
    for key in edges {
        let Some(subject) = observed.get(key.as_str()) else {
            continue;
        };
        let depth = subject.origin.as_ref().map_or(0, |origin| origin.depth) + 1;
        let recent = next
            .fires
            .iter()
            .filter(|fire| fire.state.admits() && fire.observed_at > hour_ago)
            .count();
        let today = next
            .fires
            .iter()
            .filter(|fire| fire.state.admits() && fire.observed_at > day_ago)
            .count();
        let subject_recent = next
            .fires
            .iter()
            .filter(|fire| {
                fire.subject == key && fire.state.admits() && fire.observed_at > hour_ago
            })
            .count();
        let record = next.subjects.get(&key).cloned().unwrap_or_default();
        if next.fires.len() >= CONDITION_RETAINED_RECORDS {
            if let Some(index) = next.fires.iter().position(|fire| {
                fire.state != ConditionFireState::Intent && fire.observed_at <= day_ago
            }) {
                next.fires.remove(index);
            } else {
                if let Some(entry) = next.subjects.get_mut(&key) {
                    entry.deferred = Some("backpressure".into());
                    entry.deferred_since.get_or_insert(now);
                }
                Skips {
                    state: &mut next,
                    now,
                }
                .record("backpressure", Some(&key), None);
                reason = "backpressure";
                continue;
            }
        }
        if depth > MAX_CONDITION_CHAIN_DEPTH {
            let fire = new_fire(
                &next,
                subject,
                &record,
                FireMeta {
                    epoch,
                    now,
                    depth,
                    state: ConditionFireState::Refused,
                    detail: Some(format!(
                        "loop_suspected: chain depth {depth} exceeds {MAX_CONDITION_CHAIN_DEPTH}"
                    )),
                },
            )?;
            next.stall = Some(ConditionStall {
                reason: "loop_suspected".into(),
                at: now,
                detail: format!(
                    "{key} was created by a chain {} fires deep; the limit is \
                     {MAX_CONDITION_CHAIN_DEPTH}",
                    depth - 1
                ),
                sample: vec![key.clone()],
            });
            if let Some(entry) = next.subjects.get_mut(&key) {
                entry.arm = ConditionArm::Fired;
                entry.true_since = None;
                entry.fire_id = Some(fire.fire_id.clone());
            }
            next.fires.push(fire);
            Skips {
                state: &mut next,
                now,
            }
            .record("loop_suspected", Some(&key), None);
            reason = "loop_suspected";
            break;
        }
        if subject_recent >= CONDITION_OSCILLATION_FIRES_PER_HOUR {
            next.stall = Some(ConditionStall {
                reason: "oscillating".into(),
                at: now,
                detail: format!(
                    "{key} fired {subject_recent} times in the last hour; the limit is \
                     {CONDITION_OSCILLATION_FIRES_PER_HOUR}"
                ),
                sample: vec![key.clone()],
            });
            Skips {
                state: &mut next,
                now,
            }
            .record("oscillating", Some(&key), None);
            reason = "oscillating";
            break;
        }
        if recent >= trigger.max_fires_per_hour as usize
            || today >= trigger.max_fires_per_day as usize
        {
            if let Some(entry) = next.subjects.get_mut(&key)
                && entry.deferred.as_deref() != Some("fire_cap")
            {
                entry.deferred = Some("fire_cap".into());
                entry.deferred_since.get_or_insert(now);
            }
            Skips {
                state: &mut next,
                now,
            }
            .record("fire_cap", Some(&key), None);
            reason = "fire_cap";
            continue;
        }
        let fire = new_fire(
            &next,
            subject,
            &record,
            FireMeta {
                epoch,
                now,
                depth,
                state: ConditionFireState::Intent,
                detail: None,
            },
        )?;
        if let Some(entry) = next.subjects.get_mut(&key) {
            entry.arm = ConditionArm::Fired;
            entry.true_since = None;
            entry.deferred = None;
            entry.deferred_since = None;
            entry.baselined = false;
            entry.last_fired_at = Some(now);
            entry.fire_id = Some(fire.fire_id.clone());
        }
        fired.push(fire.fire_id.clone());
        next.fires.push(fire);
    }
    if !fired.is_empty() && next.stall.is_none() && reason != "backpressure" {
        reason = "fired";
    }
    next.last_pass.fired = fired.len();
    compact(&mut next, observation, now);
    Ok(PassOutcome {
        next,
        fired,
        reason,
    })
}

struct FireMeta<'a> {
    epoch: &'a str,
    now: DateTime<Utc>,
    depth: u32,
    state: ConditionFireState,
    detail: Option<String>,
}

fn new_fire(
    state: &ConditionState,
    subject: &SubjectObservation,
    record: &ConditionSubject,
    meta: FireMeta<'_>,
) -> Result<ConditionFire, AutomationError> {
    let fire_id = fire_identity(
        &state.consumer,
        meta.epoch,
        &subject.key,
        &subject.revision,
        record.arm_sequence,
    )?;
    Ok(ConditionFire {
        action_key: format!("condition:{fire_id}:0"),
        fire_id,
        subject: subject.key.clone(),
        revision: subject.revision.clone(),
        arm_sequence: record.arm_sequence,
        epoch: meta.epoch.to_string(),
        observed_at: meta.now,
        depth: meta.depth,
        parent_fire_id: subject.origin.as_ref().map(|origin| origin.fire_id.clone()),
        captured: subject.captured.clone(),
        state: meta.state,
        action_id: None,
        acknowledged_at: None,
        attempts: 0,
        detail: meta.detail,
    })
}

fn sample<'a>(keys: impl Iterator<Item = &'a str>) -> Vec<String> {
    keys.take(STALL_SAMPLE).map(str::to_owned).collect()
}

/// Drop fire records past retention, never an unsettled intent. Capacity
/// eviction happens only when a new record needs room.
fn prune(state: &mut ConditionState, now: DateTime<Utc>) {
    let horizon = now - Duration::days(CONDITION_FIRED_RETENTION_DAYS);
    state
        .fires
        .retain(|fire| fire.state == ConditionFireState::Intent || fire.observed_at >= horizon);
}

/// Forget subjects that carry nothing: armed, not holding, not waiting and
/// never re-armed. A subject no longer seen keeps its fired tombstone for
/// the retention window, so an archived-and-restored task does not fire again
/// inside it (§3).
fn compact(state: &mut ConditionState, observation: &ConditionObservation, now: DateTime<Utc>) {
    let seen = observation
        .subjects
        .iter()
        .map(|subject| subject.key.as_str())
        .collect::<BTreeSet<_>>();
    let horizon = now - Duration::days(CONDITION_FIRED_RETENTION_DAYS);
    state.subjects.retain(|key, record| {
        if key == CONDITION_WORKSPACE_SUBJECT {
            return true;
        }
        let present = seen.contains(key.as_str()) || !observation.complete;
        match record.arm {
            ConditionArm::Fired => {
                present
                    || record
                        .last_fired_at
                        .or(record.observed_at)
                        .is_some_and(|at| at >= horizon)
            }
            ConditionArm::Armed => {
                present
                    && (record.true_since.is_some()
                        || record.deferred.is_some()
                        || record.arm_sequence > 0)
            }
        }
    });
}
