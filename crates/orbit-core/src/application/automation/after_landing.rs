//! After-landing review: the `delivery-code-review` delivery consumer
//! [ORB-13896] [ORB-13992].
//!
//! The auto-task's own `enabled` flag is the switch. `operation.review_crew`,
//! when set, is the crew of every review task it mints. Nothing else performs
//! after-landing review, so [`after_landing_health`] states in one line
//! whether that consumer can actually do it on this host and when its next
//! batch is due; `orbit doctor` fails on anything less than healthy and the
//! review read views print the same line.
//!
//! The retired `operation.review_policy = "after-landing"` still enables the
//! consumer for one release while its definition was never configured by an
//! operator — still the shipped seed, last written by `system` — so a
//! workspace that relied on the policy keeps its reviews until the operator
//! toggles the auto-task themselves.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_config::OperationLayerSource;
use orbit_types::workflow::automation::{AutomationState, CoverageClass, DeliveryTrigger};
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule};
use serde::Serialize;

use super::{
    inspect,
    source::{RemoteObservation, Source},
};
use crate::OrbitRuntime;

/// The auto-task definition after-landing review runs through.
pub(crate) const AFTER_LANDING_CONSUMER: &str = "delivery-code-review";

/// The actor the shipped seed records; any other writer configured the
/// definition explicitly.
const SEED_ACTOR: &str = "system";

impl OrbitRuntime {
    /// Whether `definition` is enabled here: its own toggle or, for one
    /// release, the retired `after-landing` policy value on a consumer no
    /// operator has configured. Every delivery evaluation, inspection and
    /// listing reads this rather than the raw field, so they cannot disagree.
    pub fn auto_task_enabled(&self, definition: &AutoTaskDefinition) -> bool {
        definition.enabled || self.legacy_after_landing_enables(definition).is_some()
    }

    /// The layer whose retired `operation.review_policy = "after-landing"`
    /// keeps `definition` enabled, when it does.
    fn legacy_after_landing_enables(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Option<OperationLayerSource> {
        let layer = self.operation_policy().legacy_after_landing?;
        let consumer = definition.name == AFTER_LANDING_CONSUMER
            && matches!(definition.schedule, AutoTaskSchedule::Deliveries { .. });
        let configured = definition
            .updated_by
            .as_deref()
            .is_some_and(|actor| actor != SEED_ACTOR);
        (consumer && !configured).then_some(layer)
    }
}

/// The crew a review task minted for `definition` must carry instead of the
/// template's: `operation.review_crew`, when this is the after-landing
/// consumer and the crew is set.
pub(super) fn review_crew_override(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
) -> Option<String> {
    if definition.name != AFTER_LANDING_CONSUMER {
        return None;
    }
    runtime.operation_policy().review_crew.value.clone()
}

/// What turned after-landing review on or left it off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterLandingSource {
    /// The auto-task's own `enabled` flag.
    AutoTask,
    /// The retired `operation.review_policy = "after-landing"` in the named
    /// layer, on a consumer no operator has configured.
    LegacyReviewPolicy(OperationLayerSource),
    /// No `delivery-code-review` definition loads here.
    Missing,
}

impl AfterLandingSource {
    /// The operator-facing provenance label.
    pub fn label(self) -> String {
        match self {
            AfterLandingSource::AutoTask => format!("auto-task {AFTER_LANDING_CONSUMER}"),
            AfterLandingSource::LegacyReviewPolicy(layer) => format!(
                "deprecated operation.review_policy = \"after-landing\" ({}); run `orbit \
                 auto-task toggle {AFTER_LANDING_CONSUMER} on` and delete the key",
                layer.label()
            ),
            AfterLandingSource::Missing => format!("auto-task {AFTER_LANDING_CONSUMER} missing"),
        }
    }
}

/// Whether after-landing review is on here, and why.
pub fn after_landing_switch(
    runtime: &OrbitRuntime,
) -> Result<(bool, AfterLandingSource), OrbitError> {
    let Some(definition) = after_landing_definition(runtime)? else {
        return Ok((false, AfterLandingSource::Missing));
    };
    if definition.enabled {
        return Ok((true, AfterLandingSource::AutoTask));
    }
    Ok(match runtime.legacy_after_landing_enables(&definition) {
        Some(layer) => (true, AfterLandingSource::LegacyReviewPolicy(layer)),
        None => (false, AfterLandingSource::AutoTask),
    })
}

