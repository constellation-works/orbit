//! Forecast maturity through the public read-only runtime API at a fixed time.

use chrono::{DateTime, Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::runtime::host_resource::accuracy::AccuracySelection;
use orbit_store::contracts::{
    AdmissionForecastPoint, AdmissionForecastRecord, HostResourceMinute, ResourceUsageStoreBackend,
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
    let selection = AccuracySelection::last_30_days(report_at);

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
                offset.unmeasured
            ),
            (0, 0, 0, 0)
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
