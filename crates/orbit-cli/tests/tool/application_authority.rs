//! Public CLI regression for the missing app-only authorization path.
use super::Fixture;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn app_command(
    fixture: &Fixture,
    credential: &Path,
    tool: &str,
    input: &Value,
) -> assert_cmd::Command {
    let mut command = fixture.pinned(assert_cmd::Command::new("/bin/sh"));
    command
        .args([
            "-c",
            "exec 9<\"$1\"; shift; export ORBIT_APPLICATION_CREDENTIAL_FD=9; exec \"$@\"",
            "app",
        ])
        .arg(credential)
        .arg(env!("CARGO_BIN_EXE_orbit"))
        .args([
            "tool",
            "run",
            tool,
            "--input",
            &input.to_string(),
            "--format",
            "json",
        ]);
    command
}

fn consent(
    fixture: &Fixture,
    request: &Value,
    suffix: &str,
    replaces: Option<&str>,
) -> (Value, PathBuf) {
    let input = fixture.home.join(format!("consent-{suffix}.json"));
    let credential = fixture.home.join(format!("credential-{suffix}"));
    std::fs::write(&input, request.to_string()).unwrap();
    let mut command = fixture.orbit_as_operator();
    command.args([
        "application",
        if replaces.is_some() {
            "rotate"
        } else {
            "consent"
        },
    ]);
    if let Some(id) = replaces {
        command.arg(id);
    }
    let output = command
        .arg("--input-file")
        .arg(input)
        .arg("--credential-file")
        .arg(&credential)
        .args(["--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    (serde_json::from_slice(&output).unwrap(), credential)
}

#[test]
fn application_consent_preserves_agent_boundaries_and_audits_refusals() {
    let backend_available = orbit_exec::macos_sandbox_test_guard(
        "application_consent_preserves_agent_boundaries_and_audits_refusals",
    );
    let fixture = Fixture::new();
    fixture.install_shapes();
    fixture.success(["plugin", "enable", "shapes"]);
    let tool = "shapes.maintain";
    let input = json!({"force":true});
    // Reproduces the original gap: a service cannot invoke this mutation;
    // neither an agent identity nor an app label provides authority.
    fixture
        .command(["tool", "run", tool, "--input", &input.to_string()])
        .env("ORBIT_APPLICATION_ID", "approved-app")
        .assert()
        .failure();
    let show = fixture.json(["plugin", "show", "shapes"]);
    let digest = show["manifest_digest"].as_str().unwrap();
    let request = json!({"application_id":"approved-app", "expires_at":"2099-01-01T00:00:00Z", "scopes":[{"operation":{"tool":tool,"input":input,"plugin":{"name":"shapes","manifest_digest":digest}},"callbacks":[]}]});
    let (grant, credential) = consent(&fixture, &request, "first", None);
    let grant_id = grant["consent_id"].as_str().unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&credential).unwrap().permissions().mode() & 0o777,
        0o600
    );
    if backend_available {
        app_command(&fixture, &credential, tool, &input)
            .assert()
            .success();
    }
    app_command(&fixture, &credential, tool, &input)
        .env("ORBIT_AGENT_NAME", "codex")
        .assert()
        .failure();
    app_command(&fixture, &credential, tool, &json!({"force":false}))
        .assert()
        .failure();
    app_command(&fixture, &credential, "orbit.routine.control", &json!({}))
        .assert()
        .failure();
    app_command(
        &fixture,
        &credential,
        "orbit.auto_task.mint",
        &json!({"acknowledge_unconditional":true}),
    )
    .assert()
    .failure();
    let other = fixture.home.join("other-workspace");
    crate::git_repo::init(&other);
    fixture
        .command(["workspace", "init", "--name", "other-app-workspace"])
        .current_dir(&other)
        .assert()
        .success();
    app_command(&fixture, &credential, tool, &input)
        .current_dir(&other)
        .assert()
        .failure();
    let (rotated, next) = consent(&fixture, &request, "rotated", Some(grant_id));
    app_command(&fixture, &credential, tool, &input)
        .assert()
        .failure();
    if backend_available {
        app_command(&fixture, &next, tool, &input)
            .assert()
            .success();
    }
    fixture
        .operator_command([
            "application",
            "revoke",
            rotated["consent_id"].as_str().unwrap(),
        ])
        .assert()
        .success();
    app_command(&fixture, &next, tool, &input)
        .assert()
        .failure();
    // Stored audit hydration must preserve host identity and consent without
    // conflating them with client attribution or storing the bearer secret.
    let audit = fixture.json(["audit", "list", "--tool", tool, "--limit", "100"]);
    let audit_text = audit.to_string();
    assert!(
        audit_text.contains(grant_id),
        "application consent provenance must survive audit readback"
    );
    assert!(audit_text.contains("approved-app"));
    let secret = std::fs::read_to_string(&credential).unwrap();
    assert!(
        !audit_text.contains(&secret),
        "STD-05: application credential must never reach an audit row"
    );
    let conn = rusqlite::Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
    let statuses: Vec<(String, String)> = conn.prepare("SELECT status, application_json FROM audit_events WHERE tool_name = ?1 AND application_json IS NOT NULL").unwrap().query_map([tool], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
    if backend_available {
        assert!(statuses.iter().any(|(status, _)| status == "success"));
    }
    assert!(statuses.iter().any(|(status, _)| status == "denied"));
    for (_, raw) in statuses {
        assert!(!raw.contains(&secret));
    }
}

#[test]
fn application_consent_refuses_stale_manifest_and_expiry() {
    let fixture = Fixture::new();
    fixture.install_shapes();
    fixture.success(["plugin", "enable", "shapes"]);
    let show = fixture.json(["plugin", "show", "shapes"]);
    let digest = show["manifest_digest"].as_str().unwrap();
    let request = json!({"application_id":"expiry-app", "expires_at":"2099-01-01T00:00:00Z", "scopes":[{"operation":{"tool":"shapes.maintain","input":{},"plugin":{"name":"shapes","manifest_digest":digest}},"callbacks":[]}]});
    let (grant, credential) = consent(&fixture, &request, "stale", None);
    // Advance persisted authority and its matching host witness together to
    // an expired time; no sleeps or timing race are needed.
    let mut expired = grant.clone();
    expired["expires_at"] = json!("2000-01-01T00:00:00Z");
    let conn = rusqlite::Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
    conn.execute(
        "UPDATE application_consents SET consent_json = ?1 WHERE consent_id = ?2",
        rusqlite::params![expired.to_string(), grant["consent_id"].as_str().unwrap()],
    )
    .unwrap();
    let witness = fixture
        .home
        .join(".orbit/state/application-consents")
        .join(format!("{}.json", grant["consent_id"].as_str().unwrap()));
    let mut document: Value = serde_json::from_slice(&std::fs::read(&witness).unwrap()).unwrap();
    document["consent"] = expired;
    std::fs::write(&witness, document.to_string()).unwrap();
    app_command(&fixture, &credential, "shapes.maintain", &json!({}))
        .assert()
        .failure()
        .stderr(predicates::str::contains("expired"));
    let (_, current) = consent(&fixture, &request, "digest", None);
    let manifest = Path::new(show["install_path"].as_str().unwrap()).join("plugin.yaml");
    let content = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        manifest,
        format!("{content}\n# a new manifest requires consent\n"),
    )
    .unwrap();
    app_command(&fixture, &current, "shapes.maintain", &json!({}))
        .assert()
        .failure()
        .stderr(predicates::str::contains("stale"));
}

