//! Automatic review switches: before-PR, before-landing and after-landing review
//! in one row.

use crate::doctor::*;

/// Every automatic-review switch in one row [ORB-13992]: before-PR review
/// (`review.timing = before_pr`, its minutes and crew), before-landing review
/// (`review.timing = before_landing`, sharing them, and the machines
/// `review.before_landing_hosts` turns it on for [ORB-15192]) and
/// after-landing review (the `delivery-code-review` auto-task, with when its
/// next batch is due), each with its source. A host list that names this
/// owner, whose own deliveries never read it, or repeats a workspace-wide
/// `review.timing = before_landing` is a warning; one whose claims would be refused is
/// an error. A switch that is on but cannot run here reviews nothing
/// while every other surface looks healthy, so that is an error, not a
/// warning: before-PR or before-landing review on a local-only ship
/// workspace [ORB-14168], either without a resolvable crew, or an after-landing
/// consumer that is missing, owned by another machine, wedged, stalled, held
/// for an operator, watching a branch that does not resolve, naming a crew
/// that does not, or trailing `origin/<branch>` past the batch's
/// `max_wait_minutes`. The trail is the remote-tracking ref already in the
/// checkout; this check does not fetch.
pub(in crate::doctor) fn doctor_check_review(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    const CHECK: &str = "review";
    let switches =
        match orbit_core::application::review::review_switches(runtime, chrono::Utc::now()) {
            Ok(switches) => switches,
            Err(error) => {
                return actionable_check(
                    CHECK,
                    WorkspaceDoctorStatus::Error,
                    format!("cannot establish which automatic review runs here: {error}"),
                    "Resolve the error, then rerun `orbit doctor`.".to_string(),
                );
            }
        };
    let message = format!(
        "before-PR review: {}. before-landing review: {}. after-landing review: {}",
        switches.before_pr_line(),
        switches.before_landing_line(),
        switches.after_landing_line()
    );
    let mut remediation = Vec::new();
    if switches.before_pr.local_route_incompatible {
        remediation.push(
            "`review.timing` is `before_pr` and this workspace ships locally, so every local \
             delivery is refused before it can run. Turn it off with `orbit config set \
             review.timing off`, or ship through the PR route. After-landing review is the \
             `delivery-code-review` auto-task, not this switch."
                .to_string(),
        );
    }
    if switches.before_landing.local_route_incompatible {
        remediation.push(
            "`review.timing` is `before_landing` and this workspace ships locally, which opens no \
             pull request to review, so every local delivery is refused before it can run. Turn \
             it off with `orbit config set review.timing off`, or ship through the PR route."
                .to_string(),
        );
    }
    // With the switch off, before-landing problems are about the listed
    // hosts' claims, not this workspace's deliveries.
    let hosts_only = !switches.before_landing.enabled && !switches.before_landing.hosts.is_empty();
    if hosts_only && switches.before_landing_unhealthy() {
        remediation.push(
            "`review.before_landing_hosts` lists machines whose claims this owner would refuse. \
             Fix each problem named above (ship through the PR route, set \
             `workflow.purpose_crews.pre_landing_review` or `workflow.purpose_crews.review`), \
             or remove `review.before_landing_hosts` from `config.toml`."
                .to_string(),
        );
    }
    let crew_unusable = |problems: usize, local_route: bool| problems > usize::from(local_route);
    if crew_unusable(
        switches.before_pr.problems.len(),
        switches.before_pr.local_route_incompatible,
    ) || (!hosts_only
        && crew_unusable(
            switches.before_landing.problems.len(),
            switches.before_landing.local_route_incompatible,
        ))
    {
        let layer = if switches.before_landing.enabled {
            "Before-landing"
        } else {
            "Before-PR"
        };
        remediation.push(format!(
            "{layer} review needs `workflow.purpose_crews.pre_landing_review`, or \
             `workflow.purpose_crews.review` while that is unset, set to a crew, or a pool with \
             a crew, that resolves on this host (it reads `{}` now); set one, or set \
             `review.timing` to `off`.",
            switches.before_pr.crew_key,
        ));
    }
    if let Some(health) = switches
        .after_landing
        .health
        .as_ref()
        .filter(|health| !health.healthy())
    {
        remediation.push(format!(
            "After-landing review runs only through `{consumer}` on the machine that owns this \
             workspace. Fix each problem named above (`orbit auto-task show {consumer} \
             --preview` shows the consumer), or turn it off with `orbit auto-task toggle \
             {consumer} off`.",
            consumer = health.consumer
        ));
    }
    let warnings = &switches.before_landing.warnings;
    if !warnings.is_empty() {
        remediation.push(format!(
            "{}. Edit `review.before_landing_hosts` to list only follower machines.",
            warnings.join("; ")
        ));
    }
    if remediation.is_empty() {
        return check(CHECK, WorkspaceDoctorStatus::Ok, message);
    }
    let status = if remediation.len() == usize::from(!warnings.is_empty()) {
        WorkspaceDoctorStatus::Warning
    } else {
        WorkspaceDoctorStatus::Error
    };
    remediation.push("Then rerun `orbit doctor`.".to_string());
    actionable_check(CHECK, status, message, remediation.join(" "))
}
