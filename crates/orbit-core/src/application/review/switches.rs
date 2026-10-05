//! One read view of both automatic-review switches [ORB-13992].
//!
//! Before-PR review is `review.before_pr` (with `review.minutes` and
//! `operation.review_crew`); after-landing review is the
//! `delivery-code-review` auto-task's `enabled` flag. `orbit config show`,
//! `orbit doctor`, the dashboard and the drain probe all render this view, so
//! they report the two switches together and with the same provenance.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_config::OperationPolicy;
use serde::Serialize;
use serde_json::{Value as JsonValue, json};

use crate::OrbitRuntime;
use crate::application::automation::{
    AfterLandingHealth, after_landing_health, after_landing_switch,
};

/// Both automatic-review switches as this workspace resolves them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewSwitches {
    pub before_pr: BeforePrSwitch,
    pub after_landing: AfterLandingSwitch,
}

/// `review.before_pr`: hold PR creation for a fresh reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BeforePrSwitch {
    pub enabled: bool,
    /// Config layer that decided `review.before_pr`.
    pub source: String,
    /// `review.minutes`: the wall-clock limit for one candidate's review.
    pub minutes: u32,
    pub minutes_source: String,
    /// `operation.review_crew`, the reviewer crew.
    pub crew: Option<String>,
    pub crew_source: String,
    /// Why before-PR review cannot run here while it is on; empty when it
    /// can or is off.
    pub problems: Vec<String>,
}

/// The `delivery-code-review` auto-task: review landed deliveries in batches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AfterLandingSwitch {
    pub enabled: bool,
    /// What decided `enabled`: the auto-task flag, or the deprecated policy.
    pub source: String,
    /// When the next batch is due, when the consumer has state here.
    pub next_batch_due: Option<String>,
    /// The consumer's health on this host, when after-landing review is on.
    pub health: Option<AfterLandingHealth>,
}

impl ReviewSwitches {
    /// The before-PR line.
    pub fn before_pr_line(&self) -> String {
        let before_pr = &self.before_pr;
        let problems = if before_pr.problems.is_empty() {
            String::new()
        } else {
            format!("unhealthy: {}; ", before_pr.problems.join("; "))
        };
        format!(
            "{problems}{} (review.before_pr, {}); {} min per candidate (review.minutes, {}); crew \
             `{}` (operation.review_crew, {})",
            if before_pr.enabled { "on" } else { "off" },
            before_pr.source,
            before_pr.minutes,
            before_pr.minutes_source,
            before_pr.crew.as_deref().unwrap_or("-"),
            before_pr.crew_source,
        )
    }

    /// The after-landing line, with the consumer's health when it is on.
    pub fn after_landing_line(&self) -> String {
        let after_landing = &self.after_landing;
        let switch = format!(
            "{} ({})",
            if after_landing.enabled { "on" } else { "off" },
            after_landing.source
        );
        match &after_landing.health {
            Some(health) => format!("{switch}; {}", health.line()),
            None => match &after_landing.next_batch_due {
                Some(due) => format!("{switch}; next batch {due}"),
                None => switch,
            },
        }
    }

    /// Whether an enabled after-landing consumer cannot review landed work.
    pub fn after_landing_unhealthy(&self) -> bool {
        self.after_landing
            .health
            .as_ref()
            .is_some_and(|health| !health.healthy())
    }

    /// Whether before-PR review is on but cannot run here.
    pub fn before_pr_unhealthy(&self) -> bool {
        !self.before_pr.problems.is_empty()
    }

    /// Whether every switch that is on can run.
    pub fn healthy(&self) -> bool {
        !self.before_pr_unhealthy() && !self.after_landing_unhealthy()
    }
}

/// The JSON view `orbit config show --json` and the dashboard's Config tab
/// share: both switches, each with its rendered `line`, and `healthy`. A
/// switch that cannot be resolved reads as unhealthy with its `error`.
/// `policy` is the review config to report; a long-lived caller passes one
/// it just loaded, since the runtime's copy is the one it opened with.
pub fn review_switches_view(
    runtime: &OrbitRuntime,
    policy: &OperationPolicy,
    now: DateTime<Utc>,
) -> JsonValue {
    let switches = match review_switches_under(runtime, policy, now) {
        Ok(switches) => switches,
        Err(error) => return json!({"healthy": false, "error": format!("unknown: {error}")}),
    };
    let mut view = serde_json::to_value(&switches).unwrap_or(JsonValue::Null);
    view["before_pr"]["line"] = JsonValue::String(switches.before_pr_line());
    view["after_landing"]["line"] = JsonValue::String(switches.after_landing_line());
    view["healthy"] = JsonValue::Bool(switches.healthy());
    view
}

/// Resolve both review switches for `runtime`'s workspace at `now`.
pub fn review_switches(
    runtime: &OrbitRuntime,
    now: DateTime<Utc>,
) -> Result<ReviewSwitches, OrbitError> {
    review_switches_under(runtime, runtime.operation_policy(), now)
}

fn review_switches_under(
    runtime: &OrbitRuntime,
    policy: &OperationPolicy,
    now: DateTime<Utc>,
) -> Result<ReviewSwitches, OrbitError> {
    let mut problems = Vec::new();
    if policy.review_before_pr.value {
        match policy.review_crew.value.as_deref() {
            None => problems.push(
                "operation.review_crew is unset, so every gated delivery is refused".to_string(),
            ),
            Some(crew) => {
                if let Err(error) = runtime.resolve_crew_for_task(Some(crew), None) {
                    problems.push(format!("review crew `{crew}` does not resolve: {error}"));
                }
            }
        }
    }
    let (enabled, source) = after_landing_switch(runtime)?;
    let health = after_landing_health(runtime, now)?;
    Ok(ReviewSwitches {
        before_pr: BeforePrSwitch {
            enabled: policy.review_before_pr.value,
            source: policy.review_before_pr.source.label().to_string(),
            minutes: policy.review_minutes.value,
            minutes_source: policy.review_minutes.source.label().to_string(),
            crew: policy.review_crew.value.clone(),
            crew_source: policy.review_crew.source.label().to_string(),
            problems,
        },
        after_landing: AfterLandingSwitch {
            enabled,
            source: source.label(),
            next_batch_due: health
                .as_ref()
                .and_then(|health| health.next_batch_due.clone()),
            health,
        },
    })
}
