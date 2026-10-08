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
