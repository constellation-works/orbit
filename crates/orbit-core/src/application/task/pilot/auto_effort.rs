//! `<crew>@auto` in `workflow.purpose_crews.task_pilot`.
//!
//! No size estimate exists before a pilot, because the pilot produces it,
//! so a pilot's effort resolves from the filed complexity. A run draws one
//! crew from the pool; preparation partitions its tasks so every partition
//! resolves to a single effort for whichever `@auto` crew the run draws
//! ([`pilot_effort_key`]), and each partition's dispatch resolves and
//! freezes its own `<crew>@<effort>` under
//! `workflow.purpose_crews.task_pilot#<round>:<partition>`.
//!
//! Escalation: when a pilot that ran at an `@auto` effort raises the task to
//! a tier whose band maps to a higher effort, or reports confidence
//! `medium` or `low`, apply holds its assessment back and the run's repair
//! round re-pilots the task once at that higher effort
//! ([`AUTO_EFFORT_ESCALATION`]). The higher-effort assessment is the one
//! applied; a repair round never escalates again.

use orbit_types::identity::{AutoEffortChoice, AutoEffortSignal, auto_effort_base};
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::{ActivityCrewDraw, CrewPurpose};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskJobPort;

/// A partition's representative signal: the filed complexity every task in
/// it resolves alike from.
pub(crate) const AUTO_EFFORT_SIGNAL: &str = "auto_effort_signal";
/// A repair partition that re-pilots a task at a higher effort: `{effort,
/// from, reason}`.
pub(crate) const AUTO_EFFORT_ESCALATION: &str = "auto_effort_escalation";
/// The prepared envelope's round: `pilots`, or `repair_pilots` for the
/// prepared input the repair apply settles.
pub(crate) const PILOT_ROUND: &str = "pilot_round";
pub(crate) const FIRST_ROUND: &str = "pilots";
/// The field of an apply step's task result naming the `<crew>@<effort>` its
/// partition ran as, when the run drew an `@auto` member.
pub(crate) const PILOT_CREW_FIELD: &str = "pilot_crew";
pub(crate) const REPAIR_ROUND: &str = "repair_pilots";

/// The `activity_crew_draws` key one partition's resolved pilot crew is
/// frozen under.
pub(crate) fn partition_draw_key(round: &str, partition_index: u64) -> String {
    format!(
        "{}#{round}:{partition_index}",
        CrewPurpose::TaskPilot.config_key()
    )
}

/// The round and partition index a pilot activity's input names, or `None`
/// for an input without a partition.
pub(crate) fn input_partition(input: &Value) -> Option<(&'static str, u64)> {
    let index = input
        .get("partition_index")
        .and_then(Value::as_u64)
        .or_else(|| input.pointer("/partition/partition_index")?.as_u64())?;
    let round = if input.get("repair_attempt").and_then(Value::as_bool) == Some(true) {
        REPAIR_ROUND
    } else {
        FIRST_ROUND
    };
    Some((round, index))
}

impl OrbitRuntime {
    /// The partition key a task filed at `complexity` takes when the
    /// task-pilot pool has `@auto` members: for each such crew, the
    /// reference its bands choose for that tier. Tasks with equal keys share
    /// an effort whichever crew the run draws. `None` when the pool has no
    /// `@auto` member, which keeps the historical partitioning.
    pub(crate) fn pilot_effort_key(&self, complexity: Option<TaskComplexity>) -> Option<String> {
        let settings = self.context.settings();
        let policy = settings.auto_effort();
        let signal = pilot_signal(complexity);
        let references = settings
            .purpose_crews()
            .pool(CrewPurpose::TaskPilot)
            .iter()
            .filter_map(|entry| {
                let name = entry
                    .rsplit_once(':')
                    .map_or(entry.as_str(), |(name, _)| name);
                let crew = settings.crews().get(auto_effort_base(name)?)?;
                Some(policy.resolve(crew, &signal, None).policy)
            })
            .collect::<Vec<_>>();
        (!references.is_empty()).then(|| references.join(","))
    }

    /// The crew one pilot partition runs as when the run drew an `@auto`
    /// member (`family`): resolved against the partition's signal, or forced
    /// to its escalation effort, and frozen under the partition's key so a
    /// retried dispatch and the apply step read the same reference.
    pub(crate) fn pilot_partition_draw(
        &self,
        input: &Value,
        family: &ActivityCrewDraw,
        choice: &AutoEffortChoice,
    ) -> Option<(String, ActivityCrewDraw)> {
        let (round, index) = input_partition(input)?;
        let base = auto_effort_base(&choice.requested)?;
        let crew = self.context.settings().crews().get(base)?;
        let partition = input.get("partition").unwrap_or(&Value::Null);
        let signal = partition
            .get(AUTO_EFFORT_SIGNAL)
            .and_then(|signal| serde_json::from_value::<AutoEffortSignal>(signal.clone()).ok())
            .unwrap_or_default();
        let forced = partition
            .pointer(&format!("/{AUTO_EFFORT_ESCALATION}/effort"))
            .and_then(Value::as_str)
            .and_then(|effort| effort.parse().ok());
        let run = input
            .get("job_run_id")
            .or_else(|| input.get("run_id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let subject = crate::application::contracts::AutoEffortSubject {
            signal,
            seed: Some(format!("pilot-auto-effort:{run}:{round}:{index}")),
            forced,
        };
        let resolved = self.auto_effort_crew_choice(crew, &subject);
        let draw = ActivityCrewDraw {
            crew: resolved.resolved.clone(),
            source: family
                .source
                .with_note(&crate::application::contracts::auto_effort_note(&resolved)),
            eligible_pool: family.eligible_pool.clone(),
            auto_effort: Some(resolved),
        };
        Some((partition_draw_key(round, index), draw))
    }
}

/// The signal a pilot resolves from: the filed tier alone.
pub(crate) fn pilot_signal(complexity: Option<TaskComplexity>) -> AutoEffortSignal {
    AutoEffortSignal {
        complexity: complexity.filter(|tier| tier.is_assessed()),
        lines: None,
        files: None,
    }
}

/// The partition item fields a partition of tasks with effort key `key`
/// carries: its representative signal.
pub(crate) fn partition_signal(
    key: Option<&str>,
    complexity: Option<TaskComplexity>,
) -> Option<Value> {
    key.map(|_| json!(pilot_signal(complexity)))
}