fn after_landing_definition(
    runtime: &OrbitRuntime,
) -> Result<Option<AutoTaskDefinition>, OrbitError> {
    Ok(runtime
        .auto_task_listing(false)?
        .into_iter()
        .map(|listed| listed.definition)
        .find(|definition| definition.name == AFTER_LANDING_CONSUMER))
}

/// Whether the `after-landing` consumer can review landed work on this host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AfterLandingHealth {
    /// The consumer definition's name.
    pub consumer: String,
    /// The definition loads and is not parked by an inactive plugin.
    pub present: bool,
    /// The consumer is enabled here.
    pub enabled: bool,
    /// This host is the consumer's resolved owner.
    pub owned_here: bool,
    /// The machine the consumer's ownership resolves to, if any.
    pub owner_machine: Option<String>,
    /// The branch the consumer watches.
    pub branch: Option<String>,
    /// Why that branch does not resolve, when it does not.
    pub branch_error: Option<String>,
    /// The admitted action the consumer is wedged on, if any.
    pub wedged_action: Option<String>,
    /// The recorded stall that suspends the consumer, if any.
    pub stall: Option<String>,
    /// The consumer's scheduling state as `orbit auto-task show` reports it.
    pub state: Option<String>,
    /// The crew its review tasks are minted with.
    pub crew: Option<String>,
    /// Why that crew cannot be resolved on this host, when it cannot.
    pub crew_error: Option<String>,
    /// When the consumer's open batch was frozen and minted.
    pub last_batch_minted_at: Option<DateTime<Utc>>,
    /// When the consumer last accepted coverage evidence for a batch.
    pub last_batch_covered_at: Option<DateTime<Utc>>,
    /// When the next batch is due, as one phrase: in flight, due now, due by
    /// a time, or waiting for landed deliveries.
    pub next_batch_due: Option<String>,
    /// The instant the oldest pending delivery makes a batch due, when the
    /// threshold is not reached first.
    pub next_batch_due_at: Option<DateTime<Utc>>,
    /// The commit the consumer has observed, when it has a cursor and origin
    /// is configured. Absent for a repository with no remote, so this report
    /// stays the shape it had before remote observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_commit: Option<String>,
    /// `refs/remotes/origin/<branch>`, when that ref exists. Doctor reads it
    /// and does not fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_head: Option<String>,
    /// First-parent commits the observed cursor has not reached. `Some(0)`
    /// means the cursor matches the remote-tracking head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commits_behind_remote: Option<u64>,
    /// The observed commit is not an ancestor of the remote-tracking head.
    #[serde(default, skip_serializing_if = "is_false")]
    pub remote_diverged: bool,
    /// Origin is configured and `refs/remotes/origin/<branch>` is missing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub remote_unfetched: bool,
    /// Every reason the consumer cannot review landed work; empty when healthy.
    pub problems: Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl AfterLandingHealth {
    /// Nothing stops the consumer from reviewing landed work.
    pub fn healthy(&self) -> bool {
        self.problems.is_empty()
    }

    /// The single operator-facing health line.
    pub fn line(&self) -> String {
        if !self.present {
            return format!(
                "unhealthy: consumer `{}` {}",
                self.consumer,
                self.problems.join("; ")
            );
        }
        let owner = match (&self.owner_machine, self.owned_here) {
            (Some(owner), true) => format!("owned here (`{owner}`)"),
            (Some(owner), false) => format!("owned by `{owner}`"),
            (None, _) => "no owner".to_string(),
        };
        let branch = self.branch_phrase();
        let progress = match (&self.wedged_action, &self.stall) {
            (Some(_), _) => "wedged on a closed batch task".to_string(),
            (None, Some(stall)) => format!("stalled ({stall})"),
            (None, None) => "not wedged or stalled".to_string(),
        };
        let minted = self
            .last_batch_minted_at
            .map(|at| format!("last batch minted at {}", at.to_rfc3339()))
            .unwrap_or_else(|| "no batch minted yet".to_string());
        let covered = self
            .last_batch_covered_at
            .map(|at| format!("last batch covered at {}", at.to_rfc3339()))
            .unwrap_or_else(|| "no batch covered yet".to_string());
        let facts = [
            format!(
                "consumer `{}` {}",
                self.consumer,
                if self.enabled { "enabled" } else { "disabled" }
            ),
            owner,
            progress,
            branch,
            format!("crew `{}`", self.crew.as_deref().unwrap_or("-")),
            format!("state {}", self.state.as_deref().unwrap_or("-")),
            minted,
            covered,
            format!(
                "next batch {}",
                self.next_batch_due.as_deref().unwrap_or("unknown")
            ),
        ]
        .join(", ");

        if self.healthy() {
            format!("healthy: {facts}")
        } else {
            format!("unhealthy: {}; {facts}", self.problems.join("; "))
        }
    }

    fn branch_phrase(&self) -> String {
        let base = match (&self.branch, &self.branch_error) {
            (Some(branch), None) => format!("branch `{branch}` resolves"),
            (Some(branch), Some(_)) => format!("branch `{branch}` does not resolve"),
            (None, _) => "no branch".to_string(),
        };
        match self.remote_clause() {
            Some(clause) => format!("{base}; {clause}"),
            None => base,
        }
    }

    /// How the observed cursor relates to the remote-tracking head. Absent
    /// when the repository has no origin, so the branch wording stays put.
    fn remote_clause(&self) -> Option<String> {
        let branch = self.branch.as_deref()?;
        if self.remote_unfetched {
            return Some(format!("origin/{branch} has no remote-tracking ref"));
        }
        let remote = self.remote_head.as_deref()?;
        if self.remote_diverged {
            return Some(format!(
                "is not an ancestor of origin/{branch} ({remote}) (history_diverged)"
            ));
        }
        match self.commits_behind_remote {
            Some(0) => Some(format!("matches origin/{branch} ({remote})")),
            Some(behind) => Some(format!(
                "trails origin/{branch} ({remote}) by {behind} commits"
            )),
            None => Some(format!(
                "origin/{branch} is at {remote}; the consumer has not observed a commit yet"
            )),
        }
    }
}

