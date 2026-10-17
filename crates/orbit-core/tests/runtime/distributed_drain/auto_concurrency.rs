//! A pull drain under `machine.auto_concurrency` on the follower.
//!
//! Under `enforce` each request names the work the follower's load forecast
//! has room for, no request goes out when nothing fits, and a claim is
//! reserved on the follower. Shadow mode records the list and requests as
//! with `off`, and `off` sends no list. While the follower's forecast holds
//! no running delivery or reservation, every kind of work fits.
//!
//! The follower reads a fake `/proc` with 15 CPUs and 32 GiB, three quarters
//! of which admission may use, and has one `change`/`low` profile using 12
//! GiB. That profile prices every other kind of work too, as the heaviest on
//! the host. Another workspace's delivery, running past its profile with
//! nothing measured, keeps the forecast from being idle without adding to it.

use super::*;
use chrono::Duration as ChronoDuration;
use orbit_core::runtime::host_resource::forecast::{AdmissionReservationRequest, ForecastDecision};
use orbit_core::runtime::host_resource::profile::ProfileLookup;
use orbit_store::contracts::{
    AdmissionForecastRecord, AdmissionLoadQuery, AdmissionLoadStoreBackend, AdmissionReservation,
    ResourceUsageStoreBackend, RunResourceMinute,
};
use orbit_types::workflow::{JobRun, PullAdmissionFit};

const GIB: u64 = 1024 * 1024 * 1024;

/// A pair whose follower runs `mode`, with the profile and fake `/proc` above.
fn forecasting(mode: &str, tasks: usize) -> (Pair, PathBuf) {
    forecasting_using(mode, tasks, 12 * GIB)
}

/// [`forecasting`] with a profile using `bytes`.
fn forecasting_using(mode: &str, tasks: usize, bytes: u64) -> (Pair, PathBuf) {
    let mut pair = Pair::with_follower_global(
        tasks,
        &format!(
            "[machine.auto_concurrency]\nmode = \"{mode}\"\nmin_profile_deliveries = 1\n\
             horizon_cap_minutes = 30\nadmit_cpu_percent = 75\nadmit_memory_percent = 75\n"
        ),
    );
    let proc_root = pair._root.path().join(FOLLOWER).join("proc");
    std::fs::create_dir_all(&proc_root).unwrap();
    pair.follower = pair
        .follower
        .clone()
        .with_host_load_proc_root(proc_root.clone());
    finished_delivery(&pair.follower, bytes);
    (pair, proc_root)
}

/// One finished 31-minute `change`/`low` delivery using `bytes`, admitted
/// two days ago.
fn finished_delivery(follower: &OrbitRuntime, bytes: u64) {
    finished_delivery_using(follower, bytes, 1.0);
}

fn finished_delivery_using(follower: &OrbitRuntime, bytes: u64, cores: f64) {
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
        finished_at: Some(admitted + ChronoDuration::minutes(31)),
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
    for minute in 0..31 {
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
                cpu_cores: cores,
                cpu_accounting: Some(orbit_store::contracts::CPU_ACCOUNTING_LINUX.into()),
                memory_bytes: bytes,
                samples: 12,
            })
            .unwrap();
    }
}

/// A delivery of another workspace on the follower's host, admitted forty
/// minutes ago and still running: past its borrowed 31-minute profile,
/// with nothing measured, it adds no load, but the forecast is not idle.
/// Returns the store that ends it.
fn neighbour_delivery(follower: &OrbitRuntime) -> (Arc<dyn JobRunStoreBackend>, String) {
    let jobs = orbit_store::compose::workspace_job_run_store(
        follower.sqlite_store().unwrap(),
        "ws-neighbour".to_string(),
    );
    let run = jobs
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({
                "task_ids": ["NB-1"],
                "resource_class": {
                    "task_id": "NB-1",
                    "work_class": "change",
                    "complexity": "low",
                    "admitted_at": Utc::now() - ChronoDuration::minutes(40),
                },
            })),
            None,
        )
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    (jobs, run.run_id)
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
        .pull_drain
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
                resource_minutes_since: None,
            },
            &mut |load| {
                seen = load.reservations.clone();
                Ok(Vec::new())
            },
        )
        .unwrap();
    seen
}

