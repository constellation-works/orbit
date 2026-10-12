use super::super::pressure::ResourcePressureEvaluator;
use super::super::{DiskSample, ResourceSeverity};
use super::fixtures::sample;
use chrono::Duration;
use orbit_config::{ConfigScope, ConfigSnapshot, ConfigStore};
use std::path::PathBuf;

#[test]
fn sustained_pressure_holds_until_below_resume_for_each_resource() {
    let settings = ConfigSnapshot::default().resource_throttle();
    for resource in 0..3 {
        let (high, resume) = match resource {
            0 => (settings.cpu_high_percent, settings.cpu_resume_percent),
            1 => (settings.memory_high_percent, settings.memory_resume_percent),
            _ => (settings.disk_high_percent, settings.disk_resume_percent),
        };
        let mut evaluator = ResourcePressureEvaluator::default();
        let mut evaluate = |second, value| {
            let mut values = [Some(0.0); 3];
            values[resource] = value;
            let sample = sample(second, values[0], values[1], values[2]);
            let now = sample.sampled_at;
            evaluator.evaluate(sample, &settings, now)
        };
        assert!(!evaluate(0, Some(f64::from(high))).throttle);
        assert!(!evaluate(5, Some(f64::from(high))).throttle);
        let held = evaluate(10, Some(f64::from(high)));
        assert!(held.throttle);
        assert_eq!(held.severity, ResourceSeverity::Critical);
        assert!(!held.reason.is_empty());
        assert!(evaluate(11, Some(f64::from(resume))).throttle);
        assert!(!evaluate(12, Some(f64::from(resume) - 0.1)).throttle);
        // A spike interrupted by the hysteresis band never becomes sustained.
        assert!(!evaluate(13, Some(f64::from(high))).throttle);
        assert!(!evaluate(18, Some(f64::from(resume))).throttle);
        assert!(!evaluate(23, Some(f64::from(high))).throttle);
        // Losing the signal releases that hold; it is not labelled healthy.
        let unknown = evaluate(24, None);
        assert!(!unknown.throttle);
        assert_eq!(unknown.severity, ResourceSeverity::Unknown);
    }
}

#[test]
fn stale_invalid_disabled_and_observation_gaps_fail_open() {
    let mut settings = ConfigSnapshot::default().resource_throttle();
    let mut evaluator = ResourcePressureEvaluator::default();
    let high = sample(0, Some(500.0), Some(95.0), Some(99.0));
    assert!(
        !evaluator
            .evaluate(high.clone(), &settings, high.sampled_at)
            .throttle
    );
    let late = sample(20, Some(500.0), Some(95.0), Some(99.0));
    assert!(
        !evaluator
            .evaluate(late.clone(), &settings, late.sampled_at)
            .throttle,
        "an unobserved gap must not establish sustained pressure"
    );
    let held = sample(30, Some(500.0), Some(95.0), Some(99.0));
    assert!(
        evaluator
            .evaluate(held.clone(), &settings, held.sampled_at)
            .throttle
    );
    let stale = evaluator.evaluate(
        held.clone(),
        &settings,
        held.sampled_at + Duration::seconds(16),
    );
    assert!(!stale.throttle);
    assert!(stale.stale);
    assert_eq!(stale.cpu.unknown_reason, Some("stale"));
    let invalid = sample(31, Some(f64::NAN), Some(101.0), Some(-1.0));
    assert_eq!(
        evaluator
            .evaluate(invalid.clone(), &settings, invalid.sampled_at)
            .severity,
        ResourceSeverity::Unknown
    );
    settings.enabled = false;
    for second in [40, 45, 50] {
        let high = sample(second, Some(100.0), Some(100.0), Some(100.0));
        let disabled = evaluator.evaluate(high.clone(), &settings, high.sampled_at);
        assert!(!disabled.throttle);
        assert_eq!(disabled.severity, ResourceSeverity::Critical);
    }
}

