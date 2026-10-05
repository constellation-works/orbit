use super::support::{Fixture, isolated, json_ok};
use serde_json::json;

#[test]
fn host_resources_are_host_scoped_and_expose_pressure_recovery_unknown_and_age() {
    isolated(
        "host::host_resources_are_host_scoped_and_expose_pressure_recovery_unknown_and_age",
        || {
            let fixture = Fixture::new();
            let server = fixture.resource_server();
            let first = json_ok(server.get("/api/host/resources?workspace=missing"));
            assert_eq!(first["cpu"]["severity"], json!("critical"));
            assert_eq!(first["throttle"], json!(false));
            assert!(first["sample_age_seconds"].as_f64().unwrap() >= 10.0);
            assert_eq!(first["disk"]["percent"], json!(92.0));
            assert_eq!(first["disk"]["severity"], json!("critical"));
            assert!(first["disk"]["path"].as_str().is_some());
            assert!(
                first.get("disks").is_none(),
                "the HTTP surface aggregates watched paths"
            );
            json_ok(server.get("/api/host/resources?workspace=all"));
            let held = json_ok(server.get("/api/host/resources"));
            assert_eq!(held["throttle"], json!(true));
            assert_eq!(held["disk"]["percent"], json!(92.0));
            assert_eq!(
                held["pressures"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|pressure| pressure["resource"].as_str().unwrap().starts_with("disk "))
                    .count(),
                1
            );
            assert!(!held["reason"].as_str().unwrap().is_empty());
            let unknown = json_ok(server.get("/api/host/resources"));
            assert_eq!(unknown["cpu"]["percent"], json!(null));
            assert_eq!(unknown["cpu"]["severity"], json!("unknown"));
            assert_eq!(unknown["throttle"], json!(false));
            assert_eq!(unknown["disk"], json!(null));
            let stale = json_ok(server.get("/api/host/resources"));
            assert_eq!(stale["stale"], json!(true));
            assert_eq!(stale["disk"], json!(null));
            assert_eq!(stale["memory"]["severity"], json!("unknown"));
            let asset = server.get("/static/js/host-resources.js");
            assert!(asset.status().is_success());
            assert!(
                asset.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("application/javascript")
            );
        },
    );
}

#[test]
fn global_host_resources_work_without_a_selected_workspace_and_use_global_settings() {
    isolated(
        "host::global_host_resources_work_without_a_selected_workspace_and_use_global_settings",
        || {
            let fixture = Fixture::new();
            std::fs::write(fixture.global.join("config.toml"), "[workflow.resource_throttle]\nenabled = false\ndisk_high_percent = 95\ndisk_resume_percent = 90").unwrap();
            let server = fixture.server(false);
            let sample = json_ok(server.get("/api/host/resources?workspace=missing"));
            assert_eq!(sample["thresholds"]["enabled"], json!(false));
            assert_eq!(sample["thresholds"]["disk_high_percent"], json!(95));
            assert_eq!(sample["throttle"], json!(false));
            assert!(sample.get("disks").is_none());
            assert!(sample["disk"]["percent"].as_f64().is_some());
            let path = sample["disk"]["path"].as_str().unwrap();
            assert!(
                [
                    fixture.global.clone(),
                    fixture.work.parent().unwrap().to_path_buf(),
                    fixture.work.join("state/worktrees"),
                ]
                .iter()
                .any(|watched| watched == std::path::Path::new(path)),
                "the aggregate must name a watched path; ties on one filesystem may select any of them"
            );
        },
    );
}
