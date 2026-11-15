//! The shared (model, recorded effort) grouping key for the agent
//! scorecards and the model ranking's effort split. The encoding matches
//! `orbit_store::contracts::agent_effort_usage` exactly: a present dispatch
//! identity with no rendered effort is `None` (the provider default), a
//! historical invocation with no dispatch identity is `Some("unknown")`,
//! and a rendered effort is its wire name (`low`, `medium`, `high`,
//! `xhigh`, `max`). Scorecard rows and the ranking therefore never disagree
//! on what a model-and-effort population is.
//!
//! Effort is attributed per unit of work from that unit's own invocations:
//! the invocations of the unit's job run whose activity matches the role
//! (implementing, reviewing or piloting), narrowed to the unit itself by
//! the model it recorded and, when the producing step or worker is known
//! (a pilot receipt's worker, a review settlement's gate), by that
//! activity id (a review gate also covers its re-review rounds, told apart by
//! the settlement times). A run that carried several rounds — an initial and a
//! repair pilot, a before-PR and a before-landing review — therefore
//! never lends one round's effort to another round's model. A unit with
//! no recorded run has no invocation evidence of its own and is
//! `unknown`; it never borrows a same-task invocation from another
//! attempt or run. A review job is the exception on activity: the task
//! that files findings itself runs as a task executor
//! (`implement_one` / `agent_implement`), so its filing units count under
//! that executor invocation, never under an independent gate review in
//! the same run; gate settlements keep their own reviewer invocation.
//! When the unit's invocations ran at more than one effort, the unit
//! counts under the effort of its main invocation — the matching
//! invocation with the longest recorded duration, ties broken by the
//! latest end time then the highest invocation id. When no completed
//! invocation of the unit's run matches — the agent launched but the
//! invocation never completed — the unit counts under the effort of
//! that run's latest matching launch record (`agent_executions`, same
//! activity and model, narrowed to the unit's own round as above). A
//! unit with neither a matching invocation nor a matching launch
//! record is `unknown` and is never folded into a known effort, except
//! that a pulled attempt whose follower reported its leaf's effort on the
//! claim settlement counts under that report [ORB-23825]: the follower read
//! it from its own records by this same rule.
//!
//! Last, and only when none of those recorded sources names an effort, a
//! unit whose own recorded crew label is a `<crew>@<effort>` reference
//! (`opus@medium`) counts under that effort [ORB-24150]: the label is the
//! reference the unit was dispatched under, so a row never says `effort
//! unknown` while its crew list states the effort. A recorded effort always
//! wins over the label, even when they disagree. A bare crew label (`opus`)
//! names no effort and stays `unknown`: no per-unit record keeps the effort a
//! bare crew resolved to at dispatch, and today's crew configuration is never
//! read back into past work. [`EffortSource`] says which source decided, and
//! `orbit agents show` reports a row's units by source. The model ranking's
//! effort split counts an identity-less implementing invocation under its
//! run's crew-label effort when this key resolved that run's attempt from the
//! label, so the split and the scorecard rows agree.

use orbit_store::contracts::InvocationAccountingFact;
use orbit_types::identity::{CREW_EFFORT_SEPARATOR, ReasoningEffort};

use super::facts::{AgentInvocationFacts, AgentLaunchFacts};
use super::{AgentFacts, MODEL_UNKNOWN};

/// A recorded reasoning effort in the shared encoding: `None` is the
/// provider default, `Some("unknown")` is historical work with no recorded
/// effort, and any other `Some` is the rendered effort's wire name.
pub(super) type RecordedEffort = Option<String>;

/// The effort value of historical work with no recorded dispatch identity.
pub(super) const EFFORT_UNKNOWN: &str = "unknown";

/// Which role's invocations a unit's main invocation is drawn from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MainActivity {
    Implement,
    Review,
    Pilot,
    /// The executor invocation of a review-job task: the filing task runs
    /// under `implement_one` / `agent_implement` regardless of its review
    /// tag, so that invocation — not a gate review in the same run —
    /// performed the filing.
    ReviewJob,
}

impl MainActivity {
    fn matches(self, activity_id: Option<&str>) -> bool {
        match self {
            MainActivity::Implement | MainActivity::ReviewJob => {
                matches!(activity_id, Some("implement_one" | "agent_implement"))
            }
            MainActivity::Review => activity_id.is_some_and(|id| id.contains("review")),
            MainActivity::Pilot => activity_id.is_some_and(|id| id.contains("pilot")),
        }
    }
}

