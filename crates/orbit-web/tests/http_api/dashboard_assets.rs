#[test]
fn host_resource_chips_execute_topbar_aggregate_pressure_unknown_and_recovery_states() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_host_resources.mjs",
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
fn dashboard_drops_superseded_audit_codebase_and_scoreboard_responses() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_panel_race.mjs",
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
fn codebase_view_executes_subtab_swap_sections_empty_state_and_denial_badge() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_codebase.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard codebase fixture");
    assert!(
        result.status.success(),
        "dashboard codebase view behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// Analytics › Orchestrators renders its definition tiles, components before
/// the composite, greys below-support rows, toggles per 100 tasks and per 8
/// active hours, and opens a count's tasks from the cached detail.
#[test]
fn orchestrators_view_executes_rate_toggle_sorting_and_drill_down() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_orchestrators.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard orchestrators fixture");
    assert!(
        result.status.success(),
        "dashboard orchestrators view behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn settings_system_view_executes_render_provenance_override_edit_and_refused_write() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_config_system.mjs",
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
            "tests/http_api/dashboard_config_hosts.mjs",
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
fn run_detail_failure_chain_links_root_cause_and_names_a_cancellation() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_run_detail_failure.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard asset behavior fixture");
    assert!(
        result.status.success(),
        "dashboard run-detail failure chain failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn runs_view_executes_load_more_live_duration_actions_header_and_cancel_style() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_runs.mjs",
        ])
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
fn dashboard_clock_defaults_to_utc_persists_local_and_keeps_reliability_range_in_utc() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_timestamps.mjs",
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
            "tests/http_api/dashboard_tasks_rail.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard asset behavior fixture");
    assert!(
        result.status.success(),
        "dashboard tasks rail count and drain wait behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn dashboard_renders_provider_reading_age_and_auth_failures() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_provider_auth.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard provider auth fixture");
    assert!(
        result.status.success(),
        "dashboard provider reading age and auth failures failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn hidden_dashboard_polls_lightly_prefixes_failures_and_notifies_only_after_opt_in() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_background_polling.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard asset behavior fixture");
    assert!(
        result.status.success(),
        "dashboard background polling behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
