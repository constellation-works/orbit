use std::path::Path;
use std::time::{Duration, Instant};

use super::support::{Fixture, isolated, json_ok};
use serde_json::{Value, json};

fn write_host_entry(fixture: &Fixture, name: &str, machine_id: &str, prefix: &str, pad: usize) {
    let text = format!(
        "schema_version = 1\n# {}\n\n[[hosts]]\nname = \"{name}\"\nmachine_id = \"{machine_id}\"\nssh = \"{name}.example\"\ntask_prefix = \"{prefix}\"\n",
        "x".repeat(pad)
    );
    std::fs::write(fixture.global.join("hosts.toml"), text).unwrap();
}

fn get_hosts(origin: &str) -> Value {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    json_ok(
        client
            .get(format!("{origin}/api/hosts?probe=false"))
            .send()
            .unwrap(),
    )
}

fn host_names(body: &Value) -> Vec<&str> {
    body["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|host| host["name"].as_str().unwrap())
        .collect()
}

fn wait_gate(gate: &Path, point: &str) {
    let reached = gate.join(format!("{point}.reached"));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !reached.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for gate {point}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn gate_file(gate: &Path, point: &str, kind: &str) {
    std::fs::write(gate.join(format!("{point}.{kind}")), b"x").unwrap();
}

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
fn hosts_list_never_pairs_repaired_snapshot_with_older_load_error() {
    isolated(
        "host::hosts_list_never_pairs_repaired_snapshot_with_older_load_error",
        || {
            let fixture = Fixture::new();
            let gate = fixture.path("host-gate");
            std::fs::create_dir_all(&gate).unwrap();
            write_host_entry(&fixture, "alpha", "hm_alpha", "AA", 0);
            let server = fixture.server_with_envs(
                false,
                &[("ORBIT_TEST_HOSTS_PUBLISH_GATE", gate.as_os_str())],
            );
            let origin = server.origin.clone();

            let v1 = get_hosts(&origin);
            let generation_v1 = v1["generation"].as_u64().unwrap();
            assert!(host_names(&v1).contains(&"alpha"), "{v1}");
            assert_eq!(v1["load_error"], json!(null), "{v1}");

            // Failure-to-repair overlap: request A records the malformed
            // file's error and pauses before reading the published view;
            // request B repairs the file and pauses between publishing the
            // repaired snapshot and clearing the older error.
            for point in [
                "pin-after-reload",
                "publish-before-clear",
                "pin-read-attempted",
            ] {
                gate_file(&gate, point, "armed");
            }
            std::fs::write(
                fixture.global.join("hosts.toml"),
                "schema_version = \"broken\"\n[[hosts]]\nname = [\n",
            )
            .unwrap();
            let origin_a = origin.clone();
            let request_a = std::thread::spawn(move || get_hosts(&origin_a));
            wait_gate(&gate, "pin-after-reload");

            write_host_entry(&fixture, "beta", "hm_beta", "BB", 24);
            let origin_b = origin.clone();
            let request_b = std::thread::spawn(move || get_hosts(&origin_b));
            wait_gate(&gate, "publish-before-clear");

            gate_file(&gate, "pin-after-reload", "release");
            wait_gate(&gate, "pin-read-attempted");
            gate_file(&gate, "publish-before-clear", "release");

            let a = request_a.join().unwrap();
            let b = request_b.join().unwrap();
            let generation_v2 = b["generation"].as_u64().unwrap();
            assert!(generation_v2 > generation_v1, "v1={v1} b={b}");
            for body in [&a, &b] {
                assert_eq!(
                    body["generation"].as_u64().unwrap(),
                    generation_v2,
                    "{body}"
                );
                assert!(host_names(body).contains(&"beta"), "{body}");
                assert!(!host_names(body).contains(&"alpha"), "{body}");
                assert_eq!(
                    body["load_error"],
                    json!(null),
                    "repaired generation must not carry the older load error: {body}"
                );
            }

            // A malformed newer file reports its error beside the last valid
            // snapshot and is retried on the next request.
            std::fs::write(
                fixture.global.join("hosts.toml"),
                "schema_version = \"broken-again\"\n[[hosts]]\nname = [ [\n# retry\n",
            )
            .unwrap();
            for _ in 0..2 {
                let failed = get_hosts(&origin);
                assert_eq!(
                    failed["generation"].as_u64().unwrap(),
                    generation_v2,
                    "{failed}"
                );
                assert!(host_names(&failed).contains(&"beta"), "{failed}");
                assert!(failed["load_error"]["message"].is_string(), "{failed}");
            }

            // Repair clears the error; an unchanged file keeps its generation.
            write_host_entry(&fixture, "gamma", "hm_gamma", "GA", 48);
            let repaired = get_hosts(&origin);
            let generation_v3 = repaired["generation"].as_u64().unwrap();
            assert!(generation_v3 > generation_v2, "{repaired}");
            assert!(host_names(&repaired).contains(&"gamma"), "{repaired}");
            assert_eq!(repaired["load_error"], json!(null), "{repaired}");
            let unchanged = get_hosts(&origin);
            assert_eq!(
                unchanged["generation"].as_u64().unwrap(),
                generation_v3,
                "{unchanged}"
            );
            assert!(host_names(&unchanged).contains(&"gamma"), "{unchanged}");
            assert_eq!(unchanged["load_error"], json!(null), "{unchanged}");
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

/// The relative module specifiers `source` imports, statically or dynamically.
fn relative_imports(source: &str) -> Vec<&str> {
    let mut specs = Vec::new();
    for opener in ["from '", "from \"", "import('", "import(\""] {
        let quote = opener.chars().last().unwrap();
        for (at, _) in source.match_indices(opener) {
            let rest = &source[at + opener.len()..];
            let Some(end) = rest.find(quote) else {
                continue;
            };
            let spec = &rest[..end];
            if spec.starts_with("./") || spec.starts_with("../") {
                specs.push(spec);
            }
        }
    }
    specs
}

/// `spec` resolved against the module served at `base`.
fn resolve_module(base: &str, spec: &str) -> String {
    let mut parts: Vec<&str> = base.split('/').collect();
    parts.pop();
    for segment in spec.split('/') {
        match segment {
            "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    parts.join("/")
}

/// The browser harnesses serve dashboard assets from disk, so a module left
/// out of the embedded file table passes them and breaks the page `orbit web
/// serve` sends. Every module the page's import graph reaches is served.
#[test]
fn every_module_the_dashboard_imports_is_served() {
    isolated("host::every_module_the_dashboard_imports_is_served", || {
        let fixture = Fixture::new();
        let server = fixture.server(false);
        let mut pending = vec!["/static/app.js".to_string()];
        let mut served = std::collections::BTreeSet::new();
        while let Some(path) = pending.pop() {
            if !served.insert(path.clone()) {
                continue;
            }
            let response = server.get(&path);
            assert!(
                response.status().is_success(),
                "{path} is imported by the dashboard but not served: {}",
                response.status()
            );
            let source = response.text().unwrap();
            pending.extend(
                relative_imports(&source)
                    .into_iter()
                    .map(|spec| resolve_module(&path, spec)),
            );
        }
        assert!(
            served.contains("/static/js/drain-start.js"),
            "the walk reaches the Drain start composer: {served:?}"
        );
    });
}