/// Scheduling states that need an operator before the consumer admits again.
const BLOCKING_STATES: &[&str] = &[
    orbit_automation::delivery::DEFINITION_CHANGED,
    "needs_attention",
    "retry_deadline_expired",
];

/// The after-landing consumer's health on this host, or `None` when
/// after-landing review is off.
pub fn after_landing_health(
    runtime: &OrbitRuntime,
    now: DateTime<Utc>,
) -> Result<Option<AfterLandingHealth>, OrbitError> {
    let definition = after_landing_definition(runtime)?;
    let requested = match &definition {
        Some(definition) => runtime.auto_task_enabled(definition),
        // A missing consumer is a problem only for a workspace that asked
        // for after-landing review through the retired policy value.
        None => runtime.operation_policy().legacy_after_landing.is_some(),
    };
    if !requested {
        return Ok(None);
    }

    let mut health = AfterLandingHealth {
        consumer: AFTER_LANDING_CONSUMER.to_string(),
        present: false,
        enabled: false,
        owned_here: false,
        owner_machine: None,
        branch: None,
        branch_error: None,
        wedged_action: None,
        stall: None,
        state: None,
        crew: None,
        crew_error: None,
        last_batch_minted_at: None,
        last_batch_covered_at: None,
        next_batch_due: None,
        next_batch_due_at: None,
        observed_commit: None,
        remote_head: None,
        commits_behind_remote: None,
        remote_diverged: false,
        remote_unfetched: false,
        problems: Vec::new(),
    };

    let Some(definition) = definition else {
        health.problems.push(format!(
            "is missing or does not load, so no landed delivery is reviewed; reinstate it with \
             `orbit auto-task restore {AFTER_LANDING_CONSUMER}`"
        ));
        return Ok(Some(health));
    };
    let AutoTaskSchedule::Deliveries {
        deliveries_landed: declared,
    } = &definition.schedule
    else {
        health.problems.push(
            "is not a `deliveries_landed` definition, so it never reviews landed deliveries"
                .to_string(),
        );
        return Ok(Some(health));
    };
    health.present = true;
    health.enabled = runtime.auto_task_enabled(&definition);
    health.branch = Some(declared.branch.clone());
    if declared.coverage != CoverageClass::LandedCodeReviewV1 {
        health.problems.push(format!(
            "uses `{}` coverage instead of `landed_code_review_v1`, so it does not certify code review",
            declared.coverage
        ));
    }

    let ownership = super::ownership::resolve(runtime, declared.owner_machine.as_deref());
    health.owned_here = ownership.owned_here;
    health.owner_machine = ownership.owner_machine.clone();
    if let Some(refusal) = inspect::delivery_ownership_refusal(runtime, &definition) {
        health.problems.push(refusal.mismatch());
    }

    health.branch_error =
        inspect::branch_unavailable(&Source::new(&runtime.paths().repo_root), &declared.branch);
    if let Some(error) = &health.branch_error {
        health.problems.push(format!(
            "branch `{}` does not resolve: {error}",
            declared.branch
        ));
    }

    health.crew = review_crew_override(runtime, &definition).or(definition.template.crew.clone());
    if let Some(crew) = &health.crew
        && let Err(error) = runtime.resolve_crew_for_task(Some(crew), None)
    {
        health.crew_error = Some(error.to_string());
        health
            .problems
            .push(format!("review crew `{crew}` does not resolve: {error}"));
    }

    // Consumer state is keyed by this host's machine identity; without one
    // the ownership problem above already says why nothing is admitted.
    if runtime.automation_machine_identity().is_none() {
        return Ok(Some(health));
    }

    let diagnostic = inspect::inspect_auto_task(runtime, &definition, now)?;
    let configured_epoch = super::ownership::auto_task_epoch(
        &definition,
        &super::ownership::with_resolved_owner(declared, &ownership),
    )?;
    let definition_changed = diagnostic
        .state
        .as_ref()
        .is_some_and(|state| state.epoch != configured_epoch);
    if let Some(wedged) = inspect::wedged_delivery_consumers(runtime, now)?
        .into_iter()
        .find(|wedged| wedged.definition == AFTER_LANDING_CONSUMER)
    {
        health.problems.push(format!(
            "wedged on a batch task ({}) that closed without accepted coverage evidence{}; recover with `orbit auto-task recover {AFTER_LANDING_CONSUMER} {}--reissue-action --reason \"re-examine the unpaid batch\"`",
            wedged.terminal_status,
            wedged
                .reason
                .as_deref()
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default(),
            if definition_changed { "--adopt-settings " } else { "" }
        ));
        health.wedged_action = Some(wedged.action_id);
    }

    if let Some(stall) = diagnostic
        .state
        .as_ref()
        .and_then(|state| state.stall.as_ref())
    {
        health.problems.push(format!(
            "stalled since {} ({})",
            stall.since.to_rfc3339(),
            stall.reason
        ));
        health.stall = Some(stall.reason.clone());
    }
    if BLOCKING_STATES.contains(&diagnostic.reason.as_str()) {
        let refused = if diagnostic.refusals.is_empty() {
            String::new()
        } else {
            format!(
                " (not adopted automatically: {})",
                diagnostic.refusals.join(", ")
            )
        };
        health.problems.push(format!(
            "consumer state is `{}`{refused}; inspect it with `orbit auto-task recover {AFTER_LANDING_CONSUMER}`",
            diagnostic.reason
        ));
    }
    health.last_batch_minted_at = diagnostic
        .state
        .as_ref()
        .and_then(|state| state.active.as_ref())
        .map(|active| active.batch.created_at);
    health.last_batch_covered_at = diagnostic
        .receipts
        .iter()
        .map(|receipt| receipt.accepted_at)
        .max();
    if health.last_batch_minted_at.is_none()
        && let Some(receipt) = diagnostic
            .receipts
            .iter()
            .max_by_key(|receipt| receipt.accepted_at)
        && let Ok(task) = runtime.get_task(&receipt.action_id)
    {
        health.last_batch_minted_at = Some(task.created_at);
    }
    if let Some(state) = &diagnostic.state {
        let (due, due_at) = next_batch_due(state, state.trigger.as_ref().unwrap_or(declared), now);
        health.next_batch_due = Some(due);
        health.next_batch_due_at = due_at;
    }
    // Without consumer state, inspection reports the branch failure as its
    // reason; that is already a problem above, and the state is the wait.
    health.state = Some(if diagnostic.state.is_none() {
        "awaiting_baseline".to_string()
    } else {
        diagnostic.reason
    });
    apply_remote_trail(
        &mut health,
        &runtime.paths().repo_root,
        &declared.branch,
        declared.max_wait_minutes,
        diagnostic
            .state
            .as_ref()
            .map(|state| state.observed.commit.as_str()),
        now,
    );

    Ok(Some(health))
}

