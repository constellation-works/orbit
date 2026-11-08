//! `<crew>@auto` pool entries and the per-crew effort bands that resolve them
//! (`workflow.auto_effort.*`).
//!
//! A pool entry written `opus@auto` (or `opus@auto:100` in a weighted pool)
//! runs `opus` at the effort its bands choose for the draw's subject: the
//! first band whose every condition the subject's signal meets. No match, or
//! no signal, falls back to the crew's configured effort. A fixed share of
//! draws ([`AUTO_EFFORT_EXPLORE_SHARE`]) runs another effort the bands name,
//! so each side of a cutover keeps getting data. Bands are workspace policy;
//! nothing in core names a cutover.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::identity::{
    AutoEffortBasis, AutoEffortChoice, AutoEffortSignal, CREW_EFFORT_SEPARATOR, Crew,
    ReasoningEffort, auto_effort_base, auto_effort_reference, resolve_crew,
};
use orbit_types::task::TaskComplexity;
use serde::{Deserialize, Serialize};

/// `workflow.auto_effort.bands`, keyed by crew.
pub const AUTO_EFFORT_BANDS_KEY: &str = "workflow.auto_effort.bands";
/// The former `workflow.auto_effort.explore_share` default, fixed when the
/// key was removed [ORB-22808]: one draw in ten runs another effort the
/// bands name.
pub const AUTO_EFFORT_EXPLORE_SHARE: f64 = 0.1;
/// Resolution of the exploration roll, in parts per million.
const EXPLORE_SCALE: u64 = 1_000_000;

/// The pools that admit `<crew>@auto`: the complexity and type pools
/// (implementers), the task-pilot purpose pool and the two review pools.
/// Every other pool refuses it.
pub const AUTO_EFFORT_POOLS: &str = "workflow.<tier>_complexity_crews, \
     workflow.type_complexity_crews, workflow.purpose_crews.task_pilot, \
     workflow.purpose_crews.review and workflow.purpose_crews.pre_landing_review";

/// One band: the effort a draw runs at when its signal meets every condition
/// the band sets. A band sets at least one condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoEffortBand {
    /// The effort this band runs the crew at.
    pub effort: ReasoningEffort,
    /// Tiers this band covers: the filed tier for a task pilot, the assessed
    /// tier for an implementer, the settled tier for a reviewer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub complexity: Vec<TaskComplexity>,
    /// Matches a reviewable diff of fewer lines. Only reviewers have a line
    /// count at admission, so an implementer or pilot never meets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below_lines: Option<u64>,
    /// Matches fewer files: the declared context-file targets for an
    /// implementer, the changed files for a reviewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below_files: Option<u64>,
}

impl AutoEffortBand {
    /// Whether `signal` meets every condition this band sets.
    fn matches(&self, signal: &AutoEffortSignal) -> bool {
        let complexity = self.complexity.is_empty()
            || signal
                .complexity
                .is_some_and(|tier| self.complexity.contains(&tier));
        let lines = self
            .below_lines
            .is_none_or(|below| signal.lines.is_some_and(|lines| lines < below));
        let files = self
            .below_files
            .is_none_or(|below| signal.files.is_some_and(|files| files < below));
        complexity && lines && files
    }

    fn has_condition(&self) -> bool {
        !self.complexity.is_empty() || self.below_lines.is_some() || self.below_files.is_some()
    }
}

/// The admitted `workflow.auto_effort.bands` policy and its exploration share.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AutoEffortPolicy {
    bands: BTreeMap<String, Vec<AutoEffortBand>>,
    explore_share: f64,
}

impl AutoEffortPolicy {
    /// The policy over admitted bands and an exploration share
    /// ([`AUTO_EFFORT_EXPLORE_SHARE`] for configured runtimes).
    pub fn new(bands: BTreeMap<String, Vec<AutoEffortBand>>, explore_share: f64) -> Self {
        Self {
            bands,
            explore_share,
        }
    }

    /// The bands configured for `crew`, by its `[crews.<name>]` table.
    pub fn bands(&self, crew: &str) -> Option<&[AutoEffortBand]> {
        self.bands
            .get(orbit_types::identity::crew_reference_base(crew))
            .map(Vec::as_slice)
            .filter(|bands| !bands.is_empty())
    }

    /// The exploration share.
    pub fn explore_share(&self) -> f64 {
        self.explore_share
    }

