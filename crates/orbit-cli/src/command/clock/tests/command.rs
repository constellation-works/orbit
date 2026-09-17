//! `orbit clock status` output tests: the JSON record `--format json` emits
//! and the human one-liner, built from a hand-made `ClockStatus` so no native
//! manager (`launchctl list`, `systemctl`) is consulted.

use std::path::PathBuf;

use orbit_core::application::routines::{ClockStatus, ClockUnitInspection, ClockUnitVerdict};
use serde_json::{Value, json};

use super::super::command::{clock_status_doc, clock_status_payload, clock_status_text};
use crate::output::payload::{Block, View};

fn enabled_status() -> ClockStatus {
    ClockStatus {
        configured_cadence_seconds: 60,
        effective_cadence_seconds: Some(60),
        enabled: true,
        loaded: true,
        running: Some(true),
        schedulable: true,
        health_issue: None,
        last_tick_at: None,
        next_tick_at: Some("Tue 2026-09-15 10:01:00 UTC".to_string()),
        platform: "launchd",
    }
}

fn paused_status() -> ClockStatus {
    ClockStatus {
        configured_cadence_seconds: 300,
        effective_cadence_seconds: None,
        enabled: false,
        loaded: false,
        running: None,
        schedulable: false,
        health_issue: None,
        last_tick_at: None,
        next_tick_at: None,
        platform: "systemd",
    }
}

fn matching_unit() -> ClockUnitInspection {
    ClockUnitInspection {
        unit_path: Some(PathBuf::from(
            "/home/op/.config/systemd/user/orbit-sweep.service",
        )),
        program_path: Some(PathBuf::from("/home/op/.orbit/bin/orbit")),
        program_version: Some("0.23.0".to_string()),
        running_path: PathBuf::from("/home/op/.orbit/bin/orbit"),
        running_version: "0.23.0".to_string(),
        verdict: ClockUnitVerdict::Matching,
    }
}

#[test]
fn json_document_serializes_every_clock_status_field() {
    let doc = clock_status_doc(&enabled_status(), None);
    assert_eq!(
        doc,
        json!({
            "state": "enabled",
            "enabled": true,
            "loaded": true,
            "running": true,
            "schedulable": true,
            "configured_cadence_seconds": 60,
            "effective_cadence_seconds": 60,
            "platform": "launchd",
            "health_issue": null,
            "last_tick_at": null,
            "next_tick_at": "Tue 2026-09-15 10:01:00 UTC",
            "program": null,
        })
    );
}

#[test]
fn json_document_reports_a_paused_clock_with_inactive_cadence() {
    let doc = clock_status_doc(&paused_status(), None);
    assert_eq!(doc["state"], "paused");
    assert_eq!(doc["enabled"], false);
    assert_eq!(doc["configured_cadence_seconds"], 300);
    assert_eq!(doc["effective_cadence_seconds"], Value::Null);
    assert_eq!(doc["running"], Value::Null);
    assert_eq!(doc["platform"], "systemd");
}

#[test]
fn json_document_flags_an_enabled_clock_without_a_trigger_as_unhealthy() {
    let mut status = enabled_status();
    status.schedulable = false;
    status.health_issue = Some("launchd reports no next trigger".to_string());
    let doc = clock_status_doc(&status, None);
    assert_eq!(doc["state"], "unhealthy");
    assert_eq!(doc["health_issue"], "launchd reports no next trigger");
}

#[test]
fn json_document_nests_the_installed_unit_program() {
    let doc = clock_status_doc(&enabled_status(), Some(&matching_unit()));
    assert_eq!(
        doc["program"],
        json!({
            "unit_path": "/home/op/.config/systemd/user/orbit-sweep.service",
            "path": "/home/op/.orbit/bin/orbit",
            "version": "0.23.0",
            "running_path": "/home/op/.orbit/bin/orbit",
            "running_version": "0.23.0",
            "verdict": "matching",
            "reason": null,
        })
    );

    let mut unit = matching_unit();
    unit.verdict = ClockUnitVerdict::Unrunnable {
        reason: "permission denied".to_string(),
    };
    let doc = clock_status_doc(&enabled_status(), Some(&unit));
    assert_eq!(doc["program"]["verdict"], "unrunnable");
    assert_eq!(doc["program"]["reason"], "permission denied");
}

#[test]
fn human_line_keeps_the_existing_shape() {
    assert_eq!(
        clock_status_text(&enabled_status(), Some(&matching_unit())),
        "clock: enabled | configured cadence: 60s | effective cadence: 60s | platform: launchd \
         | program: /home/op/.orbit/bin/orbit 0.23.0"
    );
    assert_eq!(
        clock_status_text(&paused_status(), None),
        "clock: paused | configured cadence: 300s | effective cadence: inactive | platform: systemd"
    );
}

#[test]
fn human_view_appends_the_health_line_after_the_summary() {
    let mut status = enabled_status();
    status.schedulable = false;
    status.health_issue = Some("launchd reports no next trigger".to_string());
    let text = clock_status_text(&status, None);
    let mut lines = text.lines();
    assert!(
        lines
            .next()
            .is_some_and(|line| line.starts_with("clock: unhealthy | ")),
        "{text}"
    );
    assert_eq!(
        lines.next(),
        Some("clock health: launchd reports no next trigger")
    );
    assert_eq!(lines.next(), None);
}

#[test]
fn payload_pairs_the_document_with_a_single_text_block() {
    let status = enabled_status();
    let (doc, view) = clock_status_payload(&status, None).into_view();
    assert_eq!(doc, clock_status_doc(&status, None));
    match view {
        View::Blocks(blocks) => match blocks.as_slice() {
            [Block::Text(text)] => assert_eq!(*text, clock_status_text(&status, None)),
            other => panic!("expected one text block, got {other:?}"),
        },
        other => panic!("expected a blocks view, got {other:?}"),
    }
}
