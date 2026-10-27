//! Forecast maturity through the public read-only runtime API at a fixed time.

use chrono::{DateTime, Duration, DurationRound, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::runtime::host_resource::accuracy::AccuracySelection;
use orbit_store::contracts::{
    AdmissionForecastPoint, AdmissionForecastRecord, ForecastDelivery, HostResourceMinute,
    ResourceUsageStoreBackend, RunResourceMinute,
};

#[test]
fn unfinished_minutes_stay_pending_and_only_mature_measurements_form_pairs() {
    if !super::dispatch_admission::isolated(
        "forecast_accuracy::unfinished_minutes_stay_pending_and_only_mature_measurements_form_pairs",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let store = runtime.sqlite_store().unwrap();
    let minute: DateTime<Utc> = "2026-10-15T12:00:00Z".parse().unwrap();
    let report_at = minute + Duration::seconds(30);
    let selection = AccuracySelection::since(report_at - Duration::days(30));

    for offset in [5, 10, 20, 30] {
        for (state, target) in [
            ("paired", minute - Duration::minutes(1)),
            ("missing", minute - Duration::minutes(2)),
            ("partial", minute),
            ("future", minute + Duration::minutes(5)),
        ] {
            store
                .record_admission_forecast(&AdmissionForecastRecord {
                    workspace_id: "ws_fixture".into(),
                    run_id: format!("jrun-{offset}-{state}"),
                    forecast_at: target - Duration::minutes(i64::from(offset))
                        + Duration::seconds(20),
                    mode: "shadow".into(),
                    cpu_count: Some(16),
                    memory_total_bytes: Some(32_000),
                    baseline_cpu_cores: None,
                    baseline_memory_bytes: None,
                    covered_deliveries: None,
                    points: vec![AdmissionForecastPoint {
                        offset_minutes: offset,
                        cpu_cores: 4.0,
                        memory_bytes: 8_000,
                    }],
                    admitted: vec![],
                    fit_admitted: vec![],
                    fit_skipped: vec![],
                    pull: None,
                })
                .unwrap();
        }
    }
    for (at, cpu, memory) in [
        (minute - Duration::minutes(1), 6.0, 6_000),
        // A current-minute row exists, but its values must not affect errors yet.
        (minute, 90.0, 30_000),
    ] {
        store
            .offer_host_resource_minute(&HostResourceMinute {
                minute_at: at,
                cpu_cores: cpu,
                cpu_count: 16,
                memory_bytes: memory,
                memory_total_bytes: 32_000,
                samples: 1,
                recorded_by_run_id: None,
            })
            .unwrap();
    }
    let before_forecasts = store.admission_forecasts(selection.since).unwrap();
    let before_minutes = store.host_resource_minutes(selection.since).unwrap();
    let report = runtime
        .forecast_accuracy_selected(selection.clone(), report_at)
        .unwrap();
    assert_eq!(report.report_at, report_at);
    assert_eq!(report.forecasts, 16);
    assert_eq!(report.pairs.len(), 4);
    assert_eq!(
        report
            .offsets
            .iter()
            .map(|offset| offset.offset_minutes)
            .collect::<Vec<_>>(),
        vec![5, 10, 20, 30]
    );
    for offset in &report.offsets {
        assert_eq!(
            (offset.paired, offset.pending, offset.missing),
            (1, 2, 1),
            "{offset:?}"
        );
        assert_eq!(offset.unmeasured, offset.pending + offset.missing);
        assert_eq!(
            offset.missing,
            offset.unsampled + offset.partial + offset.gap
        );
        assert_eq!(offset.gap, 0, "no delivery run is live in this fixture");
        assert_eq!(offset.paired + offset.pending + offset.missing, 4);
        assert_eq!(offset.cpu.mean_error, Some(-2.0));
        assert_eq!(offset.memory.mean_error, Some(2_000.0));
    }
    assert!(
        report
            .pairs
            .iter()
            .all(|pair| pair.minute_at == minute - Duration::minutes(1))
    );

    // Exactly at the end boundary the partial minute becomes an error pair.
    let matured = runtime
        .forecast_accuracy_selected(selection.clone(), minute + Duration::minutes(1))
        .unwrap();
    for offset in &matured.offsets {
        assert_eq!((offset.paired, offset.pending, offset.missing), (2, 1, 1));
        assert_eq!((offset.unsampled, offset.partial, offset.gap), (1, 0, 0));
        assert_eq!(offset.unmeasured, offset.pending + offset.missing);
        assert_eq!(offset.paired + offset.pending + offset.missing, 4);
        assert_eq!(offset.cpu.mean_error, Some(-44.0));
    }
    // Once the future minute ends without a row, it moves from pending to missing.
    let missing = runtime
        .forecast_accuracy_selected(selection.clone(), minute + Duration::minutes(6))
        .unwrap();
    for offset in &missing.offsets {
        assert_eq!((offset.paired, offset.pending, offset.missing), (2, 0, 2));
        assert_eq!((offset.unsampled, offset.partial, offset.gap), (2, 0, 0));
        assert_eq!(offset.unmeasured, offset.pending + offset.missing);
        assert_eq!(offset.paired + offset.pending + offset.missing, 4);
    }
    let mut empty = selection.clone();
    empty.drain = Some("jrun-absent".into());
    let empty = runtime
        .forecast_accuracy_selected(empty, report_at)
        .unwrap();
    assert_eq!(empty.forecasts, 0);
    assert!(empty.pairs.is_empty() && empty.drains.is_empty());
    for offset in empty.offsets {
        assert_eq!(
            (
                offset.paired,
                offset.pending,
                offset.missing,
                offset.unsampled,
                offset.partial,
                offset.gap,
                offset.unmeasured
            ),
            (0, 0, 0, 0, 0, 0, 0)
        );
        assert!(offset.cpu.mean_error.is_none() && offset.memory.worst_miss.is_none());
    }
    assert_eq!(
        store.admission_forecasts(selection.since).unwrap(),
        before_forecasts
    );
    assert_eq!(
        store.host_resource_minutes(selection.since).unwrap(),
        before_minutes
    );
}

/// A month without drains must not empty the report: the default selection is
/// the newest recorded forecasts, whatever their age, and `last` narrows it
/// to a count.
#[test]
fn default_selection_reads_the_newest_forecasts_after_a_break() {
    if !super::dispatch_admission::isolated(
        "forecast_accuracy::default_selection_reads_the_newest_forecasts_after_a_break",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let store = runtime.sqlite_store().unwrap();
    let report_at: DateTime<Utc> = "2026-10-27T12:00:00Z".parse().unwrap();
    // The newest drain ran 45 days before the report.
    let last_drain = report_at - Duration::days(45);
    for (index, run_id) in ["jrun-older", "jrun-newest"].into_iter().enumerate() {
        let minute = last_drain - Duration::hours(1) + Duration::hours(index as i64);
        store
            .record_admission_forecast(&AdmissionForecastRecord {
                workspace_id: "ws_fixture".into(),
                run_id: run_id.into(),
                forecast_at: minute + Duration::seconds(10),
                mode: "shadow".into(),
                cpu_count: Some(16),
                memory_total_bytes: Some(32_000),
                baseline_cpu_cores: None,
                baseline_memory_bytes: None,
                covered_deliveries: None,
                points: vec![AdmissionForecastPoint {
                    offset_minutes: 5,
                    cpu_cores: 4.0,
                    memory_bytes: 8_000,
                }],
                admitted: vec![],
                fit_admitted: vec![],
                fit_skipped: vec![],
                pull: None,
            })
            .unwrap();
        store
            .offer_host_resource_minute(&HostResourceMinute {
                minute_at: minute + Duration::minutes(5),
                cpu_cores: 5.0,
                cpu_count: 16,
                memory_bytes: 8_000,
                memory_total_bytes: 32_000,
                samples: 1,
                recorded_by_run_id: None,
            })
            .unwrap();
    }

    let latest = runtime
        .forecast_accuracy_selected(AccuracySelection::latest(), report_at)
        .unwrap();
    assert_eq!(latest.forecasts, 2, "the break must not empty the report");
    assert_eq!(latest.pairs.len(), 2);
    assert_eq!(
        latest.selection.since,
        last_drain - Duration::hours(1) + Duration::seconds(10),
        "the effective selection reaches back to the oldest forecast it read"
    );

    let last_one = runtime
        .forecast_accuracy_selected(AccuracySelection::last(1), report_at)
        .unwrap();
    assert_eq!(last_one.forecasts, 1);
    assert_eq!(last_one.pairs[0].run_id, "jrun-newest");

    let calendar = runtime
        .forecast_accuracy_selected(
            AccuracySelection::since(report_at - Duration::days(30)),
            report_at,
        )
        .unwrap();
    assert_eq!(calendar.forecasts, 0, "a time window still selects by time");
}

#[test]
fn gc_retained_forecast_excludes_missing_worker_attribution() {
    if !super::dispatch_admission::isolated(
        "forecast_accuracy::gc_retained_forecast_excludes_missing_worker_attribution",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let store = runtime.sqlite_store().unwrap();
    let forecast_at = (Utc::now() - Duration::days(45))
        .duration_trunc(Duration::minutes(1))
        .unwrap();
    let minute = forecast_at + Duration::minutes(5);
    for index in 0..201 {
        let admitted = forecast_at + Duration::minutes(index);
        store
            .record_run_resource_minute(&RunResourceMinute {
                workspace_id: "ws_fixture".into(),
                run_id: format!("jrun-worker-{index}"),
                minute_index: 0,
                minute_started_at: if index == 0 { minute } else { admitted },
                task_id: format!("task-{index}"),
                work_class: "change".into(),
                complexity: Some("hard".into()),
                resumed: false,
                delivery_admitted_at: Some(admitted),
                cpu_cores: 4.0,
                cpu_accounting: None,
                memory_bytes: 4_000,
                samples: 12,
                crew_model: None,
                orbit_version: None,
            })
            .unwrap();
    }
    store
        .record_admission_forecast(&AdmissionForecastRecord {
            workspace_id: "ws_fixture".into(),
            run_id: "jrun-forecast".into(),
            forecast_at,
            mode: "shadow".into(),
            cpu_count: Some(16),
            memory_total_bytes: Some(32_000),
            baseline_cpu_cores: Some(1.0),
            baseline_memory_bytes: Some(1_000),
            covered_deliveries: Some(vec![ForecastDelivery {
                workspace_id: "ws_fixture".into(),
                task_id: "task-0".into(),
                admitted_at: forecast_at,
                run_ids: vec!["jrun-worker-0".into()],
            }]),
            points: vec![AdmissionForecastPoint {
                offset_minutes: 5,
                cpu_cores: 5.0,
                memory_bytes: 5_000,
            }],
            admitted: vec![],
            fit_admitted: vec![],
            fit_skipped: vec![],
            pull: None,
        })
        .unwrap();
    store
        .offer_host_resource_minute(&HostResourceMinute {
            minute_at: minute,
            cpu_cores: 6.0,
            cpu_count: 16,
            memory_bytes: 6_000,
            memory_total_bytes: 32_000,
            samples: 12,
            recorded_by_run_id: None,
        })
        .unwrap();
    let report = || {
        runtime
            .forecast_accuracy_selected(AccuracySelection::latest(), Utc::now())
            .unwrap()
    };
    let before = report();
    assert_eq!(before.pairs[0].attributed_cpu_cores, Some(5.0));
    assert_eq!(before.pairs[0].attributed_memory_bytes, Some(5_000.0));
    runtime.gc_runs(true, Some(30)).unwrap();
    assert!(
        !store
            .delivery_run_resources(forecast_at)
            .unwrap()
            .iter()
            .any(|run| run.run_id == "jrun-worker-0")
    );
    let after = report();
    assert_eq!(after.pairs.len(), 1);
    assert_eq!(after.pairs[0].measured_cpu_cores, 6.0);
    assert_eq!(after.pairs[0].measured_memory_bytes, 6_000);
    assert_eq!(after.pairs[0].attributed_cpu_cores, None);
    assert_eq!(after.pairs[0].attributed_memory_bytes, None);
    let offset = after
        .offsets
        .iter()
        .find(|offset| offset.offset_minutes == 5)
        .unwrap();
    assert_eq!((offset.paired, offset.attributed_paired), (1, 0));
    assert_eq!(offset.cpu.mean_error, Some(-1.0));
    assert!(offset.attributed_cpu.mean_error.is_none());
    assert!(offset.attributed_memory.mean_error.is_none());
}