/// One invocation's recorded effort, in the shared encoding.
pub(super) fn effort_of_invocation(fact: &InvocationAccountingFact) -> RecordedEffort {
    match &fact.execution {
        Some(execution) => execution.reasoning_effort.map(|effort| effort.to_string()),
        None => Some(EFFORT_UNKNOWN.to_string()),
    }
}

/// The human label of a recorded effort: the wire name, `provider default`
/// for a null effort, and `effort unknown` for historical work.
pub(super) fn effort_label(effort: &RecordedEffort) -> String {
    match effort {
        None => "provider default".to_string(),
        Some(effort) if effort == EFFORT_UNKNOWN => "effort unknown".to_string(),
        Some(effort) => effort.clone(),
    }
}

/// A row's display label: `model @ effort`.
pub(super) fn row_label(model: &str, effort: &RecordedEffort) -> String {
    format!("{model} @ {}", effort_label(effort))
}

/// Parse a detail selection's effort name into the shared encoding. Accepts
/// the wire names plus `provider_default` / `provider default` and
/// `unknown` / `effort unknown`.
pub(super) fn parse_recorded_effort(value: &str) -> RecordedEffort {
    match value.trim() {
        "provider_default" | "provider default" => None,
        "effort_unknown" | "effort unknown" => Some(EFFORT_UNKNOWN.to_string()),
        other => Some(other.to_string()),
    }
}

/// The shared scorecard row key: model (`model unknown` when none was
/// recorded) and recorded effort.
pub(super) type ModelEffortKey = (String, RecordedEffort);

/// The key of a unit whose model may be unrecorded.
pub(super) fn model_effort_key(model: Option<&str>, effort: RecordedEffort) -> ModelEffortKey {
    (model.unwrap_or(MODEL_UNKNOWN).to_string(), effort)
}

/// One unit of work whose main invocation's effort is wanted. `model`
/// and `activity_id` bind the match to the unit's own round: an
/// invocation recorded for another model, or under another producing
/// step or worker, in the same run is another unit's evidence. Either
/// is `None` when the unit recorded none, and then it does not narrow
/// the match. `after` is the previous round's settlement on the same
/// task, when there is one: an invocation that had already ended by
/// then produced that earlier round, not this one.
#[derive(Debug, Clone, Copy)]
pub(super) struct MainInvocationUnit<'a> {
    pub(super) run_id: Option<&'a str>,
    pub(super) activity: MainActivity,
    pub(super) task_id: Option<&'a str>,
    pub(super) model: Option<&'a str>,
    pub(super) activity_id: Option<&'a str>,
    pub(super) after: Option<chrono::DateTime<chrono::Utc>>,
    /// The unit's own settlement time, when it has one: an invocation that
    /// ended after it belongs to a later round.
    pub(super) until: Option<chrono::DateTime<chrono::Utc>>,
    /// The effort a pulled attempt's follower reported on its claim
    /// settlement, when it reported one: the fallback for a unit this host
    /// holds no matching invocation or launch record of.
    pub(super) reported: Option<&'a RecordedEffort>,
    /// The crew label recorded for the unit (`opus@medium`, `opus`): the
    /// last fallback, used only when it carries an explicit `@<effort>`.
    pub(super) crew: Option<&'a str>,
}

/// Which source decided a unit's effort, in precedence order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum EffortSource {
    /// The unit's main completed invocation.
    Invocation,
    /// The latest matching launch record of an invocation that never
    /// completed.
    LaunchRecord,
    /// The effort a pulled attempt's follower reported on its claim
    /// settlement or later.
    FollowerReport,
    /// The `@<effort>` of the unit's recorded crew label.
    CrewLabel,
    /// No source names an effort: the unit is `effort unknown`.
    Unrecorded,
}

impl EffortSource {
    /// The source's wire name in scorecard output.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            EffortSource::Invocation => "invocation",
            EffortSource::LaunchRecord => "launch_record",
            EffortSource::FollowerReport => "follower_report",
            EffortSource::CrewLabel => "crew_label",
            EffortSource::Unrecorded => "unrecorded",
        }
    }
}

