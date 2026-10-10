//! One read view of every automatic-review switch [ORB-13992].
//!
//! Before-PR review is `review.before_pr` and before-landing review is
//! `review.before_landing` [ORB-14849], both with `review.minutes` and
//! `operation.review_crew`; after-landing review is the
//! `delivery-code-review` auto-task's `enabled` flag. `orbit config show`,
//! `orbit doctor`, the dashboard and the drain probe all render this view, so
//! they report the switches together and with the same provenance.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_config::OperationPolicy;
use orbit_types::workflow::ShipMode;
use serde::Serialize;
use serde_json::{Value as JsonValue, json};

use super::{local_route_before_landing_conflict, local_route_before_pr_conflict};
use crate::OrbitRuntime;
use crate::application::automation::{
    AfterLandingHealth, after_landing_health, after_landing_switch,
};

/// Every automatic-review switch as this workspace resolves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewSwitches {
    pub before_pr: BeforePrSwitch,
    pub before_landing: BeforeLandingSwitch,
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
    /// `review.before_pr` is on and automatic delivery uses the local-only
    /// route [ORB-14168]. The problem text already says so; doctor uses this
    /// to name the ship-mode remedy rather than the crew remedy. Omitted from
    /// JSON because `problems` and `healthy` carry it.
    #[serde(skip)]
    pub local_route_incompatible: bool,
}

/// `review.before_landing`: review the open PR before it lands. It shares
/// `review.minutes` and `operation.review_crew` with [`BeforePrSwitch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BeforeLandingSwitch {
    pub enabled: bool,
    /// Config layer that decided `review.before_landing`.
    pub source: String,
    /// Why before-landing review cannot run here while it is on; empty when
    /// it can or is off.
    pub problems: Vec<String>,
    /// `review.before_landing` is on and automatic delivery uses the
    /// local-only route, which opens no PR. Omitted from JSON because
    /// `problems` and `healthy` carry it.
    #[serde(skip)]
    pub local_route_incompatible: bool,
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

    /// The before-landing line; minutes and crew are the before-PR line's.
    pub fn before_landing_line(&self) -> String {
        let before_landing = &self.before_landing;
        let problems = if before_landing.problems.is_empty() {
            String::new()
        } else {
            format!("unhealthy: {}; ", before_landing.problems.join("; "))
        };
        format!(
            "{problems}{} (review.before_landing, {}); shares review.minutes and \
             operation.review_crew with before-PR review",
            if before_landing.enabled { "on" } else { "off" },
            before_landing.source,
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

    /// Whether before-landing review is on but cannot run here.
    pub fn before_landing_unhealthy(&self) -> bool {
        !self.before_landing.problems.is_empty()
    }

    /// Whether every switch that is on can run.
    pub fn healthy(&self) -> bool {
        !self.before_pr_unhealthy()
            && !self.before_landing_unhealthy()
            && !self.after_landing_unhealthy()
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
    view["before_landing"]["line"] = JsonValue::String(switches.before_landing_line());
    view["after_landing"]["line"] = JsonValue::String(switches.after_landing_line());
    view["healthy"] = JsonValue::Bool(switches.healthy());
    view
}

/// Resolve every review switch for `runtime`'s workspace at `now`.
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
    // The same mode the drain delivers in. A registered PR workspace is
    // unaffected; a local-only one cannot run either switch [ORB-14168].
    let local_route = runtime.automatic_delivery_ship_mode() == ShipMode::Local;
    let before_pr_on = policy.review_before_pr.value;
    let before_landing_on = policy.review_before_landing.value;
    let mut before_pr_problems = Vec::new();
    if before_pr_on && local_route {
        before_pr_problems.push(local_route_before_pr_conflict(
            policy.review_before_pr.source.label(),
        ));
    }
    let mut before_landing_problems = Vec::new();
    if before_landing_on && local_route {
        before_landing_problems.push(local_route_before_landing_conflict(
            policy.review_before_landing.source.label(),
        ));
    }
    let crew_problems = crew_problems(runtime, policy);
    if before_pr_on {
        before_pr_problems.extend(crew_problems.iter().cloned());
    }
    if before_landing_on {
        before_landing_problems.extend(crew_problems);
    }
    let (enabled, source) = after_landing_switch(runtime)?;
    let health = after_landing_health(runtime, now)?;
    Ok(ReviewSwitches {
        before_pr: BeforePrSwitch {
            enabled: before_pr_on,
            source: policy.review_before_pr.source.label().to_string(),
            minutes: policy.review_minutes.value,
            minutes_source: policy.review_minutes.source.label().to_string(),
            crew: policy.review_crew.value.clone(),
            crew_source: policy.review_crew.source.label().to_string(),
            problems: before_pr_problems,
            local_route_incompatible: before_pr_on && local_route,
        },
        before_landing: BeforeLandingSwitch {
            enabled: before_landing_on,
            source: policy.review_before_landing.source.label().to_string(),
            problems: before_landing_problems,
            local_route_incompatible: before_landing_on && local_route,
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

/// Why the shared reviewer crew cannot run a gated review here.
fn crew_problems(runtime: &OrbitRuntime, policy: &OperationPolicy) -> Vec<String> {
    match policy.review_crew.value.as_deref() {
        None => {
            vec!["operation.review_crew is unset, so every gated delivery is refused".to_string()]
        }
        Some(crew) => runtime
            .resolve_crew_for_task(Some(crew), None)
            .err()
            .map(|error| format!("review crew `{crew}` does not resolve: {error}"))
            .into_iter()
            .collect(),
    }
}
