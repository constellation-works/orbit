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

/// Dashboard assets are served from the embedded file table, so a module left
/// out of it breaks the page `orbit web serve` sends even when the checkout
/// still has the file. Every module the page's import graph reaches is served.
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

#[test]
fn hosts_reuse_workspace_projection_and_observe_registry_replacements() {
    isolated(
        "host::hosts_reuse_workspace_projection_and_observe_registry_replacements",
        || {
            use orbit_registry::workspace_registry::{
                load_registry_from_read_only, save_registry_to,
            };
            let fixture = Fixture::new();
            let path = fixture.global.join("workspaces.json");
            let mut registry = load_registry_from_read_only(&path).unwrap().registry;
            for index in 1..13 {
                let id = format!("ws_projection_{index:02}");
                let repo = fixture.path(&format!("repo-{index}"));
                let orbit = repo.join(".orbit");
                std::fs::create_dir_all(&orbit).unwrap();
                std::fs::write(
                    orbit.join("config.yaml"),
                    format!("schema_version: 1\nworkspace_id: {id}\n"),
                )
                .unwrap();
                let mut workspace = registry.workspaces[0].clone();
                workspace.id = id.clone();
                workspace.name = format!("projection-{index:02}");
                registry.workspaces.push(workspace);
                registry
                    .checkouts
                    .push(orbit_types::workspace::WorkspaceCheckout::owner(
                        id, repo, orbit,
                    ));
            }
            save_registry_to(&registry, &path).unwrap();
            write_host_entry(&fixture, "remote", "hm_0000000000000099", "RR", 0);
            let trace = fixture.path("projection.jsonl");
            let env = vec![("ORBIT_HTTP_HOST_PROJECTION_TRACE", trace.as_os_str())];
            #[cfg(unix)]
            let (ssh_log, search_path) = {
                use std::os::unix::fs::PermissionsExt;
                let bin = fixture.path("probe-bin");
                std::fs::create_dir_all(&bin).unwrap();
                let script = bin.join("ssh");
                std::fs::write(
                    &script,
                    r#"#!/bin/sh
printf 'probe\n' >> "$HOST_PROJECTION_SSH_LOG"
exit 99
"#,
                )
                .unwrap();
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
                let log = fixture.path("ssh-calls.log");
                let mut paths = vec![bin];
                paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
                (log, std::env::join_paths(paths).unwrap())
            };
            #[cfg(unix)]
            let env = {
                let mut env = env;
                env.extend([
                    ("PATH", search_path.as_os_str()),
                    ("HOST_PROJECTION_SSH_LOG", ssh_log.as_os_str()),
                ]);
                env
            };
            let server = fixture.server_with_envs(false, &env);
            let first = get_hosts(&server.origin);
            assert_eq!(
                first["hosts"][0]["workspaces"].as_array().unwrap().len(),
                13
            );
            assert_eq!(first["hosts"][1]["reachable"], json!(null));
            assert_eq!(first["hosts"][1]["error"], json!(null));
            for _ in 0..20 {
                assert_eq!(get_hosts(&server.origin), first);
            }
            let parses = || std::fs::read_to_string(&trace).unwrap().lines().count();
            assert_eq!(
                parses(),
                1,
                "unchanged host-list reads must reuse registry parse/validation"
            );

            // save_registry_to replaces the file atomically. Equal-length names
            // exercise content invalidation without relying on clock granularity.
            registry.workspaces[1].name = "replacement01".into();
            save_registry_to(&registry, &path).unwrap();
            let replaced = get_hosts(&server.origin);
            assert_eq!(
                replaced["hosts"][0]["workspaces"][1]["name"],
                json!("replacement01")
            );
            assert_eq!(parses(), 2);
            assert_eq!(get_hosts(&server.origin)["hosts"], replaced["hosts"]);
            assert_eq!(parses(), 2);

            let replacement = fixture.path("replacement.json");
            std::fs::write(&replacement, "{broken").unwrap();
            std::fs::rename(&replacement, &path).unwrap();
            let malformed = get_hosts(&server.origin);
            assert_eq!(malformed["hosts"][0]["workspaces"], json!([]));
            assert_eq!(malformed["hosts"][1], first["hosts"][1]);
            assert_eq!(get_hosts(&server.origin), malformed);
            assert_eq!(
                parses(),
                3,
                "malformed unchanged bytes also reuse their empty projection"
            );
            save_registry_to(&registry, &path).unwrap();
            assert_eq!(get_hosts(&server.origin)["hosts"], replaced["hosts"]);

            // Missing files and invalid machine context must not serve stale
            // rows. Both recover immediately after repair.
            std::fs::rename(&path, &replacement).unwrap();
            assert_eq!(
                get_hosts(&server.origin)["hosts"][0]["workspaces"],
                json!([])
            );
            std::fs::rename(&replacement, &path).unwrap();
            assert_eq!(get_hosts(&server.origin)["hosts"], replaced["hosts"]);
            let config = fixture.global.join("config.toml");
            let valid = std::fs::read_to_string(&config).unwrap();
            let original_machine = fixture.machine_id();
            std::fs::write(&config, "[machine]\nid = 123\n").unwrap();
            assert_eq!(
                get_hosts(&server.origin)["hosts"][0]["workspaces"],
                json!([])
            );
            std::fs::write(
                &config,
                valid.replace(&original_machine, "hm_0000000000000088"),
            )
            .unwrap();
            assert_eq!(
                get_hosts(&server.origin)["hosts"][0]["workspaces"],
                json!([])
            );
            std::fs::write(&config, valid).unwrap();
            assert_eq!(get_hosts(&server.origin)["hosts"], replaced["hosts"]);
            #[cfg(unix)]
            {
                std::fs::rename(&path, &replacement).unwrap();
                std::os::unix::fs::symlink(&replacement, &path).unwrap();
                assert_eq!(
                    get_hosts(&server.origin)["hosts"][0]["workspaces"],
                    json!([])
                );
                std::fs::remove_file(&path).unwrap();
                std::fs::rename(&replacement, &path).unwrap();
                assert_eq!(get_hosts(&server.origin)["hosts"], replaced["hosts"]);
            }
            #[cfg(unix)]
            assert!(
                !ssh_log.exists(),
                "cached host reads must launch zero SSH probes"
            );
        },
    );
}

