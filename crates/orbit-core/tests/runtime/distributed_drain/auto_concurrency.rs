//! A pull drain under `machine.auto_concurrency` on the follower.
//!
//! Under `enforce` each request names the work the follower's load forecast
//! has room for, no request goes out when nothing fits, and a claim is
//! reserved on the follower. Shadow mode records the list and requests as
//! with `off`, and `off` sends no list.
//!
//! The follower reads a fake `/proc` with 15 CPUs and 32 GiB, three quarters
//! of which admission may use, and has one `change`/`low` profile using 12
//! GiB. That profile prices every other kind of work too, as the heaviest on
//! the host.

use super::*;
use chrono::Duration as ChronoDuration;
use orbit_core::runtime::host_resource::forecast::ForecastDecision;
use orbit_store::contracts::{
    AdmissionLoadQuery, AdmissionLoadStoreBackend, AdmissionReservation, ResourceUsageStoreBackend,
    RunResourceMinute,
};
use orbit_types::workflow::{JobRun, PullAdmissionFit};

const GIB: u64 = 1024 * 1024 * 1024;

/// A pair whose follower runs `mode`, with the profile and fake `/proc` above.
fn forecasting(mode: &str) -> (Pair, PathBuf) {
    let mut pair = Pair::with_follower_global(
        1,
        &format!(
            "[machine.auto_concurrency]\nmode = \"{mode}\"\nmin_profile_deliveries = 1\n\
             horizon_cap_minutes = 10\n"
        ),
    );
    let proc_root = pair._root.path().join(FOLLOWER).join("proc");
    std::fs::create_dir_all(&proc_root).unwrap();
    pair.follower = pair
        .follower
        .clone()
        .with_host_load_proc_root(proc_root.clone());
    finished_delivery(&pair.follower, 12 * GIB);
    (pair, proc_root)
}

