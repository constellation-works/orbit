use crate::Store;
use orbit_common::test_fixtures::{TEST_CLAUDE_MODEL, TEST_CODEX_MODEL};

use super::sample_params_with;
use crate::AuditToolCallCountsByRole;
use crate::AuditToolCallCountsBySurfaceAndRole;
use crate::AuditTopToolCall;
use orbit_types::telemetry::AuditEventStatus;

#[test]
fn tool_call_counts_by_role_include_failed_and_denied_runs() {
    let store = Store::open_in_memory().expect("open store");

    for params in [
        sample_params_with("exec-success", "codex / gpt-5", AuditEventStatus::Success),
        sample_params_with("exec-failure", "codex / gpt-5", AuditEventStatus::Failure),
        sample_params_with("exec-denied", "codex / gpt-5", AuditEventStatus::Denied),
    ] {
        store
            .insert_audit_event_record(&params)
            .expect("insert audit event");
    }

    let mut non_run = sample_params_with("exec-show", "codex / gpt-5", AuditEventStatus::Failure);
    non_run.subcommand = Some("show".to_string());
    store
        .insert_audit_event_record(&non_run)
        .expect("insert non-run audit event");

    let rows = store
        .get_audit_tool_call_counts_by_role(None)
        .expect("load tool call counts");

    assert_eq!(
        rows,
        vec![AuditToolCallCountsByRole {
            role: "codex / gpt-5".to_string(),
            total: 3,
            failed: 2,
        }]
    );
}

#[test]
fn tool_call_counts_by_surface_and_role_extract_segment_after_orbit_prefix() {
    let store = Store::open_in_memory().expect("open store");

    let mut adr_show = sample_params_with(
        "exec-adr-show-1",
        TEST_CLAUDE_MODEL,
        AuditEventStatus::Success,
    );
    adr_show.tool_name = Some("orbit.adr.show".to_string());
    adr_show.target_id = Some("orbit.adr.show".to_string());
    store.insert_audit_event_record(&adr_show).expect("insert");

    let mut adr_show_failed = sample_params_with(
        "exec-adr-show-2",
        TEST_CLAUDE_MODEL,
        AuditEventStatus::Failure,
    );
    adr_show_failed.tool_name = Some("orbit.adr.show".to_string());
    adr_show_failed.target_id = Some("orbit.adr.show".to_string());
    store
        .insert_audit_event_record(&adr_show_failed)
        .expect("insert");

    let mut config_show = sample_params_with(
        "exec-config-show",
        TEST_CODEX_MODEL,
        AuditEventStatus::Success,
    );
    config_show.tool_name = Some("orbit.config.show".to_string());
    config_show.target_id = Some("orbit.config.show".to_string());
    store
        .insert_audit_event_record(&config_show)
        .expect("insert");

    let mut task_update = sample_params_with(
        "exec-task-update",
        TEST_CODEX_MODEL,
        AuditEventStatus::Success,
    );
    task_update.tool_name = Some("orbit.task.update".to_string());
    task_update.target_id = Some("orbit.task.update".to_string());
    store
        .insert_audit_event_record(&task_update)
        .expect("insert");

    // Non-orbit tool name must be excluded.
    let mut external = sample_params_with(
        "exec-external",
        TEST_CLAUDE_MODEL,
        AuditEventStatus::Success,
    );
    external.tool_name = Some("github.create_pr".to_string());
    external.target_id = Some("github.create_pr".to_string());
    store.insert_audit_event_record(&external).expect("insert");

    // Non-`run`/`run-mcp` subcommand must be excluded even on an orbit name.
    let mut non_run = sample_params_with(
        "exec-show-noise",
        TEST_CLAUDE_MODEL,
        AuditEventStatus::Success,
    );
    non_run.subcommand = Some("show".to_string());
    non_run.tool_name = Some("orbit.adr.show".to_string());
    non_run.target_id = Some("orbit.adr.show".to_string());
    store.insert_audit_event_record(&non_run).expect("insert");

    let rows = store
        .get_audit_tool_call_counts_by_surface_and_role(None)
        .expect("surface counts");

    assert_eq!(
        rows,
        vec![
            AuditToolCallCountsBySurfaceAndRole {
                surface: "adr".to_string(),
                role: TEST_CLAUDE_MODEL.to_string(),
                total: 2,
                failed: 1,
            },
            AuditToolCallCountsBySurfaceAndRole {
                surface: "config".to_string(),
                role: TEST_CODEX_MODEL.to_string(),
                total: 1,
                failed: 0,
            },
            AuditToolCallCountsBySurfaceAndRole {
                surface: "task".to_string(),
                role: TEST_CODEX_MODEL.to_string(),
                total: 1,
                failed: 0,
            },
        ]
    );
}