fn get_hosts_timed(origin: &str, path: &str) -> (Value, Duration) {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let started = Instant::now();
    let body = json_ok(client.get(format!("{origin}{path}")).send().unwrap());
    (body, started.elapsed())
}

fn remote_row(body: &Value) -> &Value {
    body["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|host| host["name"] == "remote")
        .expect("remote row")
}

/// Ordinary `GET /api/hosts` reads must not repeat a timed-out SSH probe:
/// they return the cached inventory fast and truthfully unprobed, while an
/// explicit `probe=true` still spends its budget and re-probes every time
/// (so recovery is seen as soon as the remote answers again).
#[cfg(unix)]
#[test]
fn ordinary_hosts_reads_do_not_probe_but_explicit_probe_reprobes() {
    isolated(
        "host::ordinary_hosts_reads_do_not_probe_but_explicit_probe_reprobes",
        || {
            use std::os::unix::fs::PermissionsExt;
            let fixture = Fixture::new();
            write_host_entry(&fixture, "remote", "hm_0000000000000099", "RR", 0);
            let bin = fixture.path("timeout-bin");
            std::fs::create_dir_all(&bin).unwrap();
            let script = bin.join("ssh");
            std::fs::write(
                &script,
                "#!/bin/sh\nprintf 'probe\\n' >> \"$HOST_TIMEOUT_SSH_LOG\"\nsleep 1\nexit 99\n",
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let log = fixture.path("timeout-ssh.log");
            let server = fixture.server_with_path_envs(
                false,
                &bin,
                &[("HOST_TIMEOUT_SSH_LOG", log.as_os_str())],
            );
            let ssh_calls = || {
                std::fs::read_to_string(&log)
                    .map(|calls| calls.lines().count())
                    .unwrap_or(0)
            };

            // Repeated ordinary reads: cached, fast, no SSH, not probed.
            let mut ordinary = Vec::new();
            for _ in 0..10 {
                let (body, elapsed) = get_hosts_timed(&server.origin, "/api/hosts");
                ordinary.push(elapsed);
                assert_eq!(remote_row(&body)["reachable"], json!(null), "{body}");
                assert_eq!(remote_row(&body)["error"], json!(null), "{body}");
            }
            ordinary.sort();
            let p95 = ordinary[(ordinary.len() * 95).div_ceil(100) - 1];
            assert!(
                p95 < Duration::from_millis(500),
                "ordinary Hosts reads must return within 500 ms p95, got {p95:?} from {ordinary:?}"
            );
            assert_eq!(ssh_calls(), 0, "ordinary reads must open no SSH session");

            // A deliberate fresh probe spends its budget and reports truthfully.
            let (probed, elapsed) = get_hosts_timed(&server.origin, "/api/hosts?probe=true");
            assert!(
                elapsed >= Duration::from_millis(500),
                "explicit probe should spend its probe budget, took {elapsed:?}"
            );
            assert!(ssh_calls() >= 1, "explicit probe must open an SSH session");
            assert_eq!(remote_row(&probed)["reachable"], json!(false), "{probed}");
            assert_eq!(
                remote_row(&probed)["error"]["code"],
                json!("unreachable_destination"),
                "{probed}"
            );
            let after_first_probe = ssh_calls();

            // The recent failure does not make later ordinary reads slow or
            // falsely probed, and is not latched against the next fresh probe.
            for _ in 0..5 {
                let (body, elapsed) = get_hosts_timed(&server.origin, "/api/hosts");
                assert!(
                    elapsed < Duration::from_millis(500),
                    "ordinary read after a timeout took {elapsed:?}"
                );
                assert_eq!(remote_row(&body)["reachable"], json!(null), "{body}");
            }
            assert_eq!(ssh_calls(), after_first_probe);
            let (reprobed, _) = get_hosts_timed(&server.origin, "/api/hosts?probe=true");
            assert!(
                ssh_calls() > after_first_probe,
                "a fresh probe must re-probe so recovery is visible"
            );
            assert_eq!(
                remote_row(&reprobed)["reachable"],
                json!(false),
                "{reprobed}"
            );
        },
    );
}