/// One finished three-minute `change`/`low` delivery using `bytes`, admitted
/// two days ago.
fn finished_delivery(follower: &OrbitRuntime, bytes: u64) {
    let store = follower.sqlite_store().unwrap();
    let workspace = follower.workspace_id().unwrap();
    let admitted = Utc::now() - ChronoDuration::days(2);
    let run = JobRun {
        executed_on: None,
        run_id: "jrun-profiled".into(),
        job_id: "task_pr_pipeline".into(),
        attempt: 1,
        state: JobRunState::Success,
        scheduled_at: admitted,
        started_at: Some(admitted),
        finished_at: Some(admitted + ChronoDuration::minutes(3)),
        duration_ms: None,
        created_at: admitted,
        pid: None,
        pid_start_time: None,
        input: Some(json!({"task_id": "ORB-profiled"})),
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    store
        .upsert_job_run_for_workspace(&workspace, &run, None)
        .unwrap();
    for minute in 0..3 {
        store
            .record_run_resource_minute(&RunResourceMinute {
                workspace_id: workspace.clone(),
                run_id: run.run_id.clone(),
                minute_index: minute,
                minute_started_at: admitted + ChronoDuration::minutes(i64::from(minute)),
                task_id: "ORB-profiled".into(),
                work_class: "change".into(),
                complexity: Some("low".into()),
                resumed: false,
                delivery_admitted_at: Some(admitted),
                cpu_cores: 1.0,
                memory_bytes: bytes,
                samples: 12,
            })
            .unwrap();
    }
}

/// A host of 15 idle CPUs with `used` of its 32 GiB in use.
fn host_using(proc_root: &Path, used: u64) {
    let mut stat = "cpu  0 0 0 1000 0 0 0 0 0 0\n".to_string();
    for cpu in 0..15 {
        stat.push_str(&format!("cpu{cpu} 0 0 0 0 0 0 0 0 0 0\n"));
    }
    std::fs::write(proc_root.join("stat"), stat).unwrap();
    std::fs::write(
        proc_root.join("meminfo"),
        format!(
            "MemTotal: {} kB\nMemFree: 1024 kB\nMemAvailable: {} kB\n",
            32 * GIB / 1024,
            (32 * GIB - used) / 1024
        ),
    )
    .unwrap();
}

fn admission_fit(pair: &Pair, drain: &str) -> Option<PullAdmissionFit> {
    pair.follower
        .read_run_state(drain)
        .unwrap()
        .unwrap()
        .pull_admission_fit
}

/// Every reservation on the follower's host, superseded or not.
fn reservations(follower: &OrbitRuntime) -> Vec<AdmissionReservation> {
    let now = Utc::now();
    let mut seen = Vec::new();
    follower
        .sqlite_store()
        .unwrap()
        .admit_under_host_lock(
            &AdmissionLoadQuery {
                now,
                delivery_jobs: &[],
                reservations_expire_before: now - ChronoDuration::minutes(5),
            },
            &mut |load| {
                seen = load.reservations.clone();
                Ok(Vec::new())
            },
        )
        .unwrap();
    seen
}

/// No request goes out while the forecast has room for nothing; once it has
/// room, the request names what fits, and the claim the owner returns is
/// reserved on the follower before its leaf run exists.
#[test]
fn an_enforcing_follower_requests_only_work_its_forecast_has_room_for() {
    if !isolated(
        module_path!(),
        "an_enforcing_follower_requests_only_work_its_forecast_has_room_for",
    ) {
        return;
    }
    let (pair, proc_root) = forecasting("enforce");
    // 16 GiB in use plus 12 is over the 24 GiB admission may reach.
    host_using(&proc_root, 16 * GIB);
    let drain = pair.run_drain();

    let pass = pair.pass(&drain);

    assert!(pair.wire.calls("orbit.task.pull").is_empty(), "{pass}");
    let refusal = pass["refusal"].as_str().unwrap_or_default();
    assert!(refusal.starts_with("forecast_no_headroom:"), "{pass}");
    let fit = admission_fit(&pair, &drain).expect("the fit is recorded");
    assert!(fit.sent && fit.admissible.is_empty(), "{fit:?}");
    assert!(
        fit.unfit.iter().any(|unfit| unfit.work == "change/low"),
        "{fit:?}"
    );
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");

    host_using(&proc_root, 4 * GIB);
    let pass = pair.pass(&drain);

    let pulls = pair.wire.calls("orbit.task.pull");
    assert_eq!(pulls.len(), 1, "{pass}");
    let keys = &pulls[0]["admissible"]["keys"];
    assert!(
        keys.as_array()
            .unwrap()
            .contains(&json!({"work_class": "change", "complexity": "low"})),
        "{}",
        pulls[0]
    );
    let reserved = reservations(&pair.follower);
    assert_eq!(reserved.len(), 1, "{reserved:?}");
    assert_eq!(reserved[0].task_id, pair.tasks[0]);
    assert_eq!(
        (
            reserved[0].work_class.as_str(),
            reserved[0].complexity.as_deref()
        ),
        ("change", Some("low"))
    );
    let leaf = pair
        .leaf_runs()
        .into_iter()
        .next()
        .expect("the claim made a leaf");
    let leaf = pair.follower_jobs.get_job_run(&leaf).unwrap().unwrap();
    assert!(
        leaf.created_at >= reserved[0].reserved_at,
        "the leaf run replaces the reservation in the forecast"
    );
    let reserved_in_forecast = pair
        .follower
        .admit_against_forecast(|forecast| {
            Ok(ForecastDecision {
                outcome: forecast.reservations.len(),
                admitted: Vec::new(),
            })
        })
        .unwrap();
    assert_eq!(reserved_in_forecast, 0, "counted once, by its leaf run");
}

/// Shadow mode records what it would have sent and requests as with `off`,
/// even with no room; `off` computes nothing and sends no list.
#[test]
fn shadow_and_off_followers_request_without_a_list() {
    if !isolated(
        module_path!(),
        "shadow_and_off_followers_request_without_a_list",
    ) {
        return;
    }
    for mode in ["shadow", "off"] {
        let (pair, proc_root) = forecasting(mode);
        host_using(&proc_root, 16 * GIB);
        let drain = pair.run_drain();

        let pass = pair.pass(&drain);

        let pulls = pair.wire.calls("orbit.task.pull");
        assert_eq!(pulls.len(), 1, "{mode}: {pass}");
        assert!(pulls[0].get("admissible").is_none(), "{mode}: {}", pulls[0]);
        let fit = admission_fit(&pair, &drain);
        if mode == "shadow" {
            let fit = fit.expect("shadow mode records the fit");
            assert!(!fit.sent && fit.admissible.is_empty(), "{fit:?}");
        } else {
            assert_eq!(fit, None);
            assert!(reservations(&pair.follower).is_empty());
        }
    }
}