/// Compare the consumer's cursor with the remote-tracking head already in
/// the checkout. This does not fetch: a pass that cannot reach origin defers
/// on its own, and doctor has to stay read-only.
fn apply_remote_trail(
    health: &mut AfterLandingHealth,
    root: &std::path::Path,
    branch: &str,
    max_wait_minutes: u32,
    observed: Option<&str>,
    now: DateTime<Utc>,
) {
    let observation = match Source::new(root).remote_observation(branch, observed) {
        Ok(observation) => observation,
        Err(error) => {
            health.problems.push(format!(
                "could not compare the observed source with origin/{branch}: {error}"
            ));
            return;
        }
    };
    let Some(observation) = observation else {
        return;
    };
    record_remote_observation(health, &observation);
    if observation.diverged {
        health.problems.push(format!(
            "observed source is not an ancestor of origin/{branch} (history_diverged)"
        ));
    }
    if observation.unfetched && observation.observed.is_some() {
        health.problems.push(format!(
            "origin/{branch} has no remote-tracking ref, so landed deliveries cannot be observed"
        ));
    }
    if let (Some(behind), Some(epoch)) = (observation.behind, observation.oldest_unobserved_epoch)
        && behind > 0
        && let Some(landed) = DateTime::from_timestamp(epoch, 0)
        && now.signed_duration_since(landed) >= Duration::minutes(i64::from(max_wait_minutes))
    {
        health.problems.push(format!(
            "observed source trails origin/{branch} by {behind} commits past max_wait_minutes ({max_wait_minutes})"
        ));
    }
}

