//! One read view of every automatic-review switch [ORB-13992].
//!
//! Before-PR review is `review.timing = before_pr` and before-landing review
//! is `review.timing = before_landing` [ORB-14849], both with `review.minutes`
//! and the pre-landing reviewer pool (`workflow.purpose_crews.pre_landing_review`,
//! else `workflow.purpose_crews.review`); an owner may also turn before-landing
//! review on only for the claims of the machines `review.before_landing_hosts`
//! lists [ORB-15192]. After-landing review is the `delivery-code-review`
//! auto-task's `enabled` flag, and its review tasks draw from
//! `workflow.purpose_crews.review`. `orbit config show`,
//! `orbit doctor`, the dashboard and the drain probe all render this view, so
//! they report the switches together and with the same provenance.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_config::{OperationPolicy, PRE_LANDING_REVIEW_CREW_KEY};
use orbit_types::workflow::ShipMode;
use serde::Serialize;
use serde_json::{Value as JsonValue, json};

use super::crew::{
    REVIEW_CREW_SELECTION_RULE, configured_pool_members, pre_landing_review_crew_unset,
};
use super::{
    ReviewDistributedPort, local_route_before_landing_conflict, local_route_before_pr_conflict,
};
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

/// `review.timing = before_pr`: hold PR creation for a fresh reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BeforePrSwitch {
    pub enabled: bool,
    /// Config layer that decided `review.timing`.
    pub source: String,
    /// `review.minutes`: the wall-clock limit for one candidate's review.
    pub minutes: u32,
    pub minutes_source: String,
    /// The reviewer crew or pool before-PR and before-landing review draw
    /// from, as canonical `name[:weight]` entries; empty when unset.
    pub crews: Vec<String>,
    /// Config layer that set `crew_key`.
    pub crew_source: String,
    /// The key `crews` came from: `workflow.purpose_crews.pre_landing_review`,
    /// or `workflow.purpose_crews.review` while that is unset.
    pub crew_key: &'static str,
    /// How each review chooses from `crews` when it names more than one
    /// [ORB-15195].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crew_selection: Option<&'static str>,
    /// Why before-PR review cannot run here while it is on; empty when it
    /// can or is off.
    pub problems: Vec<String>,
    /// `review.timing` is `before_pr` and automatic delivery uses the local-only
    /// route [ORB-14168]. The problem text already says so; doctor uses this
    /// to name the ship-mode remedy rather than the crew remedy. Omitted from
    /// JSON because `problems` and `healthy` carry it.
    #[serde(skip)]
    pub local_route_incompatible: bool,
}