    /// The effort the bands choose for `signal`, before exploration, with
    /// the index of the band that chose it; `None` when no band matches or
    /// there is no signal, which falls back to the crew's configured effort.
    pub fn policy_effort(
        &self,
        crew: &Crew,
        signal: &AutoEffortSignal,
    ) -> Option<(usize, ReasoningEffort)> {
        if signal.is_empty() {
            return None;
        }
        self.bands(crew.base_name())?
            .iter()
            .enumerate()
            .find(|(_, band)| band.matches(signal))
            .map(|(index, band)| (index, band.effort))
    }

    /// Resolve `crew` (the `[crews.<name>]` entry a `<crew>@auto` member
    /// names) for one draw. `explore_ticket` is the draw's uniform roll; a
    /// draw without one (no subject to seed it from) never explores.
    pub fn resolve(
        &self,
        crew: &Crew,
        signal: &AutoEffortSignal,
        explore_ticket: Option<u64>,
    ) -> AutoEffortChoice {
        let base = crew.base_name();
        let (basis, band, policy_effort) = match self.policy_effort(crew, signal) {
            Some((index, effort)) => (AutoEffortBasis::Band, Some(index), Some(effort)),
            None => (AutoEffortBasis::Fallback, None, crew.assignment.effort),
        };
        let policy = effort_reference(base, policy_effort);
        let explored_effort = explore_ticket.and_then(|ticket| {
            let share = (self.explore_share * EXPLORE_SCALE as f64).round() as u64;
            if ticket % EXPLORE_SCALE >= share {
                return None;
            }
            let others = self
                .named_efforts(crew)
                .into_iter()
                .filter(|effort| *effort != policy_effort)
                .collect::<Vec<_>>();
            (!others.is_empty())
                .then(|| others[((ticket / EXPLORE_SCALE) % others.len() as u64) as usize])
        });
        let resolved =
            explored_effort.map_or_else(|| policy.clone(), |effort| effort_reference(base, effort));
        AutoEffortChoice {
            requested: auto_effort_reference(base),
            explored: explored_effort.is_some(),
            resolved,
            policy,
            basis,
            band,
            signal: signal.clone(),
        }
    }

    /// The choice a task-pilot escalation runs: `crew` at `effort`, for the
    /// same signal.
    pub fn escalated(
        &self,
        crew: &Crew,
        signal: &AutoEffortSignal,
        effort: ReasoningEffort,
    ) -> AutoEffortChoice {
        let base = crew.base_name();
        let resolved = effort_reference(base, Some(effort));
        AutoEffortChoice {
            requested: auto_effort_reference(base),
            policy: resolved.clone(),
            resolved,
            basis: AutoEffortBasis::Escalation,
            band: None,
            explored: false,
            signal: signal.clone(),
        }
    }

    /// The higher effort a task pilot that ran as `piloted` must be re-run
    /// at, or `None`.
    ///
    /// A pilot escalates once, and only from an `@auto` resolution: when its
    /// assessment raised the task above its `filed` tier to one whose band
    /// maps to a higher effort than it ran at, or when it reported confidence
    /// `medium` or `low`, in which case the target is the highest effort the
    /// crew's bands name. A crew with no configured effort counts as below
    /// every band effort.
    pub fn pilot_escalation(
        &self,
        crew: &Crew,
        piloted: &AutoEffortChoice,
        filed: Option<TaskComplexity>,
        assessed: Option<TaskComplexity>,
        confidence: Option<&str>,
    ) -> Option<ReasoningEffort> {
        if piloted.basis == AutoEffortBasis::Escalation {
            return None;
        }
        let bands = self.bands(crew.base_name())?;
        let ran = reference_effort(&piloted.resolved);
        let raised = assessed
            .filter(|tier| tier.is_assessed())
            .filter(|tier| tier_rank(*tier) > filed.map_or(0, tier_rank))
            .and_then(|tier| {
                let signal = AutoEffortSignal {
                    complexity: Some(tier),
                    ..AutoEffortSignal::default()
                };
                self.policy_effort(crew, &signal).map(|(_, effort)| effort)
            });
        let unsure = matches!(confidence.map(str::trim), Some("medium" | "low"))
            .then(|| bands.iter().map(|band| band.effort).max())
            .flatten();
        let target = raised.max(unsure)?;
        (ran.is_none_or(|ran| target > ran)).then_some(target)
    }

    /// Every effort `crew` can resolve to: each band's, and the crew's
    /// configured effort (`None` when it names none), in ascending order.
    fn named_efforts(&self, crew: &Crew) -> Vec<Option<ReasoningEffort>> {
        let mut efforts = self
            .bands(crew.base_name())
            .unwrap_or_default()
            .iter()
            .map(|band| Some(band.effort))
            .chain([crew.assignment.effort])
            .collect::<Vec<_>>();
        efforts.sort();
        efforts.dedup();
        efforts
    }
}

