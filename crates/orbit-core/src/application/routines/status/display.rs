use super::*;

/// Consecutive failures at the head of a routine's fire history.
#[derive(Debug, Clone)]
pub struct RoutineFailureStreak {
    /// Number of distinct fires whose latest attempt failed.
    pub count: usize,
    /// Slot of the oldest failure in the streak (RFC 3339).
    pub since: Option<String>,
    /// History ended before a non-failure was seen: count is a lower bound
    /// and the streak may have started before `since`.
    pub truncated: bool,
}

/// Operator-facing schedule readiness. Theoretical next-slot math may still be
/// present; this state says whether that time is armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleDisplayState {
    /// Enabled, not paused, and a next slot is a real scheduled evaluation.
    Scheduled,
    /// Definition `enabled` is false. A next slot, if present, is hypothetical.
    Disabled,
    /// Host-local pause. A next slot, if present, is hypothetical.
    Paused,
    /// Enabled delivery- or state-triggered work is waiting on that trigger.
    Waiting,
    /// The scheduler has never recorded a cursor for this definition.
    NeverObserved,
    /// Source or trigger state cannot be shown as a next evaluation, or this
    /// host can never admit the delivery work it would evaluate.
    Unavailable,
}

impl ScheduleDisplayState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Disabled => "disabled",
            Self::Paused => "paused",
            Self::Waiting => "waiting",
            Self::NeverObserved => "never_observed",
            Self::Unavailable => "unavailable",
        }
    }

    /// Disabled and paused rows may still carry a theoretical next slot.
    pub fn is_hypothetical(self) -> bool {
        matches!(self, Self::Disabled | Self::Paused)
    }

    /// The `{state, at, hypothetical}` next-evaluation record. A waiting
    /// trigger has no slot to show.
    pub fn next_evaluation(self, at: Option<String>) -> Value {
        json!({
            "state": self.as_str(),
            "at": if self == Self::Waiting { None } else { at },
            "hypothetical": self.is_hypothetical(),
        })
    }
}

/// Full effective state of one routine on this host.
#[derive(Debug, Clone)]
pub struct RoutineStatus {
    /// The loaded definition plus its provenance.
    pub routine: LoadedRoutine,
    /// Host-local pause, when one is set (RFC 3339 pause timestamp).
    pub paused_at: Option<String>,
    /// First scheduler observation on this host (RFC 3339).
    pub first_observed_at: Option<String>,
    /// Most recent scheduled slot consumed by the scheduler (RFC 3339).
    pub last_evaluated_slot: Option<String>,
    /// Next scheduled slot (RFC 3339, host-local), when computable.
    pub next_due: Option<String>,
    /// Most recent fire recorded on this host, from whichever trigger fired
    /// it: the cron fire store, or for a state-, delivery- or
    /// condition-triggered routine the newest run that routine's automation
    /// admitted when that is newer.
    pub last_fire: Option<RoutineFireRecord>,
    /// Recent distinct fires, newest first, including automation runs.
    pub recent_fires: Vec<RoutineFireRecord>,
    /// Latest consecutive failures; success, skips and in-flight fires reset it.
    pub failure_streak: RoutineFailureStreak,
    pub automation: Option<serde_json::Value>,
}

impl RoutineStatus {
    /// Whether the routine would currently fire on this host when due:
    /// enabled and not paused.
    pub fn effective(&self) -> bool {
        self.routine.definition.enabled && self.paused_at.is_none()
    }

    /// How Operations (and other projections) should label the next slot.
    pub fn schedule_display_state(&self) -> ScheduleDisplayState {
        if !self.routine.definition.enabled {
            return ScheduleDisplayState::Disabled;
        }
        if self.paused_at.is_some() {
            return ScheduleDisplayState::Paused;
        }
        if automation_unavailable(self.automation.as_ref()) {
            return ScheduleDisplayState::Unavailable;
        }
        if self.routine.definition.trigger.deliveries_landed.is_some()
            || self.routine.definition.trigger.state.is_some()
            || self.routine.definition.trigger.condition.is_some()
        {
            return ScheduleDisplayState::Waiting;
        }
        if self.next_due.is_some() {
            return ScheduleDisplayState::Scheduled;
        }
        if self.first_observed_at.is_none() {
            return ScheduleDisplayState::NeverObserved;
        }
        ScheduleDisplayState::Unavailable
    }

    /// This status as the `(key, field)` columns a surface renders, in order.
    pub fn project(&self, columns: &[(&str, RoutineField)]) -> Map<String, Value> {
        columns
            .iter()
            .map(|(key, field)| ((*key).to_string(), self.field(*field)))
            .collect()
    }

