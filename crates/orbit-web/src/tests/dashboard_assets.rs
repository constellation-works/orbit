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