#[test]
fn top_tool_calls_groups_by_tool_name_and_role_with_limit() {
    let store = Store::open_in_memory().expect("open store");

    // gpt-5.5: 3x orbit.task.show
    for i in 0..3 {
        let mut p = sample_params_with(
            &format!("exec-show-{i}"),
            TEST_CODEX_MODEL,
            AuditEventStatus::Success,
        );
        p.tool_name = Some("orbit.task.show".to_string());
        p.target_id = Some("orbit.task.show".to_string());
        store.insert_audit_event_record(&p).expect("insert");
    }

    // claude-opus-4-7: 2x orbit.search
    for i in 0..2 {
        let mut p = sample_params_with(
            &format!("exec-claude-search-{i}"),
            TEST_CLAUDE_MODEL,
            AuditEventStatus::Success,
        );
        p.tool_name = Some("orbit.search".to_string());
        p.target_id = Some("orbit.search".to_string());
        store.insert_audit_event_record(&p).expect("insert");
    }

    // gpt-5.5: 1× orbit.task.update
    {
        let mut p = sample_params_with(
            "exec-task-update",
            TEST_CODEX_MODEL,
            AuditEventStatus::Success,
        );
        p.tool_name = Some("orbit.task.update".to_string());
        p.target_id = Some("orbit.task.update".to_string());
        store.insert_audit_event_record(&p).expect("insert");
    }

    // Non-orbit tool — must be excluded.
    {
        let mut p = sample_params_with(
            "exec-non-orbit",
            TEST_CODEX_MODEL,
            AuditEventStatus::Success,
        );
        p.tool_name = Some("github.create_pr".to_string());
        p.target_id = Some("github.create_pr".to_string());
        store.insert_audit_event_record(&p).expect("insert");
    }

    // Non-`run`/`run-mcp` subcommand on an orbit name — must be excluded.
    {
        let mut p = sample_params_with(
            "exec-show-noise",
            TEST_CODEX_MODEL,
            AuditEventStatus::Success,
        );
        p.subcommand = Some("show".to_string());
        p.tool_name = Some("orbit.task.show".to_string());
        p.target_id = Some("orbit.task.show".to_string());
        store.insert_audit_event_record(&p).expect("insert");
    }

    let rows = store
        .get_audit_top_tool_calls(None, 0)
        .expect("top tool calls");
    assert_eq!(
        rows,
        vec![
            AuditTopToolCall {
                tool_name: "orbit.task.show".to_string(),
                role: TEST_CODEX_MODEL.to_string(),
                total: 3,
            },
            AuditTopToolCall {
                tool_name: "orbit.search".to_string(),
                role: TEST_CLAUDE_MODEL.to_string(),
                total: 2,
            },
            AuditTopToolCall {
                tool_name: "orbit.task.update".to_string(),
                role: TEST_CODEX_MODEL.to_string(),
                total: 1,
            },
        ]
    );

    // Limit caps the row count, preserving sort order.
    let limited = store
        .get_audit_top_tool_calls(None, 2)
        .expect("top tool calls limited");
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0].tool_name, "orbit.task.show");
    assert_eq!(limited[1].tool_name, "orbit.search");
}

