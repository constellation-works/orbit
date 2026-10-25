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

/// Analytics › Agents counts each role tab's ranked rows, splits the strip by
/// owner, moves the detail column with the selected row, keeps "How the score
/// works" open across polls, re-reads the detail after a newer scan, shows the
/// empty and not-computed states, the Reviewers provisional banner, `accepts
/// often` tag and verdict-outcome bars, and routes `#analytics/orchestrators`
/// to `#analytics/agents/orchestrators`.
#[test]
fn agents_view_executes_selection_disclosure_states_and_redirect() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_agents_implementers.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard agents fixture");
    assert!(
        result.status.success(),
        "dashboard agents view behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// Analytics › Agents › Pilots ranks the crews, leaves the tier matrix out of
/// the detail column and renders one assessed-by-observed matrix per row with
/// its diagonal and its too-easy and too-hard counts.
#[test]
fn agents_pilots_view_executes_ranking_detail_and_tier_matrix() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_agents_pilots.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard pilots fixture");
    assert!(
        result.status.success(),
        "dashboard pilots view behavior failed:\n{}\n{}",
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

#[test]
#[ignore = "requires ORBIT_PLAYWRIGHT_MODULE and ORBIT_CONTEXT_BASENAME_BROWSER_EVIDENCE_DIR"]
fn context_selector_basenames_keep_contention_colors_and_return_to_normal_on_refresh() {
    let output = std::process::Command::new("node")
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/http_api/dashboard_context_basename_color_browser.mjs"),
        )
        .arg(std::env::var_os("ORBIT_PLAYWRIGHT_MODULE").expect("prepared Playwright module"))
        .arg(
            std::env::var_os("ORBIT_CONTEXT_BASENAME_BROWSER_EVIDENCE_DIR")
                .expect("browser evidence directory"),
        )
        .output()
        .expect("node is required to execute the context basename browser fixture");
    assert!(
        output.status.success(),
        "context selector basename contention colours failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
