//! `operation.review_policy = after-landing` and the consumer that carries it
//! out [ORB-13896].
//!
//! The policy is the switch. While it is `after-landing`, the shipped
//! `delivery-code-review` delivery consumer is enabled whatever its own
//! `enabled` field says, and `operation.review_crew`, when set, is the crew of
//! every review task it mints. Nothing else performs after-landing review, so
//! [`after_landing_health`] states in one line whether that consumer can
//! actually do it on this host; `orbit doctor` fails on anything less than
//! healthy and `orbit config show` prints the same line.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_config::ReviewPolicy;
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule};
use serde::Serialize;

use super::{inspect, source::Source};
use crate::OrbitRuntime;

/// The auto-task definition `after-landing` review runs through.
pub(crate) const AFTER_LANDING_CONSUMER: &str = "delivery-code-review";

impl OrbitRuntime {
    /// Whether `operation.review_policy = after-landing` keeps `definition`
    /// enabled: it is the delivery consumer that policy runs through.
    pub fn auto_task_enabled_by_review_policy(&self, definition: &AutoTaskDefinition) -> bool {
        self.operation_policy().review_policy.value == ReviewPolicy::AfterLanding
            && definition.name == AFTER_LANDING_CONSUMER
            && matches!(definition.schedule, AutoTaskSchedule::Deliveries { .. })
    }

    /// Whether `definition` is enabled here: its own toggle, or the review
    /// policy that drives it. Every delivery evaluation, inspection and
    /// listing reads this rather than the raw field, so they cannot disagree.
    pub fn auto_task_enabled(&self, definition: &AutoTaskDefinition) -> bool {
        definition.enabled || self.auto_task_enabled_by_review_policy(definition)
    }
}

/// The crew a review task minted for `definition` must carry instead of the
/// template's: `operation.review_crew`, when the policy drives this consumer
/// and the crew is set.
pub(super) fn review_crew_override(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
) -> Option<String> {
    if !runtime.auto_task_enabled_by_review_policy(definition) {
        return None;
    }
    runtime.operation_policy().review_crew.value.clone()
}

/// Whether the `after-landing` consumer can review landed work on this host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AfterLandingHealth {
    /// The consumer definition's name.
    pub consumer: String,
    /// The definition loads and is not parked by an inactive plugin.
    pub present: bool,
    /// The consumer is enabled here (by the policy or its own toggle).
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
    /// Every reason the consumer cannot review landed work; empty when healthy.
    pub problems: Vec<String>,
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
        let branch = match (&self.branch, &self.branch_error) {
            (Some(branch), None) => format!("branch `{branch}` resolves"),
            (Some(branch), Some(_)) => format!("branch `{branch}` does not resolve"),
            (None, _) => "no branch".to_string(),
        };
        let progress = match (&self.wedged_action, &self.stall) {
            (Some(action), _) => format!("wedged on {action}"),
            (None, Some(stall)) => format!("stalled ({stall})"),
            (None, None) => "not wedged or stalled".to_string(),
        };
        let batch = match (self.last_batch_minted_at, self.last_batch_covered_at) {
            (Some(at), _) => format!("last batch minted {}", at.to_rfc3339()),
            (None, Some(at)) => format!("last batch covered {}", at.to_rfc3339()),
            (None, None) => "no batch minted yet".to_string(),
        };
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
            batch,
        ]
        .join(", ");

        if self.healthy() {
            format!("healthy: {facts}")
        } else {
            format!("unhealthy: {}; {facts}", self.problems.join("; "))
        }
    }
}

/// Scheduling states that need an operator before the consumer admits again.
const BLOCKING_STATES: &[&str] = &[
    orbit_automation::delivery::DEFINITION_CHANGED,
    "needs_attention",
    "retry_deadline_expired",
];

/// The `after-landing` consumer's health on this host, or `None` when the
/// review policy is not `after-landing`.
pub fn after_landing_health(
    runtime: &OrbitRuntime,
    now: DateTime<Utc>,
) -> Result<Option<AfterLandingHealth>, OrbitError> {
    if runtime.operation_policy().review_policy.value != ReviewPolicy::AfterLanding {
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
        problems: Vec::new(),
    };

    let Some(definition) = runtime
        .auto_task_listing(false)?
        .into_iter()
        .map(|listed| listed.definition)
        .find(|definition| definition.name == AFTER_LANDING_CONSUMER)
    else {
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

    if let Some(wedged) = inspect::wedged_delivery_consumers(runtime, now)?
        .into_iter()
        .find(|wedged| wedged.definition == AFTER_LANDING_CONSUMER)
    {
        health.problems.push(format!(
            "wedged on action {} that closed without accepted coverage evidence{}",
            wedged.action_id,
            wedged
                .reason
                .as_deref()
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default()
        ));
        health.wedged_action = Some(wedged.action_id);
    }

    let diagnostic = inspect::inspect_auto_task(runtime, &definition, now)?;
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
        health.problems.push(format!(
            "consumer state is `{}`; inspect it with `orbit auto-task recover {AFTER_LANDING_CONSUMER}`",
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
    // Without consumer state, inspection reports the branch failure as its
    // reason; that is already a problem above, and the state is the wait.
    health.state = Some(if diagnostic.state.is_none() {
        "awaiting_baseline".to_string()
    } else {
        diagnostic.reason
    });

    Ok(Some(health))
}