/// Forecast records on the follower, in request order.
fn forecasts(pair: &Pair) -> Vec<AdmissionForecastRecord> {
    pair.follower
        .sqlite_store()
        .unwrap()
        .admission_forecasts(Utc::now() - ChronoDuration::hours(1))
        .unwrap()
}

/// No request goes out while the forecast has room for nothing; once it has
/// room, the request names what fits, and the claim the owner returns is
/// reserved on the follower before its leaf run exists.
fn an_enforcing_follower_requests_only_work_its_forecast_has_room_for() {
    let (pair, proc_root) = forecasting("enforce", 1);
    neighbour_delivery(&pair.follower);
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
    let records = forecasts(&pair);
    assert_eq!(records.len(), 1);
    let held = records[0].pull.as_ref().unwrap();
    assert!(!held.sent && held.forecast_no_headroom);
    assert!(records[0].admitted.is_empty());
    assert_eq!(
        records[0]
            .points
            .iter()
            .map(|point| point.offset_minutes)
            .collect::<Vec<_>>(),
        [5, 10, 20, 30]
    );
    assert!(
        records[0]
            .points
            .iter()
            .all(|point| point.memory_bytes == 16 * GIB)
    );

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
    let records = forecasts(&pair);
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].admitted, pair.tasks);
    let sent = records[1].pull.as_ref().unwrap();
    assert!(sent.sent && !sent.forecast_no_headroom);
    assert!(
        records[1]
            .points
            .iter()
            .all(|point| point.memory_bytes == 16 * GIB),
        "4 GiB baseline plus the returned 12 GiB claim at every offset: {records:?}"
    );
}

/// Shadow mode records what it would have sent and requests as with `off`,
/// even with no room; `off` computes nothing and sends no list.
fn shadow_and_off_followers_request_without_a_list() {
    for (mode, tasks) in [("shadow", 1), ("shadow", 0), ("off", 1)] {
        let (pair, proc_root) = forecasting(mode, tasks);
        neighbour_delivery(&pair.follower);
        host_using(&proc_root, 16 * GIB);
        let drain = pair.run_drain();

        let follower = pair.follower.clone();
        *pair.wire.before_pull.lock().unwrap() = Some(Box::new(move || {
            assert!(
                reservations(&follower).is_empty(),
                "shadow/off never holds pending capacity"
            );
            Ok(())
        }));
        let pass = pair.pass(&drain);

        let pulls = pair.wire.calls("orbit.task.pull");
        assert_eq!(pulls.len(), 1, "{mode}: {pass}");
        assert!(pulls[0].get("admissible").is_none(), "{mode}: {}", pulls[0]);
        let fit = admission_fit(&pair, &drain);
        if mode == "shadow" {
            let fit = fit.expect("shadow mode records the fit");
            assert!(!fit.sent && fit.admissible.is_empty(), "{fit:?}");
            let records = forecasts(&pair);
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].admitted, pair.tasks);
            let decision = records[0].pull.as_ref().unwrap();
            assert!(decision.sent && decision.forecast_no_headroom);
            let expected = (16 + 12 * tasks as u64) * GIB;
            assert!(
                records[0]
                    .points
                    .iter()
                    .all(|point| point.memory_bytes == expected),
                "{records:?}"
            );
            let report = pair.follower.forecast_accuracy().unwrap();
            let counts = &report.drains[0];
            assert_eq!(
                (
                    counts.pull_requests,
                    counts.pull_sent,
                    counts.pull_would_hold
                ),
                (1, 1, 1)
            );
            assert!(
                counts.would_hold.is_empty(),
                "pull decisions count requests, not task differences"
            );
        } else {
            assert_eq!(fit, None);
            assert!(reservations(&pair.follower).is_empty());
            assert!(forecasts(&pair).is_empty());
        }
    }
}