    fn field(&self, field: RoutineField) -> Value {
        let routine = &self.routine;
        let definition = &routine.definition;
        match field {
            RoutineField::Name => json!(definition.name),
            RoutineField::Description => json!(definition.description),
            RoutineField::Source => json!(routine.source_workspace),
            RoutineField::Origin => json!(routine.origin.as_str()),
            RoutineField::Path => json!(routine.path.display().to_string()),
            RoutineField::Target => json!(definition.target.as_ref_string()),
            RoutineField::Enabled => json!(definition.enabled),
            RoutineField::PausedAt => json!(self.paused_at),
            RoutineField::Effective => json!(self.effective()),
            RoutineField::Cron => json!(definition.trigger.cron),
            RoutineField::Trigger => json!(definition.trigger),
            RoutineField::MissedRun => json!(definition.trigger.missed_run),
            RoutineField::Policy => json!(definition.policy),
            RoutineField::Automation => json!(self.automation),
            RoutineField::FirstObservedAt => json!(self.first_observed_at),
            RoutineField::LastEvaluatedSlot => json!(self.last_evaluated_slot),
            RoutineField::NextDue => json!(self.next_due),
            RoutineField::NextEvaluation => self
                .schedule_display_state()
                .next_evaluation(self.next_due.clone()),
            RoutineField::State => json!(self.schedule_display_state().as_str()),
            RoutineField::LastFire(columns) => json!(
                self.last_fire
                    .as_ref()
                    .map(|fire| project_fire(fire, columns))
            ),
            RoutineField::RecentFires(columns) => json!(
                self.recent_fires
                    .iter()
                    .map(|fire| project_fire(fire, columns))
                    .collect::<Vec<_>>()
            ),
            RoutineField::FailureStreak => json!({
                "count": self.failure_streak.count,
                "since": self.failure_streak.since,
                "truncated": self.failure_streak.truncated,
            }),
        }
    }
}

/// One field of the shared routine status projection ([`RoutineStatus::project`]).
/// Each surface pairs a field with the key it renders it under.
#[derive(Debug, Clone, Copy)]
pub enum RoutineField {
    Name,
    Description,
    /// Source workspace name.
    Source,
    Origin,
    /// Definition file path.
    Path,
    Target,
    /// Versioned definition `enabled`.
    Enabled,
    PausedAt,
    /// Enabled and not paused.
    Effective,
    Cron,
    /// The whole trigger definition.
    Trigger,
    MissedRun,
    Policy,
    /// Raw automation diagnostic of a state- or delivery-triggered routine.
    Automation,
    FirstObservedAt,
    LastEvaluatedSlot,
    NextDue,
    /// [`ScheduleDisplayState::next_evaluation`] over the next slot.
    NextEvaluation,
    /// [`ScheduleDisplayState`] token.
    State,
    /// The last fire, projected with these columns.
    LastFire(&'static [(&'static str, FireField)]),
    /// Recent distinct fires, newest first, projected with these columns.
    RecentFires(&'static [(&'static str, FireField)]),
    /// `{count, since, truncated}` of the latest consecutive failures.
    FailureStreak,
}

/// One field of the shared fire projection ([`project_fire`]).
#[derive(Debug, Clone, Copy)]
pub enum FireField {
    Slot,
    Attempt,
    State,
    /// Coarse outcome: true on success, false on failure, null otherwise.
    Ok,
    RunId,
    Detail,
    /// When the attempt was recorded.
    StartedAt,
    /// Last update of a terminal attempt; null while it is in flight.
    FinishedAt,
    /// Wall-clock duration of a terminal attempt.
    DurationMs,
    UpdatedAt,
}

/// One fire attempt as the `(key, field)` columns a surface renders, in order.
pub fn project_fire(fire: &RoutineFireRecord, columns: &[(&str, FireField)]) -> Value {
    let terminal = fire.state.is_terminal();
    let field = |field: FireField| match field {
        FireField::Slot => json!(fire.slot),
        FireField::Attempt => json!(fire.attempt),
        FireField::State => json!(fire.state.as_str()),
        FireField::Ok => json!(match fire.state {
            RoutineFireState::Succeeded => Some(true),
            RoutineFireState::Failed | RoutineFireState::TimedOut | RoutineFireState::Error => {
                Some(false)
            }
            RoutineFireState::Skipped | RoutineFireState::Intent | RoutineFireState::Dispatched => {
                None
            }
        }),
        FireField::RunId => json!(fire.run_id),
        FireField::Detail => json!(fire.detail),
        FireField::StartedAt => json!(fire.created_at),
        FireField::FinishedAt => json!(terminal.then(|| fire.updated_at.clone())),
        FireField::DurationMs => json!(terminal.then(|| fire_duration_ms(fire)).flatten()),
        FireField::UpdatedAt => json!(fire.updated_at),
    };
    Value::Object(
        columns
            .iter()
            .map(|(key, column)| ((*key).to_string(), field(*column)))
            .collect(),
    )
}

fn fire_duration_ms(fire: &RoutineFireRecord) -> Option<i64> {
    let start = DateTime::parse_from_rfc3339(&fire.created_at).ok()?;
    let end = DateTime::parse_from_rfc3339(&fire.updated_at).ok()?;
    Some((end - start).num_milliseconds())
}

fn automation_unavailable(automation: Option<&serde_json::Value>) -> bool {
    automation
        .and_then(|value| value.get("reason"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|reason| reason == "source_unavailable" || reason == "state_unavailable")
}