#[test]
fn config_admission_rejects_invalid_thresholds_and_exposes_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    for body in [
        "cpu_high_percent = 0",
        "disk_resume_percent = 101",
        "memory_resume_percent = 95\nmemory_high_percent = 90",
    ] {
        std::fs::write(&path, format!("[workflow.resource_throttle]\n{body}")).unwrap();
        assert!(
            ConfigStore::open(ConfigScope::Global, &path)
                .and_then(|store| store.snapshot())
                .is_err()
        );
    }
    std::fs::write(&path, "[workflow.resource_throttle]\nenabled = false\ncpu_high_percent = 97\ncpu_resume_percent = 65").unwrap();
    let config = ConfigStore::open(ConfigScope::Global, &path)
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(
        config.value_for("workflow.resource_throttle.cpu_high_percent"),
        Some(serde_json::json!(97))
    );
    assert_eq!(config.resource_throttle().cpu_resume_percent, 65);
    assert!(!config.resource_throttle().enabled);
}

#[test]
fn disk_aggregation_keeps_all_path_hysteresis_but_reports_one_held_disk() {
    let settings = ConfigSnapshot::default().resource_throttle();
    for count in [0, 1, 22] {
        let mut evaluator = ResourcePressureEvaluator::default();
        let mut evaluate = |second, values: Vec<Option<f64>>, stale| {
            let mut sample = sample(second, Some(10.0), Some(10.0), None);
            sample.disks = values
                .into_iter()
                .enumerate()
                .map(|(index, used_percent)| DiskSample {
                    path: PathBuf::from(format!("/watched/{index}")),
                    used_percent,
                })
                .collect();
            let now = sample.sampled_at + Duration::seconds(if stale { 16 } else { 0 });
            evaluator.evaluate(sample, &settings, now)
        };
        // Unknown paths cannot hide the highest known reading.
        let mut values = vec![None; count];
        if let Some(last) = values.last_mut() {
            *last = Some(95.0);
        }
        let first = evaluate(0, values.clone(), false);
        assert_eq!(first.disk.is_some(), count > 0);
        if let Some(disk) = &first.disk {
            assert_eq!(disk.reading.percent, Some(95.0));
            assert_eq!(disk.reading.severity, ResourceSeverity::Critical);
            assert_eq!(disk.path, PathBuf::from(format!("/watched/{}", count - 1)));
        }
        assert!(!first.throttle);
        evaluate(5, values.clone(), false);
        let held = evaluate(10, values, false);
        assert_eq!(
            held.throttle,
            count > 0,
            "any watched path must still hold admission"
        );
        if count > 0 {
            // All paths now go high, then sustain; the warning remains one disk.
            evaluate(11, vec![Some(96.0); count], false);
            evaluate(16, vec![Some(96.0); count], false);
            let held = evaluate(21, vec![Some(96.0); count], false);
            assert_eq!(held.pressures.len(), 1);
            assert_eq!(held.reason.matches("disk ").count(), 1);
            let throttle = orbit_types::workflow::ResourceThrottle {
                resources: held.pressures,
            };
            assert_eq!(
                throttle.describe().matches("disk ").count(),
                1,
                "CLI/MCP share the deduplicated pressure warning"
            );
            // A previously held path stays held in the hysteresis band even
            // when a different path has a higher, newly observed reading.
            let mut values = vec![Some(0.0); count];
            values[count - 1] = Some(f64::from(settings.disk_resume_percent));
            evaluate(22, values.clone(), false);
            if count > 1 {
                values[0] = Some(99.0);
            }
            let band = evaluate(23, values.clone(), false);
            assert!(band.throttle);
            assert_eq!(band.pressures.len(), 1);
            assert!(
                band.pressures[0]
                    .resource
                    .ends_with(&format!("/{}", count - 1))
            );
            assert_eq!(
                band.disk.unwrap().reading.percent,
                Some(if count > 1 {
                    99.0
                } else {
                    f64::from(settings.disk_resume_percent)
                })
            );
            let stale = evaluate(24, values, true);
            assert!(stale.disk.is_none());
            assert!(!stale.throttle);
        }
        let unknown = evaluate(40, vec![None; count], false);
        assert!(unknown.disk.is_none());
        assert!(!unknown.throttle);
    }
}

