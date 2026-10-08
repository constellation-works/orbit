use crate::assets::{DASHBOARD_CSP, DASHBOARD_FILES, serve_dashboard_file};
use axum::http::{HeaderMap, HeaderValue, header};

#[test]
fn dashboard_routes_emit_csp() {
    for &(route, _, _) in DASHBOARD_FILES {
        let response = serve_dashboard_file(route, &HeaderMap::new());
        assert_eq!(
            response.headers().get(header::CONTENT_SECURITY_POLICY),
            Some(&HeaderValue::from_static(DASHBOARD_CSP)),
            "{route} route must emit the dashboard CSP"
        );
    }
}

#[test]
fn dashboard_markdown_call_sites_use_sanitizing_wrapper() {
    let wrapper = include_str!("../../assets/dashboard/js/markdown.js");
    let app = include_str!("../../assets/dashboard/app.js");
    let tasks = include_str!("../../assets/dashboard/js/tasks.js");
    let plugins = include_str!("../../assets/dashboard/js/plugins.js");
    assert!(
        wrapper.contains("purifier.sanitize("),
        "markdown wrapper must sanitize rendered HTML before DOM insertion"
    );
    for (name, source) in [("app", app), ("tasks", tasks), ("plugins", plugins)] {
        assert!(
            !source.contains("marked.parse("),
            "{name} must not bypass the sanitizing markdown wrapper"
        );
        assert!(
            source.contains("renderMarkdown("),
            "{name} must call the sanitizing markdown wrapper"
        );
    }
    assert!(
        !plugins.contains("innerHTML = source"),
        "plugin source must not be assigned as raw innerHTML"
    );
}

#[test]
fn host_resource_chips_execute_topbar_aggregate_pressure_unknown_and_recovery_states() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "src/tests/dashboard_host_resources.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard asset behavior fixture");
    assert!(
        result.status.success(),
        "dashboard host resource behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn dashboard_drops_superseded_audit_policy_and_scoreboard_responses() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "src/tests/dashboard_panel_race.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard panel race fixture");
    assert!(
        result.status.success(),
        "dashboard panel race behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn settings_system_view_executes_render_provenance_override_edit_and_refused_write() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "src/tests/dashboard_config_system.mjs",
        ])
        .env("TZ", "America/Los_Angeles")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard asset behavior fixture");
    assert!(
        result.status.success(),
        "dashboard settings system behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn settings_hosts_view_executes_rows_freshness_load_banner_and_inline_mutations() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "src/tests/dashboard_config_hosts.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard asset behavior fixture");
    assert!(
        result.status.success(),
        "dashboard settings hosts behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn runs_view_executes_load_more_live_duration_actions_header_and_cancel_style() {
    let result = std::process::Command::new("node")
        .args(["--experimental-vm-modules", "src/tests/dashboard_runs.mjs"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard asset behavior fixture");
    assert!(
        result.status.success(),
        "dashboard runs behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn dashboard_clock_renders_local_times_with_zone_and_reliability_range_in_utc() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "src/tests/dashboard_timestamps.mjs",
        ])
        .env("TZ", "America/Los_Angeles")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard asset behavior fixture");
    assert!(
        result.status.success(),
        "dashboard timestamp behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn tasks_rail_count_reports_matching_total_across_pages_and_aggregate_view() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "src/tests/dashboard_tasks_rail.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard asset behavior fixture");
    assert!(
        result.status.success(),
        "dashboard tasks rail count behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
