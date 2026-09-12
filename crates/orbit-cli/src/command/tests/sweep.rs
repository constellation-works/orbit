//! `orbit sweep` output/reporting tests: the quiet-by-default
//! filtering that keeps the once-a-minute clock from growing its log, and the
//! stable `--json` shape machine consumers depend on.

use orbit_core::application::routines::{RoutineSweepReport, SweepOutcome};

use crate::command::sweep::{format_report_line, outcome_json, report_is_noteworthy};

fn report(action: &'static str) -> RoutineSweepReport {
    RoutineSweepReport {
        routine: "nightly".to_string(),
        source: "polaris".to_string(),
        origin: "committed",
        action,
        reason: None,
        slot: None,
        run_id: None,
    }
}

#[test]
fn noteworthy_actions_print_by_default_churn_does_not() {
    for action in ["fired", "retry_fired", "baselined", "error"] {
        assert!(
            report_is_noteworthy(action),
            "{action} should print by default"
        );
    }
    // The high-churn rows a healthy per-minute pass produces are suppressed.
    for action in ["skipped", "would_fire", "would_baseline"] {
        assert!(
            !report_is_noteworthy(action),
            "{action} should be quiet by default"
        );
    }
}

#[test]
fn format_report_line_includes_slot_and_run() {
    let mut r = report("fired");
    r.slot = Some("2026-01-01T00:01:00+00:00".to_string());
    r.run_id = Some("run-1".to_string());
    let line = format_report_line(&r);
    assert!(line.contains("nightly (polaris): fired"));
    assert!(line.contains("slot 2026-01-01T00:01:00+00:00"));
    assert!(line.contains("run run-1"));
}

#[test]
fn json_shape_is_stable() {
    let outcome = SweepOutcome {
        host_id: "dk-mac".to_string(),
        machine_id: "hm_dk_mac".to_string(),
        lock_busy: false,
        reports: vec![RoutineSweepReport {
            routine: "nightly".to_string(),
            source: "polaris".to_string(),
            origin: "committed",
            action: "fired",
            reason: None,
            slot: Some("2026-01-01T00:01:00+00:00".to_string()),
            run_id: Some("run-1".to_string()),
        }],
        load_errors: Vec::new(),
        no_workspace_loaded: None,
    };

    let value = outcome_json(&outcome, false);
    let object = value.as_object().expect("json object");
    for key in [
        "host_id",
        "machine_id",
        "dry_run",
        "lock_busy",
        "fired",
        "reports",
        "load_errors",
        "no_workspace_loaded",
    ] {
        assert!(object.contains_key(key), "missing top-level key {key}");
    }
    assert_eq!(object["fired"], 1);

    let first = &value["reports"][0];
    let report_obj = first.as_object().expect("report object");
    for key in [
        "routine", "source", "origin", "action", "reason", "slot", "run_id",
    ] {
        assert!(report_obj.contains_key(key), "missing report key {key}");
    }
    assert_eq!(first["action"], "fired");
    assert!(object["no_workspace_loaded"].is_null());
}

#[test]
fn json_includes_the_no_workspace_loaded_row() {
    let outcome = SweepOutcome {
        host_id: "dk-mac".to_string(),
        machine_id: "hm_dk_mac".to_string(),
        lock_busy: false,
        reports: Vec::new(),
        load_errors: Vec::new(),
        no_workspace_loaded: Some(
            "sweep.no_workspace_loaded: orbit 0.21.0 loaded 0/2 workspaces; first error [nebula]: schema migration failed"
                .to_string(),
        ),
    };

    let value = outcome_json(&outcome, false);
    assert_eq!(
        value["no_workspace_loaded"],
        "sweep.no_workspace_loaded: orbit 0.21.0 loaded 0/2 workspaces; first error [nebula]: schema migration failed"
    );
}