/// `<crew>@<effort>`, or the bare crew when no effort applies.
fn effort_reference(base: &str, effort: Option<ReasoningEffort>) -> String {
    match effort {
        Some(effort) => format!("{base}{CREW_EFFORT_SEPARATOR}{effort}"),
        None => base.to_string(),
    }
}

/// The effort a resolved reference names, if any.
fn reference_effort(reference: &str) -> Option<ReasoningEffort> {
    reference
        .split_once(CREW_EFFORT_SEPARATOR)
        .and_then(|(_, effort)| effort.parse().ok())
}

fn tier_rank(tier: TaskComplexity) -> u8 {
    match tier {
        TaskComplexity::Unassessed => 0,
        TaskComplexity::Low => 1,
        TaskComplexity::Medium => 2,
        TaskComplexity::Hard => 3,
        TaskComplexity::XHard => 4,
    }
}

/// Admit `workflow.auto_effort.bands` against the crew registry: each key a
/// defined crew (not a `<crew>@<effort>` reference), each list non-empty,
/// each band with a condition, assessed tiers, positive bounds and an
/// effort that crew's provider and model accept.
pub(crate) fn admit_auto_effort_bands(
    bands: &BTreeMap<String, Vec<AutoEffortBand>>,
    crews: &BTreeMap<String, Crew>,
) -> Result<(), OrbitError> {
    for (crew, list) in bands {
        let setting = format!("{AUTO_EFFORT_BANDS_KEY}.{crew}");
        if crew.contains(CREW_EFFORT_SEPARATOR) || !crews.contains_key(crew) {
            let known = crews.keys().cloned().collect::<Vec<_>>().join(", ");
            return Err(OrbitError::InvalidInput(format!(
                "{setting} names no crew defined in [crews.*]; bands key on the crew's own name \
                 (known: {known})"
            )));
        }
        if list.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "{setting} must list at least one band; remove the key to drop {crew}'s bands"
            )));
        }
        for (index, band) in list.iter().enumerate() {
            let label = format!("{setting} band {}", index + 1);
            if !band.has_condition() {
                return Err(OrbitError::InvalidInput(format!(
                    "{label} sets no condition; give it `complexity`, `below_lines` or \
                     `below_files` (a draw no band matches runs the crew's configured effort)"
                )));
            }
            if band.complexity.iter().any(|tier| !tier.is_assessed()) {
                return Err(OrbitError::InvalidInput(format!(
                    "{label} names tier `unassessed`; complexity lists low, medium, hard or xhard"
                )));
            }
            if band.below_lines == Some(0) || band.below_files == Some(0) {
                return Err(OrbitError::InvalidInput(format!(
                    "{label} sets a bound of 0, which nothing is below"
                )));
            }
            resolve_crew(
                &format!("{crew}{CREW_EFFORT_SEPARATOR}{}", band.effort),
                crews,
            )
            .map_err(|error| OrbitError::InvalidInput(format!("{label}: {error}")))?;
        }
    }
    Ok(())
}

/// Refuse a canonical pool's `<crew>@auto` entries that `setting` cannot
/// hold: every one when `bands` is `None` (a pool outside
/// [`AUTO_EFFORT_POOLS`]), else one whose crew has no bands.
pub(crate) fn check_auto_effort_entries(
    entries: &[String],
    setting: &str,
    bands: Option<&BTreeMap<String, Vec<AutoEffortBand>>>,
) -> Result<(), OrbitError> {
    for entry in entries {
        let name = entry
            .rsplit_once(crate::crew_pools::WEIGHT_SEPARATOR)
            .map_or(entry.as_str(), |(name, _)| name);
        let Some(base) = auto_effort_base(name) else {
            continue;
        };
        match bands {
            None => {
                return Err(OrbitError::InvalidInput(format!(
                    "{setting} names '{name}'; only {AUTO_EFFORT_POOLS} accept `@auto`"
                )));
            }
            Some(bands) if bands.get(base).is_none_or(Vec::is_empty) => {
                return Err(OrbitError::InvalidInput(format!(
                    "{setting} names '{name}', but crew '{base}' has no bands; configure \
                     {AUTO_EFFORT_BANDS_KEY}.{base} or name an effort ('{base}@high')"
                )));
            }
            Some(_) => {}
        }
    }
    Ok(())
}