#[test]
fn application_consent_bounds_host_completion_callbacks() {
    if !orbit_exec::macos_sandbox_test_guard("application_consent_bounds_host_completion_callbacks")
    {
        return;
    }
    let fixture = Fixture::new();
    let task = fixture.json([
        "task",
        "add",
        "--title",
        "App completion target",
        "--complexity",
        "low",
        "--description",
        "Check app callback authority.",
    ]);
    let id = task["id"].as_str().unwrap();
    let approved = json!({"id":id,"comment":"Applied with explicit user consent"});
    let refused = json!({"id":id,"comment":"This payload was never approved"});
    let source = fixture.source("appcallback");
    let root = super::write_shell_backend(
        &source,
        format!(
            r#"#!/bin/sh
cat >/dev/null
'{}' tool run orbit.task.update --input '{}' >/dev/null 2>&1 || exit 71
if '{}' tool run orbit.task.update --input '{}' >/dev/null 2>&1; then exit 72; fi
printf '{{"ok":true,"output":{{"callback":"applied-and-bounded"}}}}\n'
"#,
            env!("CARGO_BIN_EXE_orbit"),
            approved,
            env!("CARGO_BIN_EXE_orbit"),
            refused
        ),
    );
    std::fs::write(
        root.join("plugin.yaml"),
        r#"schemaVersion: 2
kind: Plugin
metadata:
  name: appcallback
  version: 0.1.0
  description: Application completion fixture.
spec:
  permissions:
    orbit_tools: [orbit.task.update]
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: apply
      description: Apply one approved operation.
      execution_kind: mutating
      mcp_scope: workspace
"#,
    )
    .unwrap();
    fixture.add(&source, &["--enable", "--grant", "orbit_tools"]);
    let show = fixture.json(["plugin", "show", "appcallback"]);
    let request = json!({"application_id":"callback-app", "expires_at":"2099-01-01T00:00:00Z", "scopes":[{"operation":{"tool":"appcallback.apply","input":{},"plugin":{"name":"appcallback","manifest_digest":show["manifest_digest"]}},"callbacks":[{"tool":"orbit.task.update","input":approved,"plugin":null}]}]});
    let (_, credential) = consent(&fixture, &request, "callback", None);
    app_command(&fixture, &credential, "appcallback.apply", &json!({}))
        .assert()
        .success()
        .stdout(predicates::str::contains("applied-and-bounded"));
    let shown = fixture.json(["task", "show", id]);
    assert!(
        shown
            .to_string()
            .contains("Applied with explicit user consent")
    );
    assert!(
        !shown
            .to_string()
            .contains("This payload was never approved")
    );
    let conn = rusqlite::Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
    let rows: Vec<(String, Value)> = conn.prepare("SELECT status, application_json FROM audit_events WHERE tool_name = 'orbit.task.update' AND application_json IS NOT NULL").unwrap().query_map([], |r| { let raw: String = r.get(1)?; Ok((r.get(0)?, serde_json::from_str(&raw).unwrap())) }).unwrap().map(Result::unwrap).collect();
    assert!(
        rows.iter()
            .any(|(status, app)| status == "success" && app["callback"] == true)
    );
    assert!(
        rows.iter()
            .any(|(status, app)| status == "denied" && app["callback"] == true)
    );
}

