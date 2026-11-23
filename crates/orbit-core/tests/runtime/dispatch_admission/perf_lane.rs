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

/// A perf task waiting on a held lane names the lane even when every drain
/// slot is full [ORB-26479]: the lane is what it waits on, not the slot count.
/// The holder is a live leaf, so the single slot is taken.
#[test]
fn a_perf_task_waiting_on_the_lane_reports_it_when_no_slot_is_free() {
    if !isolated(
        "dispatch_admission::perf_lane::a_perf_task_waiting_on_the_lane_reports_it_when_no_slot_is_free",
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
    running_run(
        &runtime,
        "task_auto_pipeline",
        json!({"task_ids": [holder]}),
    );
    let waiting = seed(
        &runtime,
        Seed::named("second measured").tags(&["perf-review"]),
    )
    .id;

    let readiness = runtime
        .workspace_auto_readiness(&[], Some(1), 50, &[])
        .unwrap();
    let entry = readiness_task(&readiness, &waiting);
    assert_eq!(entry["reason"], "perf_lane_busy", "{entry}");
    assert_ne!(entry["eligible"], true, "{entry}");
    assert_eq!(entry["blocking_task_ids"], json!([holder]), "{entry}");
    assert_eq!(entry["conflicts"][0]["provenance"], "perf_lane", "{entry}");
    let detail = entry["detail"].as_str().unwrap();
    assert!(
        detail.contains("perf lane") && detail.contains(&holder),
        "{detail}"
    );
}

#[test]
fn perf_claim_only_holds_the_lane_on_its_execution_machine() {
    if !isolated(
        "dispatch_admission::perf_lane::perf_claim_only_holds_the_lane_on_its_execution_machine",
    ) {
        return;
    }
    use orbit_store::contracts::{
        AdmissionRunContext, ExecutionClaim, ExecutionClaimPhase, ExecutionLocation,
    };
    for (machine, local) in [("remote", false), ("owner", true)] {
        let (_root, runtime, repo) = runtime();
        let runtime = runtime.with_automation_machine_identity(Some("owner".into()));
        write_files(&repo, &["holder.rs", "candidate.rs"]);
        let holder = seed(
            &runtime,
            Seed::named("holder")
                .status(TaskStatus::InProgress)
                .tags(&["performance"])
                .files(&["holder.rs"]),
        );
        let candidate = seed(
            &runtime,
            Seed::named("candidate")
                .tags(&["performance"])
                .files(&["candidate.rs"]),
        );
        let claim = ExecutionClaim {
            claim_id: "perf-claim".into(),
            task_id: holder.id.clone(),
            request_id: "request".into(),
            executed_on: ExecutionLocation {
                machine_id: machine.into(),
                machine_name: None,
            },
            run_context: AdmissionRunContext {
                run_id: "claim-run".into(),
                job_name: "task_pipeline".into(),
                machine_name: None,
            },
            footprint: vec!["file:holder.rs".into()],
            reservation_id: "reservation".into(),
            reservation_expires_at: (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
            phase: ExecutionClaimPhase::Running,
            repair: None,
        };
        let connection =
            rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
        connection.execute("INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at) VALUES (?1, 'distributed-execution-claim-v1', ?2, ?3, 'fixture', ?4)", rusqlite::params![runtime.workspace_id().unwrap(), claim.claim_id, serde_json::to_string(&claim).unwrap(), Utc::now().to_rfc3339()]).unwrap();
        let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
        let wave = classify(&runtime, &drain);
        let readiness = default_readiness(&runtime);
        let entry = readiness_task(&readiness, &candidate.id);
        if local {
            let waiting = deferred(&wave, &candidate.id);
            assert_eq!(waiting["reason"], "perf_lane_busy", "{wave:#}");
            assert_eq!(waiting["blocking_task_ids"], json!([holder.id]));
            assert_eq!(entry["reason"], "perf_lane_busy", "{entry}");
            assert_eq!(entry["blocking_task_ids"], json!([holder.id]));
        } else {
            assert_eq!(wave["loose_task_ids"], json!([candidate.id]), "{wave:#}");
            assert_eq!(entry["reason"], "ready", "{entry}");
            assert_eq!(entry["eligible"], true, "{entry}");
        }
    }
}