/// Work whose profile alone is over the 24 GiB admission may reach is unfit
/// while another delivery runs on the host, and admissible once the
/// follower's forecast holds no running delivery or reservation, so the host
/// is not kept from it for good.
#[test]
fn an_enforcing_follower_admits_work_too_big_to_fit_only_on_an_idle_host() {
    if !isolated(
        module_path!(),
        "an_enforcing_follower_admits_work_too_big_to_fit_only_on_an_idle_host",
    ) {
        return;
    }
    crate::scenarios::with_immutable_host_load(|| {
        let (pair, proc_root) = forecasting_using("enforce", 1, 25 * GIB);
        let (neighbour_jobs, neighbour) = neighbour_delivery(&pair.follower);
        host_using(&proc_root, 4 * GIB);
        let drain = pair.run_drain();

        let pass = pair.pass(&drain);

        assert!(pair.wire.calls("orbit.task.pull").is_empty(), "{pass}");
        let fit = admission_fit(&pair, &drain).expect("the fit is recorded");
        assert!(fit.admissible.is_empty(), "{fit:?}");
        let unfit = fit
            .unfit
            .iter()
            .find(|unfit| unfit.work == "change/low")
            .expect("change/low is unfit");
        assert_eq!(unfit.reason, "memory 91% at +0m", "{fit:?}");

        neighbour_jobs
            .finalize_job_run(&neighbour, JobRunState::Success, Utc::now(), Some(0))
            .unwrap();
        let pass = pair.pass(&drain);

        let pulls = pair.wire.calls("orbit.task.pull");
        assert_eq!(pulls.len(), 1, "{pass}");
        assert!(
            pulls[0]["admissible"]["keys"]
                .as_array()
                .unwrap()
                .contains(&json!({"work_class": "change", "complexity": "low"})),
            "{}",
            pulls[0]
        );
        let fit = admission_fit(&pair, &drain).unwrap();
        assert!(fit.admissible.iter().any(|work| work == "change/low"));
        assert!(fit.unfit.is_empty(), "{fit:?}");
    });
}

