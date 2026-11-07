/// Every top-level dashboard view has a view-manifest entry the UI rules
/// render, or a recorded exemption with its reason.
#[test]
fn every_top_level_dashboard_view_has_a_visual_manifest_entry_or_exemption() {
    let result = std::process::Command::new("node")
        .args(["tests/http_api/dashboard_visual_manifest_coverage.mjs"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to execute the dashboard manifest coverage guard");
    assert!(
        result.status.success(),
        "dashboard visual manifest coverage failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