// Deterministic interleaving through the schema-1 shared file, without ambient
// host pressure or sleeps. Each pass starts with a fresh evaluator, as separate
// workspace processes do.
fn evaluate_shared_file(
    path: &std::path::Path,
    settings: &orbit_config::ResourceThrottleSettings,
    second: i64,
    values: [f64; 3],
) -> super::super::HostResourceStatus {
    let mut evaluator = match std::fs::read(path) {
        Ok(encoded) => {
            let history: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(history["schema_version"], 1);
            serde_json::from_value::<ResourcePressureEvaluator>(history["evaluator"].clone())
                .unwrap()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ResourcePressureEvaluator::default()
        }
        Err(error) => panic!("cannot read shared pressure fixture: {error}"),
    };
    let sample = sample(second, Some(values[0]), Some(values[1]), Some(values[2]));
    let now = sample.sampled_at;
    let status = evaluator.evaluate(sample, settings, now);
    std::fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "evaluator": evaluator,
            "cpu_ticks": super::super::HostCpuTickHistory::default(),
        }))
        .unwrap(),
    )
    .unwrap();
    status
}

#[test]
fn interleaved_threshold_sets_keep_independent_sustain_windows_and_holds() {
    for resource in 0..3 {
        // Both halves of the threshold pair participate in the key.
        for (other_high, other_resume) in [(95, 85), (90, 80)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("host-resource-pressure.json");
            let mut settings = ConfigSnapshot::default().resource_throttle();
            settings.cpu_high_percent = 90;
            settings.cpu_resume_percent = 85;
            settings.memory_high_percent = 90;
            settings.memory_resume_percent = 85;
            settings.disk_high_percent = 90;
            settings.disk_resume_percent = 85;
            let mut other = settings.clone();
            match resource {
                0 => {
                    other.cpu_high_percent = other_high;
                    other.cpu_resume_percent = other_resume;
                }
                1 => {
                    other.memory_high_percent = other_high;
                    other.memory_resume_percent = other_resume;
                }
                _ => {
                    other.disk_high_percent = other_high;
                    other.disk_resume_percent = other_resume;
                }
            }
            let mut values = [0.0; 3];
            for second in [0, 5, 10, 15] {
                // The higher high mark starts its own sustain window five
                // seconds later; the lower mark must still hold at ten.
                values[resource] = if second == 0 { 92.0 } else { 99.0 };
                for (workspace, since) in [
                    (&settings, 0),
                    (&other, if other_high == 95 { 5 } else { 0 }),
                ] {
                    let status = evaluate_shared_file(&path, workspace, second, values);
                    assert_eq!(status.throttle, second >= since + 10);
                    if status.throttle {
                        assert_eq!(status.pressures.len(), 1);
                        assert_eq!(
                            status.pressures[0].since,
                            sample(since, None, None, None).sampled_at
                        );
                    }
                }
            }
            // Consulting another threshold set cannot release a held resource
            // in its hysteresis band, even below its own high mark.
            values[resource] = 87.0;
            for workspace in [&settings, &other, &settings] {
                assert!(evaluate_shared_file(&path, workspace, 16, values).throttle);
            }
            if other_resume == 80 {
                values[resource] = 82.0;
                assert!(!evaluate_shared_file(&path, &settings, 17, values).throttle);
                assert!(evaluate_shared_file(&path, &other, 17, values).throttle);
            }
            // Retention and observation-gap limits apply to each threshold key.
            values[resource] = 99.0;
            assert!(!evaluate_shared_file(&path, &settings, 33, values).throttle);
            assert!(!evaluate_shared_file(&path, &other, 33, values).throttle);
        }
    }
}

#[test]
fn identical_thresholds_share_sustain_history_across_workspace_passes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host-resource-pressure.json");
    let first_workspace = ConfigSnapshot::default().resource_throttle();
    let second_workspace = first_workspace.clone();
    let values = [100.0; 3];
    assert!(!evaluate_shared_file(&path, &first_workspace, 0, values).throttle);
    assert!(!evaluate_shared_file(&path, &second_workspace, 5, values).throttle);
    let held = evaluate_shared_file(&path, &first_workspace, 10, values);
    assert_eq!(held.pressures.len(), 3);
    let band = [
        f64::from(second_workspace.cpu_resume_percent),
        f64::from(second_workspace.memory_resume_percent),
        f64::from(second_workspace.disk_resume_percent),
    ];
    assert_eq!(
        evaluate_shared_file(&path, &second_workspace, 11, band)
            .pressures
            .len(),
        3
    );
    assert!(!evaluate_shared_file(&path, &second_workspace, 12, [0.0; 3]).throttle);
    assert!(!evaluate_shared_file(&path, &first_workspace, 12, band).throttle);
}

