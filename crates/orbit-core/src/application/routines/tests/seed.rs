//! The seeded ship sweep must finish before its next fire.

use chrono::{TimeZone, Utc};
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_engine::activity_job::load_job_asset;
use orbit_types::workflow::{JobV2StepBody, MissedRunPolicy, OverlapPolicy};

use super::super::due::next_occurrence;
use super::super::parse_cron;
use super::super::seed::SUPERSEDED_ROUTINE_TEMPLATES;
use super::super::template::{ShippedShape, shipped_shape_of};
use super::materialize::{current_template, render};
use crate::application::job::DEFAULT_JOB_FILES;

/// `overlap: forbid` skips a fire while the previous one is still running, and
/// `missed_run: skip` does not make that fire up. The 20-minute cadence
/// equalled the 1200-second drain, so a non-empty backlog skipped every other
/// scheduled ship [ORB-14358].
#[test]
fn shipped_ship_sweep_leaves_slack_past_its_drain_window() {
    let current = render(current_template("ship_sweep"), "ship_sweep", "orbit");
    let routine = parse_routine_yaml(&current).expect("seeded ship sweep parses");
    assert_eq!(routine.target.job_name(), "workspace_ship_pipeline");
    assert_eq!(routine.policy.overlap, OverlapPolicy::Forbid);
    assert_eq!(routine.trigger.missed_run, MissedRunPolicy::Skip);

    let gap_seconds = minimum_gap_seconds(&routine.trigger.cron);
    let window_seconds = ship_drain_window_seconds();
    assert!(
        gap_seconds > window_seconds,
        "ship-sweep cadence ({gap_seconds}s) must outlast the drain window \
         ({window_seconds}s) so overlap:forbid does not skip the next fire \
         (ORB-14358)"
    );

    let previous = render(superseded_ship_sweep(), "ship_sweep", "orbit");
    let opted_in = previous.replacen("enabled: false", "enabled: true", 1);
    assert_ne!(
        opted_in, previous,
        "the prior template must carry an enabled opt-in the test can flip"
    );
    assert_eq!(
        shipped_shape_of("ship_sweep", &previous),
        Some(ShippedShape::Superseded),
        "a workspace still on the 20-minute ship sweep must refresh"
    );
    assert_eq!(
        shipped_shape_of("ship_sweep", &opted_in),
        Some(ShippedShape::Superseded),
        "opting in must not look like a local edit of the old cadence"
    );
    let opted_current = current.replacen("enabled: false", "enabled: true", 1);
    assert_eq!(
        shipped_shape_of("ship_sweep", &opted_current),
        Some(ShippedShape::Current),
        "the 30-minute cadence, opted in, is the template this release ships"
    );
}

fn superseded_ship_sweep() -> &'static str {
    let mut matches = SUPERSEDED_ROUTINE_TEMPLATES
        .iter()
        .filter(|(stem, _)| *stem == "ship_sweep")
        .map(|(_, template)| *template);
    let template = matches
        .next()
        .expect("the 20-minute ship sweep is retained as a superseded template");
    assert!(
        matches.next().is_none(),
        "ship_sweep has one superseded shape"
    );
    template
}

fn ship_drain_window_seconds() -> i64 {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find(|(name, _)| *name == "workspace_ship_pipeline")
        .map(|(_, yaml)| *yaml)
        .expect("workspace_ship_pipeline is a seeded job");
    let job = load_job_asset(yaml).expect("ship job parses");
    let step = job
        .spec
        .steps
        .iter()
        .find(|step| step.id == "auto")
        .expect("ship job waits on an auto step");
    let input = match &step.body {
        JobV2StepBody::TargetRef(target) => target.default_input.as_ref(),
        JobV2StepBody::Target(target) => target.default_input.as_ref(),
        _ => None,
    };
    let seconds = input
        .and_then(|input| input.pointer("/run_input/for_seconds"))
        .and_then(serde_json::Value::as_i64)
        .expect("auto step run_input.for_seconds is a number");
    assert!(seconds > 0, "drain window must be positive");
    seconds
}

/// Shortest gap between successive fires across one UTC day.
fn minimum_gap_seconds(expression: &str) -> i64 {
    let cron = parse_cron(expression).expect("ship-sweep cron parses");
    let mut cursor = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
    let mut min_gap: Option<i64> = None;
    for _ in 0..48 {
        let next = next_occurrence(&cron, &cursor).expect("next ship-sweep slot");
        let gap = next.signed_duration_since(cursor).num_seconds();
        assert!(gap > 0, "cron slots must move forward");
        min_gap = Some(min_gap.map_or(gap, |current| current.min(gap)));
        cursor = next;
    }
    min_gap.expect("a day of ship-sweep slots")
}
