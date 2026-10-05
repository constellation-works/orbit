//! Scheduler catch-up after downtime [ORB-10149].

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::OrbitRuntime;
use crate::application::auto_tasks::scheduler::{SchedulerOptions, run_auto_task_scheduler_at};

use super::interval_params;

fn runtime() -> OrbitRuntime {
    OrbitRuntime::in_memory().expect("build in-memory runtime")
}

fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0)
        .single()
        .expect("valid ts")
}

fn fire(runtime: &OrbitRuntime, now: DateTime<Utc>) -> Vec<(String, Option<String>)> {
    let outcome = run_auto_task_scheduler_at(runtime, now, SchedulerOptions::default())
        .expect("scheduler pass");
    outcome
        .reports
        .iter()
        .map(|report| (report.action.to_string(), report.task_id.clone()))
        .collect()
}

#[test]
fn catch_up_collapses_downtime_to_one_task() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    fire(&runtime, t0); // baseline
    // Six hours of downtime: a single make-up task, not six.
    let reports = fire(&runtime, t0 + Duration::minutes(370));
    assert_eq!(reports.iter().filter(|(a, _)| a == "fired").count(), 1);
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 1);
}