#[test]
fn schema_one_readers_start_empty_on_new_keys_and_matching_legacy_history_migrates() {
    // Frozen previous-reader persisted contract: string-keyed states with these
    // required fields. Its evaluator looks up only unqualified resource names.
    #[derive(serde::Deserialize)]
    struct PreviousState {
        last_sample: Option<chrono::DateTime<chrono::Utc>>,
        high_since: Option<chrono::DateTime<chrono::Utc>>,
        held: bool,
        high_percent: u8,
        resume_percent: u8,
    }
    #[derive(serde::Deserialize)]
    struct PreviousEvaluator {
        states: std::collections::BTreeMap<String, PreviousState>,
    }
    #[derive(serde::Deserialize)]
    struct PreviousHistory {
        schema_version: u8,
        evaluator: PreviousEvaluator,
        #[serde(default)]
        cpu_ticks: super::super::HostCpuTickHistory,
    }

    let settings = ConfigSnapshot::default().resource_throttle();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host-resource-pressure.json");
    let names = ["cpu", "memory", "disk /workspace"];
    let thresholds = [
        (settings.cpu_high_percent, settings.cpu_resume_percent),
        (settings.memory_high_percent, settings.memory_resume_percent),
        (settings.disk_high_percent, settings.disk_resume_percent),
    ];
    for legacy_held in [false, true] {
        let since = sample(0, None, None, None).sampled_at;
        let last = sample(5, None, None, None).sampled_at;
        let legacy_states: serde_json::Map<String, serde_json::Value> = names
            .iter()
            .zip(thresholds)
            .map(|(name, (high, resume))| {
                (
                    (*name).into(),
                    serde_json::json!({
                        "last_sample": last,
                        "high_since": since,
                        "held": legacy_held,
                        "high_percent": high,
                        "resume_percent": resume,
                    }),
                )
            })
            .collect();
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "evaluator": { "states": legacy_states },
            }))
            .unwrap(),
        )
        .unwrap();
        let mut different = settings.clone();
        different.cpu_high_percent = if settings.cpu_high_percent == 100 {
            99
        } else {
            100
        };
        let other = evaluate_shared_file(&path, &different, 5, [100.0; 3]);
        assert!(
            other
                .pressures
                .iter()
                .all(|pressure| pressure.resource != "cpu"),
            "a mismatching threshold set must not inherit a legacy CPU hold"
        );
        let values = if legacy_held {
            thresholds.map(|(_, resume)| f64::from(resume))
        } else {
            [100.0; 3]
        };
        let migrated = evaluate_shared_file(&path, &settings, 10, values);
        assert_eq!(migrated.pressures.len(), 3);
        assert!(
            migrated
                .pressures
                .iter()
                .all(|pressure| pressure.since == since)
        );

        let encoded = std::fs::read(&path).unwrap();
        // The previous read_history size and schema checks still accept the file.
        assert!(encoded.len() <= 64 * 1024);
        let previous: PreviousHistory = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(previous.schema_version, 1);
        let _cpu_ticks = previous.cpu_ticks;
        for (name, (high, resume)) in names.iter().zip(thresholds) {
            assert!(
                !previous.evaluator.states.contains_key(*name),
                "previous readers must start fresh rather than reuse another threshold set"
            );
            let state = &previous.evaluator.states[&format!("{high}/{resume}@{name}")];
            assert_eq!(state.last_sample, Some(migrated.sampled_at));
            assert_eq!(state.high_since, Some(since));
            assert!(state.held);
            assert_eq!((state.high_percent, state.resume_percent), (high, resume));
        }
    }
}