fn record_remote_observation(health: &mut AfterLandingHealth, observation: &RemoteObservation) {
    health.observed_commit = observation.observed.clone();
    health.remote_head = observation.remote_head.clone();
    health.commits_behind_remote = observation.behind;
    health.remote_diverged = observation.diverged;
    health.remote_unfetched = observation.unfetched;
}

/// When the consumer's next batch is due, mirroring the delivery evaluator:
/// a batch is due once `threshold` deliveries are pending or the oldest has
/// waited `max_wait_minutes`; nothing new is due while a batch is in flight.
fn next_batch_due(
    state: &AutomationState,
    trigger: &DeliveryTrigger,
    now: DateTime<Utc>,
) -> (String, Option<DateTime<Utc>>) {
    let pending = state.pending.len();
    let threshold = trigger.threshold;
    if state.active.is_some() {
        return (
            format!("after the batch in flight ({pending} pending)"),
            None,
        );
    }
    let Some(oldest) = state.pending.first() else {
        return (
            format!("after {threshold} landed deliveries (none pending)"),
            None,
        );
    };
    let due_at = oldest.landed_at + Duration::minutes(i64::from(trigger.max_wait_minutes));
    if pending >= threshold || due_at <= now {
        return (
            format!("due now ({pending}/{threshold} pending)"),
            Some(due_at.min(now)),
        );
    }
    (
        format!(
            "due by {} or at {threshold} landed deliveries ({pending} pending)",
            due_at.to_rfc3339()
        ),
        Some(due_at),
    )
}
