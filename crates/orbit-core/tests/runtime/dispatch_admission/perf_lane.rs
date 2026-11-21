use super::*;

fn classify(runtime: &OrbitRuntime, drain: &str) -> Value {
    runtime
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": drain, "max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify")
}

fn deferred<'a>(wave: &'a Value, task: &str) -> &'a Value {
    wave["deferred_conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .unwrap_or_else(|| panic!("{task} is not deferred: {wave:#}"))
}

/// Two tasks that run the paired timing protocol share one host-wide perf
/// lane, so a second is not admitted while the first runs, nor beside it in
/// one wave. It waits as `perf_lane_busy`, naming the task that holds the
/// lane, in the drain's pass and in readiness; unrelated work still starts.
#[test]
fn a_second_perf_task_waits_for_the_perf_lane_instead_of_taking_a_slot() {
    if !isolated(
        "dispatch_admission::perf_lane::a_second_perf_task_waits_for_the_perf_lane_instead_of_taking_a_slot",
    ) {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    let holder = seed(
        &runtime,
        Seed::named("measured change")
            .status(TaskStatus::InProgress)
            .tags(&["performance"]),
    )
    .id;
    let waiting = seed(
        &runtime,
        Seed::named("second measured").tags(&["perf-review"]),
    )
    .id;
    let plain = seed(&runtime, Seed::named("unrelated")).id;
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));

    let wave = classify(&runtime, &drain);
    assert_eq!(wave["loose_task_ids"], json!([plain]), "{wave:#}");
    let entry = deferred(&wave, &waiting);
    assert_eq!(entry["reason"], "perf_lane_busy", "{entry}");
    assert_eq!(entry["blocking_task_ids"], json!([holder]), "{entry}");
    assert_eq!(entry["conflicts"][0]["provenance"], "perf_lane", "{entry}");
    let detail = entry["detail"].as_str().unwrap();
    assert!(
        detail.contains("perf lane") && detail.contains(&holder),
        "{detail}"
    );

    let readiness = runtime
        .workspace_auto_readiness(&[], Some(4), 50, &[])
        .unwrap();
    let entry = readiness_task(&readiness, &waiting);
    assert_eq!(entry["reason"], "perf_lane_busy", "{entry}");
    assert_ne!(entry["eligible"], true, "{entry}");
    assert_eq!(entry["blocking_task_ids"], json!([holder]), "{entry}");
    assert_eq!(readiness_task(&readiness, &plain)["reason"], "ready");
}

/// With no perf task running, the first perf task of a wave takes the lane
/// and the next waits on it, so one wave never admits both.
#[test]
fn one_wave_admits_a_single_perf_task() {
    if !isolated("dispatch_admission::perf_lane::one_wave_admits_a_single_perf_task") {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    let first = seed(
        &runtime,
        Seed::named("first measured").tags(&["performance"]),
    )
    .id;
    let second = seed(
        &runtime,
        Seed::named("second measured").tags(&["performance"]),
    )
    .id;
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));

    let wave = classify(&runtime, &drain);
    assert_eq!(wave["loose_task_ids"], json!([first]), "{wave:#}");
    let entry = deferred(&wave, &second);
    assert_eq!(entry["reason"], "perf_lane_busy", "{entry}");
    assert_eq!(entry["blocking_task_ids"], json!([first]), "{entry}");
}