/// The effort a crew label states: `Some` for a `<crew>@<effort>` reference
/// with a known effort name, `None` for a bare crew or an unparsable suffix.
fn crew_label_effort(crew: &str) -> Option<RecordedEffort> {
    let (_, effort) = crew.trim().split_once(CREW_EFFORT_SEPARATOR)?;
    let effort: ReasoningEffort = effort.parse().ok()?;
    Some(Some(effort.to_string()))
}

/// Whether the invocation recorded under pipeline step `step` can have
/// produced a unit bound to `id`. A pilot receipt's worker and a review
/// settlement's gate step (`review`, `landing_review`) match exactly, except
/// that a gate also covers the re-review rounds (`re_review`, `re_review_2`):
/// they settle through the same gate, so the settlement comment names the
/// gate's timing, never the round's step. The unit's `after` and `until`
/// bounds then pick its own round among them.
fn step_produced(id: &str, step: &str) -> bool {
    step == id || (matches!(id, "review" | "landing_review") && step.starts_with("re_review"))
}

/// The effort of a unit's main invocation: among the unit's own
/// invocations in its job run, preferring those linked to its task, the
/// one with the longest recorded duration. When no completed invocation
/// of the run matches, the latest launch record of the run for the same
/// activity and model supplies the effort, then the follower's report, then
/// an explicit `@<effort>` on the unit's crew label. `unknown` when none of
/// them names one.
pub(super) fn main_invocation_effort(
    facts: &AgentFacts,
    unit: MainInvocationUnit<'_>,
) -> RecordedEffort {
    main_effort_with_source(facts, unit).0
}

/// [`main_invocation_effort`] and the source that decided it.
pub(super) fn main_effort_with_source(
    facts: &AgentFacts,
    unit: MainInvocationUnit<'_>,
) -> (RecordedEffort, EffortSource) {
    let (effort, source) = recorded_main_effort(&facts.invocations, &facts.launches, unit);
    if effort.as_deref() != Some(EFFORT_UNKNOWN) {
        return (effort, source);
    }
    if let Some(reported) = unit.reported
        && reported.as_deref() != Some(EFFORT_UNKNOWN)
    {
        return (reported.clone(), EffortSource::FollowerReport);
    }
    match unit.crew.and_then(crew_label_effort) {
        Some(labelled) => (labelled, EffortSource::CrewLabel),
        None => (effort, EffortSource::Unrecorded),
    }
}

/// [`main_invocation_effort`] from this host's own records alone, with
/// whether an invocation or a launch record decided it.
fn recorded_main_effort(
    invocations: &AgentInvocationFacts,
    launches: &AgentLaunchFacts,
    unit: MainInvocationUnit<'_>,
) -> (RecordedEffort, EffortSource) {
    let (effort, _, source) = recorded_main_identity(invocations, launches, unit);
    (effort, source)
}

