use orbit_core::AuditStats;
use serde_json::json;

use super::super::stats::{denied_operations_section, scoped_to_tool, stats_to_json};

fn breakdown() -> Vec<(String, i64)> {
    vec![
        ("workspace teardown".to_string(), 12),
        ("orbit.task.locks.release".to_string(), 2),
    ]
}

fn stats() -> AuditStats {
    AuditStats {
        total: 24356,
        success_count: 22000,
        failure_count: 629,
        denied_count: 1727,
        avg_duration_ms: 12.25,
        p95_duration_ms: 40,
        max_duration_ms: 900,
    }
}

/// [ORB-12257] The denial breakdown is what makes 1,727 refusals readable, so
/// it has to name each operation next to its count.
#[test]
fn the_denial_section_names_each_operation_with_its_count() {
    assert_eq!(
        denied_operations_section(&breakdown()),
        "\nDenied governed operations:\n      12  workspace teardown\n       2  orbit.task.locks.release"
    );
}

/// Nothing refused means no section at all, rather than an empty heading
/// hanging off the totals.
#[test]
fn the_denial_section_is_absent_when_nothing_was_denied() {
    assert!(denied_operations_section(&[]).is_empty());
}

/// `--tool` scopes the totals, so it must scope the breakdown too: a
/// tool-scoped report that lists every operation in the store is reporting on
/// two different populations under one heading.
#[test]
fn a_tool_filter_scopes_the_breakdown_to_that_operation() {
    assert_eq!(
        scoped_to_tool(breakdown(), Some("orbit.task.locks.release")),
        vec![("orbit.task.locks.release".to_string(), 2)]
    );
    assert!(scoped_to_tool(breakdown(), Some("orbit.task.delete")).is_empty());
    assert_eq!(scoped_to_tool(breakdown(), None), breakdown());
}

#[test]
fn the_json_projection_carries_the_breakdown() {
    assert_eq!(
        stats_to_json(&stats(), &breakdown()),
        json!({
            "total": 24356,
            "success_count": 22000,
            "failure_count": 629,
            "denied_count": 1727,
            "avg_duration_ms": 12.25,
            "p95_duration_ms": 40,
            "max_duration_ms": 900,
            "denied_by_operation": [
                { "operation": "workspace teardown", "count": 12 },
                { "operation": "orbit.task.locks.release", "count": 2 },
            ],
        })
    );
}