/// `review.timing = before_landing`: review the open PR before it lands. It shares
/// `review.minutes` and the reviewer pool with [`BeforePrSwitch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BeforeLandingSwitch {
    pub enabled: bool,
    /// Config layer that decided `review.timing`.
    pub source: String,
    /// `review.before_landing_hosts`: machines whose claimed leaves review
    /// before landing while `enabled` is off here.
    pub hosts: Vec<String>,
    /// Config layer that decided `review.before_landing_hosts`.
    pub hosts_source: String,
    /// Why before-landing review cannot run here while it is on, or for the
    /// listed hosts' claims; empty when it can or is off.
    pub problems: Vec<String>,
    /// A host list that does not do what it seems to: it names this owner,
    /// whose own deliveries never read it, or `enabled` already covers every
    /// machine.
    pub warnings: Vec<String>,
    /// `review.timing` is `before_landing` and automatic delivery uses the
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
    /// `workflow.purpose_crews.review`, the pool the minted review tasks draw
    /// from as canonical `name[:weight]` entries; empty when unset.
    pub crews: Vec<String>,
    /// Config layer that set `crews`.
    pub crew_source: String,
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
            "{problems}{} (review.timing, {}); {} min per candidate (review.minutes, {}); {}",
            if before_pr.enabled { "on" } else { "off" },
            before_pr.source,
            before_pr.minutes,
            before_pr.minutes_source,
            self.pre_landing_crew_phrase(),
        )
    }

    /// The reviewer pool before-PR and before-landing review share, with the
    /// key and layer it came from and, for a fallback, that the pre-landing
    /// key is unset.
    fn pre_landing_crew_phrase(&self) -> String {
        let before_pr = &self.before_pr;
        let crew = match before_pr.crews.as_slice() {
            [] => "crew `-`".to_string(),
            [only] => format!("crew `{only}`"),
            crews => format!("crews [{}]", crews.join(", ")),
        };
        let fallback = if before_pr.crew_key == PRE_LANDING_REVIEW_CREW_KEY {
            String::new()
        } else {
            format!("; {PRE_LANDING_REVIEW_CREW_KEY} is unset")
        };
        let selection = before_pr
            .crew_selection
            .map_or_else(String::new, |rule| format!("; {rule}"));
        format!(
            "{crew} ({}, {}{fallback}{selection})",
            before_pr.crew_key, before_pr.crew_source,
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
        let hosts = if before_landing.hosts.is_empty() || before_landing.enabled {
            String::new()
        } else {
            format!(
                "; on for claims from {} (review.before_landing_hosts, {})",
                before_landing.hosts.join(", "),
                before_landing.hosts_source,
            )
        };
        format!(
            "{problems}{} (review.timing, {}){hosts}; shares review.minutes and its reviewer \
             with before-PR review: {}",
            if before_landing.enabled { "on" } else { "off" },
            before_landing.source,
            self.pre_landing_crew_phrase(),
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
/// it just loaded, since the runtime's copy is the one it opened with. The
/// task reads behind it — the after-landing consumer's open instance, wedged
/// action and last batch — share one acquisition of the task store's commit
/// boundary.
pub fn review_switches_view(
    runtime: &OrbitRuntime,
    policy: &OperationPolicy,
    now: DateTime<Utc>,
) -> JsonValue {
    let switches =
        match runtime.in_task_read_section(|| review_switches_under(runtime, policy, now)) {
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
    let hosts = &policy.review_before_landing_hosts;
    let mut before_landing_warnings = Vec::new();
    if before_landing_on {
        before_landing_problems.extend(crew_problems);
        if !hosts.value.is_empty() {
            before_landing_warnings.push(format!(
                "review.before_landing_hosts ({}) is redundant: review.timing ({}) is \
                 already before_landing for every delivery",
                hosts.source.label(),
                policy.review_before_landing.source.label(),
            ));
        }
    } else if !hosts.value.is_empty() {
        // The listed hosts' claims are what reviews here, admitted under the
        // owner's claim ship mode; a follower resolves the crew itself, but
        // an unset one refuses every claim.
        if runtime.owner_delivery_ship().mode == "local" {
            before_landing_problems.push(format!(
                "review.before_landing_hosts ({}) lists machines, but this workspace ships \
                 locally and opens no pull request to review, so their claims are refused",
                hosts.source.label(),
            ));
        }
        if policy.pre_landing_review_pool().entries.is_empty() {
            before_landing_problems.push(format!(
                "{}, so every claim from a listed host is refused",
                pre_landing_review_crew_unset()
            ));
        }
    }
    if let Some(owner) = runtime.drain_owner_machine()
        && hosts.value.contains(&owner)
    {
        before_landing_warnings.push(format!(
            "review.before_landing_hosts ({}) lists this owner ({owner}); its own deliveries \
             never read the list, only claims it pulls from itself do, so set \
             review.timing = before_landing to review its own deliveries before landing",
            hosts.source.label(),
        ));
    }
    let (enabled, source) = after_landing_switch(runtime)?;
    let health = after_landing_health(runtime, now)?;
    let crew = policy.pre_landing_review_pool();
    Ok(ReviewSwitches {
        before_pr: BeforePrSwitch {
            enabled: before_pr_on,
            source: policy.review_before_pr.source.label().to_string(),
            minutes: policy.review_minutes.value,
            minutes_source: policy.review_minutes.source.label().to_string(),
            crews: crew.entries.to_vec(),
            crew_source: crew.source.label().to_string(),
            crew_key: crew.key,
            crew_selection: (crew.entries.len() > 1).then_some(REVIEW_CREW_SELECTION_RULE),
            problems: before_pr_problems,
            local_route_incompatible: before_pr_on && local_route,
        },
        before_landing: BeforeLandingSwitch {
            enabled: before_landing_on,
            source: policy.review_before_landing.source.label().to_string(),
            hosts: hosts.value.clone(),
            hosts_source: hosts.source.label().to_string(),
            problems: before_landing_problems,
            warnings: before_landing_warnings,
            local_route_incompatible: before_landing_on && local_route,
        },
        after_landing: AfterLandingSwitch {
            enabled,
            source: source.label(),
            crews: policy.review_crew.value.clone(),
            crew_source: policy.review_crew.source.label().to_string(),
            next_batch_due: health
                .as_ref()
                .and_then(|health| health.next_batch_due.clone()),
            health,
        },
    })
}

/// Why the pre-landing reviewer crew cannot run a gated review here. A pool
/// can while any member resolves [ORB-15195].
fn crew_problems(runtime: &OrbitRuntime, policy: &OperationPolicy) -> Vec<String> {
    let pool = policy.pre_landing_review_pool();
    let members = match configured_pool_members(runtime, pool.entries, pool.key) {
        Ok(members) => members,
        Err(error) => {
            return vec![format!("{} does not resolve: {error}", pool.key)];
        }
    };
    match members.as_slice() {
        [] => {
            vec![format!(
                "{}, so every gated delivery is refused",
                pre_landing_review_crew_unset()
            )]
        }
        [only] => runtime
            .resolve_crew_for_task(Some(&only.name), None)
            .err()
            .map(|error| format!("review crew `{}` does not resolve: {error}", only.name))
            .into_iter()
            .collect(),
        _ => runtime
            .review_crew_candidates(&members)
            .err()
            .map(|error| format!("no crew of the review pool resolves: {error}"))
            .into_iter()
            .collect(),
    }
}