/// [`recorded_main_effort`] with the model of the record that decided it:
/// the main invocation's model, else the launch record's. `None` when no
/// record decided the effort.
fn recorded_main_identity(
    invocations: &AgentInvocationFacts,
    launches: &AgentLaunchFacts,
    unit: MainInvocationUnit<'_>,
) -> (RecordedEffort, Option<String>, EffortSource) {
    let unrecorded = || {
        (
            Some(EFFORT_UNKNOWN.to_string()),
            None,
            EffortSource::Unrecorded,
        )
    };
    let MainInvocationUnit {
        run_id: Some(run_id),
        activity,
        task_id,
        model,
        activity_id,
        after,
        until,
        reported: _,
        crew: _,
    } = unit
    else {
        return unrecorded();
    };
    let candidates = invocations.invocations_for_run(run_id).filter(|fact| {
        activity.matches(fact.activity_id.as_deref())
            && activity_id.is_none_or(|id| {
                fact.activity_id
                    .as_deref()
                    .is_some_and(|step| step_produced(id, step))
            })
            && model.is_none_or(|model| fact.model.as_deref() == Some(model))
            && after.is_none_or(|after| fact.ts > after)
            && until.is_none_or(|until| fact.ts <= until)
    });
    let linked: Vec<_> = match task_id {
        Some(task) => {
            let linked: Vec<_> = candidates
                .clone()
                .filter(|fact| fact.task_ids.iter().any(|id| id == task))
                .collect();
            if linked.is_empty() {
                candidates.collect()
            } else {
                linked
            }
        }
        None => candidates.collect(),
    };
    if let Some(main) = linked.into_iter().max_by(|left, right| {
        left.duration_ms
            .cmp(&right.duration_ms)
            .then(left.ts.cmp(&right.ts))
            .then(left.id.cmp(&right.id))
    }) {
        return match effort_of_invocation(main) {
            effort if effort.as_deref() == Some(EFFORT_UNKNOWN) => unrecorded(),
            effort => (effort, main.model.clone(), EffortSource::Invocation),
        };
    }
    // No completed invocation of this unit's run matched: the agent may
    // have launched without ever completing (a failed, interrupted, held
    // or cancelled run). Its launch record still carries the dispatched
    // effort, so take the latest launch of this run for the same
    // activity and model, narrowed to the unit's own round exactly as
    // the invocation match above. A launch for another model or another
    // activity in the same run is another unit's evidence.
    launches
        .launches_for_run(run_id)
        .filter(|record| {
            activity.matches(Some(record.activity_id.as_str()))
                && activity_id.is_none_or(|id| step_produced(id, record.activity_id.as_str()))
                && model.is_none_or(|model| record.model.as_deref() == Some(model))
                && after.is_none_or(|after| record.started_at > after)
                && until.is_none_or(|until| record.started_at <= until)
        })
        .max_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then(left.id.cmp(&right.id))
        })
        .map_or_else(unrecorded, |record| {
            (
                record
                    .identity
                    .reasoning_effort
                    .map(|effort| effort.to_string()),
                record.model.clone(),
                EffortSource::LaunchRecord,
            )
        })
}

impl crate::OrbitRuntime {
    /// The reasoning effort a claimed leaf `run` ran at, as its follower
    /// reports it on the claim settlement: the effort of the run's main
    /// implementing invocation (else its latest matching launch record) in
    /// this host's own records, exactly as this host's scorecards would count
    /// it. `Some(None)` is the provider default, and `None` an effort this
    /// host does not know (no matching record, or an unreadable store), which
    /// the follower then omits. Attribution only; never derived from crew
    /// configuration or the leaf's crew label.
    pub(crate) fn leaf_reported_effort(
        &self,
        run: &orbit_types::workflow::JobRun,
    ) -> Option<RecordedEffort> {
        self.leaf_reported_identity(run).map(|(effort, _)| effort)
    }

    /// [`Self::leaf_reported_effort`] with the model of the same record (the
    /// main implementing invocation, else the launch record), when that
    /// record names one [ORB-24150]. Never derived from a crew name or
    /// configuration.
    pub(crate) fn leaf_reported_identity(
        &self,
        run: &orbit_types::workflow::JobRun,
    ) -> Option<(RecordedEffort, Option<String>)> {
        let store = self.sqlite_store().ok()?;
        let workspace_id = self.stores().jobs().workspace_id().to_string();
        let since = Some(run.created_at);
        let until = chrono::Utc::now();
        let invocations = store
            .list_invocation_accounting_facts(&orbit_store::contracts::InvocationAccountingQuery {
                workspace_id: Some(workspace_id.clone()),
                since,
                until,
            })
            .unwrap_or_else(|error| {
                tracing::warn!(%error, run_id = %run.run_id, "leaf effort: invocations unreadable");
                Vec::new()
            });
        let launches = store
            .list_agent_executions(&orbit_store::contracts::AgentExecutionQuery {
                workspace_id: Some(workspace_id),
                since,
                until,
            })
            .unwrap_or_else(|error| {
                tracing::warn!(%error, run_id = %run.run_id, "leaf effort: launch records unreadable");
                Vec::new()
            });
        let run_id = run.run_id.to_string();
        let (effort, model, _) = recorded_main_identity(
            &invocations.into(),
            &launches.into(),
            MainInvocationUnit {
                run_id: Some(&run_id),
                activity: MainActivity::Implement,
                task_id: None,
                model: run.crew_model.as_deref(),
                activity_id: None,
                after: None,
                until: None,
                reported: None,
                crew: None,
            },
        );
        let model = model.filter(|model| !model.trim().is_empty());
        (effort.as_deref() != Some(EFFORT_UNKNOWN)).then_some((effort, model))
    }
}