/// Exercise the inherited host route without spawning a sandboxed backend.
/// The separate backend case above proves the real confinement integration.
#[test]
fn application_host_route_refuses_changed_inputs_and_revoked_consent() {
    if std::env::var_os("ORBIT_APPLICATION_ROUTE_FIXTURE_CHILD").is_none() {
        let output = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let name = "plugin_cli_group::application_authority::application_host_route_refuses_changed_inputs_and_revoked_consent";
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        orbit_common::test_env::clear_inherited_authority(|key| {
            child.env_remove(key);
        });
        child
            .args(["--exact", name, "--nocapture"])
            .env("ORBIT_APPLICATION_ROUTE_FIXTURE_CHILD", "1")
            .env("HOME", output.path());
        let result = orbit_common::test_env::run_child_test(&mut child, name, output.path());
        orbit_common::test_env::assert_child_test_passed(
            name,
            result.status,
            &result.stdout,
            &result.stderr,
        );
        return;
    }
    use orbit_core::{OrbitError, OrbitRuntime};
    use orbit_tools::plugin::PluginCallbackSession;
    use orbit_tools::{OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, ReservationOwnerContext};
    use orbit_types::tool::ApplicationInvocation;
    use std::os::unix::process::CommandExt;
    use std::sync::Arc;

    struct Host(OrbitRuntime);
    impl OrbitToolHost for Host {
        fn execute(
            &self,
            _: OrbitBuiltinAction,
            _: Value,
            _: Option<String>,
            _: Option<String>,
            _: Option<ReservationOwnerContext>,
        ) -> Result<Value, OrbitError> {
            Err(OrbitError::Execution(
                "fixture uses only host-routed callbacks".into(),
            ))
        }
        fn task_scope(&self) -> OrbitTaskScope {
            Default::default()
        }
        fn execute_host_routed_callback(
            &self,
            tool: &str,
            input: Value,
        ) -> Result<Value, OrbitError> {
            self.0
                .execute_tool_command_dispatch_with_session_context(
                    tool,
                    input,
                    None,
                    None,
                    orbit_core::adapter::command::ToolEntryPoint::Cli,
                    Default::default(),
                )
                .map(|output| output.value)
        }
    }
    let fixture = Fixture::new();
    let task = fixture.json([
        "task",
        "add",
        "--title",
        "Routed application target",
        "--complexity",
        "low",
        "--description",
        "Bound completion",
    ]);
    let id = task["id"].as_str().unwrap();
    fixture.install_status(
        "routefixture",
        "  permissions:\n    orbit_tools: [orbit.task.update]\n",
        &["--enable", "--grant", "orbit_tools"],
    );
    let show = fixture.json(["plugin", "show", "routefixture"]);
    let approved = json!({"id":id,"comment":"Routed application approval"});
    let request = json!({"application_id":"route-app","expires_at":"2099-01-01T00:00:00Z","scopes":[{"operation":{"tool":"routefixture.status","input":{},"plugin":{"name":"routefixture","manifest_digest":show["manifest_digest"]}},"callbacks":[{"tool":"orbit.task.update","input":approved,"plugin":null}]}]});
    let (grant, _) = consent(&fixture, &request, "host-route", None);
    let global = fixture.home.join(".orbit");
    let runtime = OrbitRuntime::from_roots(&global, &fixture.work.join(".orbit")).unwrap();
    let provenance = orbit_types::plugin::PluginProvenance {
        name: "routefixture".into(),
        version: "0.1.0".into(),
        manifest_digest: show["manifest_digest"].as_str().unwrap().into(),
        grants: vec![],
    };
    let mut callback =
        PluginCallbackSession::mint(&global, &provenance, &["orbit.task.update".into()]).unwrap();
    callback
        .bind_application(Some(&ApplicationInvocation {
            consent: serde_json::from_value(grant.clone()).unwrap(),
            scope_index: 0,
            callback: false,
        }))
        .unwrap();
    let host: Arc<dyn OrbitToolHost> = Arc::new(Host(runtime));
    callback.open_host_route(Some(&host)).unwrap();
    let run = |input: &Value| {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_orbit"));
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let credential_fd = callback.credential_fd();
        let route_fd = callback.host_route_fd().unwrap();
        command
            .current_dir(&fixture.work)
            .env("HOME", &fixture.home)
            .env("USERPROFILE", &fixture.home)
            .env("ORBIT_PLUGIN_CALLBACK", "descriptor-required")
            .args([
                "tool",
                "run",
                "orbit.task.update",
                "--input",
                &input.to_string(),
                "--format",
                "json",
            ]);
        // SAFETY: dup2 only maps the two live host-owned fds in the child;
        // the guard remains alive until output() reaps it.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(credential_fd, 3) < 0 || libc::dup2(route_fd, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.output().unwrap()
    };
    let output = run(&approved);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let changed = run(&json!({"id":id,"comment":"not approved"}));
    assert!(!changed.status.success());
    fixture
        .operator_command([
            "application",
            "revoke",
            grant["consent_id"].as_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(!run(&approved).status.success());
    let audit = fixture.json([
        "audit",
        "list",
        "--tool",
        "orbit.task.update",
        "--limit",
        "100",
    ]);
    assert!(audit.to_string().contains("route-app"));
}