/// A second workspace shares the first follower's host database and wire.
fn another_workspace(pair: &Pair, proc_root: &Path) -> Pair {
    let repo = pair._root.path().join(FOLLOWER).join("other-repo");
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    let output = crate::git_test_support::command()
        .args(["init", "--bare", "--initial-branch=main", "-q"])
        .arg(repo.join(".git"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let follower = calm_host(
        OrbitRuntime::from_roots(&pair.follower.global_root(), &repo.join(".orbit")).unwrap(),
    )
    .with_automation_machine_identity(Some(FOLLOWER.into()))
    .with_coordination_write_owner(Some(OWNER.into()))
    .with_drain_owner_transport(pair.wire.clone())
    .with_host_load_proc_root(proc_root.to_path_buf());
    let jobs = orbit_store::compose::workspace_job_run_store(
        follower.sqlite_store().unwrap(),
        follower.workspace_id().unwrap(),
    );
    Pair {
        _root: pair._root.clone(),
        wire: pair.wire.clone(),
        owner_repo: pair.owner_repo.clone(),
        follower,
        follower_repo: repo,
        follower_jobs: jobs,
        destination: pair.destination.clone(),
        tasks: pair.tasks.clone(),
    }
}

/// Try one local admission on another workspace, under the same host lock.
fn local_admission(follower: &OrbitRuntime, task: &str) -> bool {
    let lookup = ProfileLookup::new("ws-local", "change", Some("low".into()), false);
    follower
        .admit_against_forecast(|forecast| {
            let profile = forecast.profiles.lookup(&lookup).profile.unwrap();
            let fits = forecast.gap(75, 75).unwrap().fit(&profile).is_ok();
            Ok(ForecastDecision {
                outcome: fits,
                admitted: if fits {
                    vec![AdmissionReservationRequest {
                        task_id: task.into(),
                        lookup,
                    }]
                } else {
                    vec![]
                },
            })
        })
        .unwrap()
}

/// Interleave a local drain and another workspace's pull exactly between
/// the first pull's fit and owner response. No sleeps or host load are used.
#[test]
fn pending_pull_capacity_serializes_other_host_admitters_and_recovers_lost_replies() {
    if !isolated(
        module_path!(),
        "pending_pull_capacity_serializes_other_host_admitters_and_recovers_lost_replies",
    ) {
        return;
    }
    // Static proc files never advance ticks. Skip their sampling wait only;
    // real timestamps, raw reads and all pending-capacity transitions remain.
    crate::scenarios::with_immutable_host_load(|| {
        for (bytes, cores, lost) in [
            (12 * GIB, 1.0, false),
            (GIB, 6.0, false),
            (12 * GIB, 1.0, true),
        ] {
            let (pair, proc_root) = forecasting_using("enforce", 2, bytes);
            // Replace the fixture's history with this case's CPU curve.
            finished_delivery_using(&pair.follower, bytes, cores);
            neighbour_delivery(&pair.follower);
            host_using(&proc_root, 4 * GIB);
            let second = another_workspace(&pair, &proc_root);
            assert_ne!(
                second.follower.workspace_id().unwrap(),
                pair.follower.workspace_id().unwrap()
            );
            let second_drain = second.run_drain();
            let follower = pair.follower.clone();
            let wire = pair.wire.clone();
            let barrier = move || {
                let held = reservations(&follower);
                assert!(
                    !held.is_empty() && held.iter().all(|r| r.task_id.starts_with("pending:")),
                    "{held:?}"
                );
                let (cpu, memory) = follower
                    .admit_against_forecast(|forecast| {
                        assert!(
                            forecast.cpu.baseline.is_none(),
                            "static proc ticks leave CPU unknown"
                        );
                        Ok(ForecastDecision {
                            outcome: (forecast.cpu.peak.value, forecast.memory.peak.value),
                            admitted: vec![],
                        })
                    })
                    .unwrap();
                assert!(cpu <= 15.0 * 0.75 && memory <= (24 * GIB) as f64);
                assert!(
                    !local_admission(&follower, "local-in-flight"),
                    "local work cannot fill the pending pull's gap"
                );
                let before = wire.calls("orbit.task.pull").len();
                let pass = second.pass(&second_drain);
                assert_eq!(wire.calls("orbit.task.pull").len(), before, "{pass}");
                assert!(
                    second.leaf_runs().is_empty(),
                    "the second workspace must not execute"
                );
                Ok(())
            };
            *pair.wire.before_pull.lock().unwrap() = Some(Box::new(barrier));
            if lost {
                pair.wire.lose_next_reply("orbit.task.pull");
            }
            // Keep the leaf queued at bind so this fixture never starts a real
            // provider; it still exercises durable leaf creation and pricing.
            pair.wire.lose_next_reply("orbit.drain.claim.bind");
            let drain = pair.run_drain();
            let pass = pair.pass(&drain);
            if lost {
                assert!(error_of(&pass).contains("dropped"), "{pass}");
                assert!(pair.leaf_runs().is_empty());
                let pending = reservations(&pair.follower);
                assert!(
                    !pending.is_empty()
                        && pending.iter().all(|r| r.task_id.starts_with("pending:"))
                );
                assert!(!local_admission(&pair.follower, "local-before-expiry"));
                // Advance only the admission clock: the normal TTL path deletes
                // the lost request's reservation, without waiting five minutes.
                pair.follower
                    .admit_against_forecast_at(Utc::now() + ChronoDuration::minutes(6), |_| {
                        Ok(ForecastDecision {
                            outcome: (),
                            admitted: vec![],
                        })
                    })
                    .unwrap();
                assert!(reservations(&pair.follower).is_empty());
                assert!(local_admission(&pair.follower, "local-after-expiry"));
                let sends = pair.wire.calls("orbit.task.pull").len();
                pair.pass(&drain);
                assert_eq!(
                    pair.wire.calls("orbit.task.pull").len(),
                    sends,
                    "an expired request must reacquire capacity before re-send"
                );
                assert!(pair.leaf_runs().is_empty());
                let now = Utc::now();
                pair.follower
                    .sqlite_store()
                    .unwrap()
                    .admit_under_host_lock_replacing(
                        &AdmissionLoadQuery {
                            now,
                            delivery_jobs: &[],
                            reservations_expire_before: now - ChronoDuration::minutes(5),
                            resource_minutes_since: None,
                        },
                        &[("ws-local", "local-after-expiry")],
                        &mut |_| Ok(vec![]),
                    )
                    .unwrap();
                let follower = pair.follower.clone();
                *pair.wire.before_pull.lock().unwrap() = Some(Box::new(move || {
                    assert!(
                        reservations(&follower)
                            .iter()
                            .all(|r| r.task_id.starts_with("pending:"))
                    );
                    assert!(!local_admission(&follower, "local-on-retry"));
                    Ok(())
                }));
                pair.pass(&drain);
                let calls = pair.wire.calls("orbit.task.pull");
                assert_eq!(
                    calls[0]["request_id"], calls[1]["request_id"],
                    "retry the same owner claim"
                );
            }
            assert_eq!(pair.leaf_runs().len(), 1, "{pass}");
            let held = reservations(&pair.follower);
            assert_eq!(
                held.len(),
                1,
                "placeholder alternatives are replaced, not accumulated: {held:?}"
            );
            assert_eq!(held[0].task_id, pair.tasks[0]);
            let leaf = pair
                .follower_jobs
                .get_job_run(&pair.leaf_runs()[0])
                .unwrap()
                .unwrap();
            assert_eq!(
                leaf.input.as_ref().unwrap()["resource_class"]["complexity"],
                "low"
            );
            pair.follower
                .admit_against_forecast(|forecast| {
                    assert!(
                        forecast.reservations.is_empty(),
                        "leaf replaces precise reservation"
                    );
                    assert!(
                        forecast.cpu.baseline.is_none(),
                        "static proc ticks leave CPU unknown"
                    );
                    assert!(forecast.cpu.peak.value <= 15.0 * 0.75);
                    assert!(forecast.memory.peak.value <= (24 * GIB) as f64);
                    Ok(ForecastDecision {
                        outcome: (),
                        admitted: vec![],
                    })
                })
                .unwrap();
        }
    });
}

/// Definite idle/refusal and cancelled unknown requests release capacity.
#[test]
fn pending_pull_capacity_is_released_when_no_claim_will_execute() {
    if !isolated(
        module_path!(),
        "pending_pull_capacity_is_released_when_no_claim_will_execute",
    ) {
        return;
    }
    crate::scenarios::with_immutable_host_load(|| {
        for outcome in ["idle", "refusal", "cancel"] {
            let (pair, proc_root) = forecasting("enforce", 0);
            neighbour_delivery(&pair.follower);
            host_using(&proc_root, 4 * GIB);
            let drain = pair.run_drain();
            let follower = pair.follower.clone();
            let wire = pair.wire.clone();
            *pair.wire.before_pull.lock().unwrap() = Some(Box::new(move || {
                assert!(!reservations(&follower).is_empty());
                if outcome == "refusal" {
                    return Err(OrbitError::PolicyDenied(
                        "fixture owner refuses this request".into(),
                    ));
                }
                if outcome == "cancel" {
                    *wire.unreachable.lock().unwrap() = true;
                }
                Ok(())
            }));
            let result = pair.follower.run_deterministic("pull_refill", &json!({}), &json!({
            "run_id": drain, "destination": pair.destination, "window_expired": false, "max_active_leaf_runs": 1,
        }), ToolContext::default());
            if outcome == "refusal" {
                assert!(error_of(&result.unwrap()).contains("fixture owner refuses"));
            } else {
                result.unwrap();
            }
            if outcome == "cancel" {
                assert!(
                    !reservations(&pair.follower).is_empty(),
                    "unknown owner outcome retains capacity until recovery or TTL"
                );
                *pair.wire.unreachable.lock().unwrap() = false;
                // Exercise the drain's own graceful cancellation path.
                pair.follower
                    .cancel_job_run_with_options(&drain, "fixture", "fixture", None, false)
                    .unwrap();
                pair.pass(&drain);
            }
            assert!(reservations(&pair.follower).is_empty(), "{outcome}");
            assert!(pair.leaf_runs().is_empty());
        }
    });
}
// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    an_enforcing_follower_requests_only_work_its_forecast_has_room_for_scenarios,
    an_enforcing_follower_requests_only_work_its_forecast_has_room_for,
    shadow_and_off_followers_request_without_a_list
);