#[test]
fn audit_event_aggregates_by_tool_splits_failures_by_surface() {
    let store = Store::open_in_memory().expect("open store");
    let since = chrono::Utc::now() - chrono::Duration::hours(1);

    let mut cli_ok = sample_params_with("exec-cli-ok", "codex", AuditEventStatus::Success);
    cli_ok.subcommand = Some("run".to_string());
    cli_ok.tool_name = Some("orbit.search".to_string());
    cli_ok.duration_ms = 50;
    store.insert_audit_event_record(&cli_ok).expect("insert");

    let mut cli_fail = sample_params_with("exec-cli-fail", "codex", AuditEventStatus::Failure);
    cli_fail.subcommand = Some("run".to_string());
    cli_fail.tool_name = Some("orbit.search".to_string());
    cli_fail.duration_ms = 150;
    store.insert_audit_event_record(&cli_fail).expect("insert");

    let mut mcp_fail = sample_params_with("exec-mcp-fail", "codex", AuditEventStatus::Failure);
    mcp_fail.subcommand = Some("run-mcp".to_string());
    mcp_fail.tool_name = Some("orbit.search".to_string());
    mcp_fail.duration_ms = 250;
    store.insert_audit_event_record(&mcp_fail).expect("insert");

    // Event with NULL tool_name folds into "unknown".
    let mut no_tool = sample_params_with("exec-no-tool", "codex", AuditEventStatus::Success);
    no_tool.subcommand = None;
    no_tool.tool_name = None;
    no_tool.duration_ms = 10;
    store.insert_audit_event_record(&no_tool).expect("insert");

    let rows = store
        .get_audit_event_aggregates_by_tool(&since)
        .expect("aggregates by tool");

    let search = rows
        .iter()
        .find(|r| r.tool_name == "orbit.search")
        .expect("orbit.search row");
    assert_eq!(search.total, 3);
    assert_eq!(search.successes, 1);
    assert_eq!(search.failures, 2);
    assert_eq!(search.denials, 0);
    assert_eq!(search.mcp_total, 1);
    assert_eq!(search.cli_total, 2);
    assert_eq!(search.mcp_failures, 1);
    assert_eq!(search.cli_failures, 1);
    assert_eq!(search.avg_duration_ms.round() as i64, 150);

    let unknown = rows
        .iter()
        .find(|r| r.tool_name == "unknown")
        .expect("unknown bucket");
    assert_eq!(unknown.total, 1);
    assert_eq!(unknown.successes, 1);
    assert_eq!(unknown.failures, 0);
    assert_eq!(unknown.denials, 0);
    assert_eq!(unknown.mcp_total, 0);
    assert_eq!(unknown.cli_total, 0);
}

#[test]
fn audit_event_aggregates_by_role_splits_subcommand_surface() {
    let store = Store::open_in_memory().expect("open store");
    let since = chrono::Utc::now() - chrono::Duration::hours(1);

    let mut codex_cli = sample_params_with("exec-codex-cli", "codex", AuditEventStatus::Success);
    codex_cli.subcommand = Some("run".to_string());
    store.insert_audit_event_record(&codex_cli).expect("insert");

    let mut codex_mcp = sample_params_with("exec-codex-mcp", "codex", AuditEventStatus::Success);
    codex_mcp.subcommand = Some("run-mcp".to_string());
    store.insert_audit_event_record(&codex_mcp).expect("insert");

    let mut codex_other =
        sample_params_with("exec-codex-other", "codex", AuditEventStatus::Success);
    codex_other.subcommand = Some("show".to_string());
    store
        .insert_audit_event_record(&codex_other)
        .expect("insert");

    let mut codex_internal =
        sample_params_with("exec-codex-internal", "codex", AuditEventStatus::Success);
    codex_internal.subcommand = None;
    store
        .insert_audit_event_record(&codex_internal)
        .expect("insert");

    let mut human = sample_params_with("exec-human", "human", AuditEventStatus::Success);
    human.subcommand = Some("run".to_string());
    store.insert_audit_event_record(&human).expect("insert");

    let rows = store
        .get_audit_event_aggregates_by_role(&since)
        .expect("aggregates by role");

    let codex = rows.iter().find(|r| r.role == "codex").expect("codex row");
    assert_eq!(codex.total, 4);
    assert_eq!(codex.mcp, 1);
    assert_eq!(codex.cli, 1);
    assert_eq!(codex.other, 1);
    assert_eq!(codex.no_subcommand, 1);
    assert_eq!(
        codex.mcp + codex.cli + codex.other + codex.no_subcommand,
        codex.total
    );

    let human = rows.iter().find(|r| r.role == "human").expect("human row");
    assert_eq!(human.total, 1);
    assert_eq!(human.mcp, 0);
    assert_eq!(human.cli, 1);
    assert_eq!(human.other, 0);
    assert_eq!(human.no_subcommand, 0);
    assert_eq!(
        human.mcp + human.cli + human.other + human.no_subcommand,
        human.total
    );
}
