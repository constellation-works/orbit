//! Bound and read application callbacks: a plugin entry admitted under a
//! constrained scope runs its Orbit callbacks under application authority,
//! held to the entry's bound task and payload digest.
use super::Fixture;
use super::application_authority::{app_command, consent};
use chrono::{Duration, SecondsFormat, Utc};
use serde_json::{Value, json};

const PAYLOAD: &str = "{\"kind\":\"knowledge\",\"marker\":\"bound-payload-5c1e\"}";

fn expires_in(days: i64) -> String {
    (Utc::now() + Duration::days(days)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn sha256_hex(bytes: &[u8]) -> String {
    orbit_common::security::release::sha256_hex(bytes)
}

/// A `proposed` task tagged `proposal` carrying `proposal.json`.
fn proposal_task(fixture: &Fixture, title: &str, payload: &str) -> String {
    let task = fixture.json([
        "task",
        "add",
        "--title",
        title,
        "--complexity",
        "low",
        "--description",
        "Application callback target.",
        "--tag",
        "proposal",
    ]);
    let id = task["id"].as_str().unwrap().to_owned();
    if task["status"] != "proposed" {
        fixture
            .operator_command(["task", "update", &id, "--status", "proposed", "--force"])
            .assert()
            .success();
    }
    put_payload(fixture, &id, payload);
    id
}

/// Store `payload` as the task's `proposal.json`, as the operator.
fn put_payload(fixture: &Fixture, id: &str, payload: &str) {
    let source = fixture.work.join(format!("proposal-{id}.json"));
    std::fs::write(&source, payload).unwrap();
    let input = json!({"id": id, "path": "proposal.json", "source_path": source});
    fixture
        .operator_command([
            "tool",
            "run",
            "orbit.task.artifact.put",
            "--input",
            &input.to_string(),
        ])
        .assert()
        .success();
    std::fs::remove_file(source).unwrap();
}

/// The office accept profile, abridged: the entry binds the task and its
/// payload digest; every callback is bound to the entry's task.
fn accept_scope(plugin: &Value) -> Value {
    json!({
        "name": "proposal-accept",
        "operation": {
            "tool": "proposal.accept",
            "plugin": plugin,
            "fields": {
                "id": {"task": {"tags_all": ["proposal"], "status": ["proposed"],
                                "artifact_sha256": {"path": "proposal.json", "field": "sha256"}}},
                "sha256": {"sha256": {}}
            }
        },
        "callbacks": [
            {"tool": "orbit.task.show", "plugin": null,
             "fields": {"id": {"bound": "id"}, "fields": {"subset_of": ["id", "status", "tags"]}}},
            {"tool": "orbit.task.artifact.get", "plugin": null,
             "fields": {"id": {"bound": "id"}, "path": {"const": "proposal.json"}}},
            {"tool": "orbit.task.update", "plugin": null,
             "fields": {"id": {"bound": "id"},
                        "status": {"transition": {"from": ["proposed"], "to": "in-progress"}},
                        "plan": {"text": {"max_chars": 512}},
                        "comment": {"text": {"max_chars": 512}}}},
            {"tool": "orbit.task.update", "plugin": null,
             "fields": {"id": {"bound": "id"},
                        "status": {"transition": {"from": ["in-progress"], "to": "review"}},
                        "execution_summary": {"text": {"max_chars": 4096}}}},
            {"tool": "orbit.task.update", "plugin": null,
             "fields": {"id": {"bound": "id"},
                        "status": {"transition": {"from": ["review"], "to": "done"}},
                        "execution_summary": {"text": {"max_chars": 4096}}}}
        ]
    })
}

/// Install the `proposal` fixture plugin with `script` as its backend.
fn install_proposal_plugin(fixture: &Fixture, script: &str) -> Value {
    let source = fixture.source("proposal");
    let root = super::write_shell_backend(&source, script);
    std::fs::write(
        root.join("plugin.yaml"),
        r#"schemaVersion: 2
kind: Plugin
metadata:
  name: proposal
  version: 0.1.0
  description: Accepts one reviewed proposal.
spec:
  permissions:
    orbit_tools: [orbit.task.show, orbit.task.artifact.get, orbit.task.update, orbit.task.list]
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: accept
      description: Accept a reviewed proposal.
      execution_kind: mutating
      mcp_scope: workspace
      input_schema:
        type: object
        properties:
          id: { type: string, description: Proposal task id. }
          sha256: { type: string, description: Digest of the proposal bytes. }
"#,
    )
    .expect("write manifest");
    fixture.add(&source, &["--enable", "--grant", "orbit_tools"]);
    let show = fixture.json(["plugin", "show", "proposal"]);
    json!({"name": "proposal", "manifest_digest": show["manifest_digest"]})
}

fn application_rows(fixture: &Fixture, tool: &str) -> Vec<(String, Value)> {
    let conn = rusqlite::Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
    conn.prepare(
        "SELECT status, application_json FROM audit_events \
         WHERE tool_name = ?1 AND application_json IS NOT NULL",
    )
    .unwrap()
    .query_map([tool], |row| {
        let raw: String = row.get(1)?;
        Ok((row.get(0)?, serde_json::from_str(&raw).unwrap()))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

/// A plugin's bound accept runs end to end on application authority alone:
/// it reads the bound task and payload and walks the task to `done`. The
/// same backend is refused a second task id and an unlisted callback.
#[test]
fn application_bound_accept_runs_callbacks_and_refuses_another_task_or_unlisted_call() {
    if !orbit_exec::macos_sandbox_test_guard(
        "application_bound_accept_runs_callbacks_and_refuses_another_task_or_unlisted_call",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let accepted = proposal_task(&fixture, "Accept me", PAYLOAD);
    let other = proposal_task(&fixture, "Leave me alone", PAYLOAD);
    let show = json!({"id": accepted, "fields": ["id", "status", "tags"]});
    let read = json!({"id": accepted, "path": "proposal.json"});
    let start = json!({"id": accepted, "status": "in-progress",
                       "plan": "Apply the reviewed proposal", "comment": "Accepted in the app"});
    let review = json!({"id": accepted, "status": "review", "execution_summary": "Applied"});
    let done = json!({"id": accepted, "status": "done", "execution_summary": "Applied and closed"});
    let other_show = json!({"id": other, "fields": ["status"]});
    let other_start = json!({"id": other, "status": "in-progress",
                             "plan": "Not approved", "comment": "Not approved"});
    let comment_only = json!({"id": accepted, "comment": "Never consented"});
    let script = format!(
        r#"#!/bin/sh
cat >/dev/null
run() {{ orbit tool run "$1" --input "$2" --format json; }}
run orbit.task.show '{show}' | grep -q '"proposal"' || exit 71
run orbit.task.artifact.get '{read}' | grep -q 'bound-payload-5c1e' || exit 72
if run orbit.task.show '{other_show}' >/dev/null 2>&1; then exit 73; fi
if run orbit.task.update '{other_start}' >/dev/null 2>&1; then exit 74; fi
if run orbit.task.update '{comment_only}' >/dev/null 2>&1; then exit 75; fi
if run orbit.task.list '{{}}' >/dev/null 2>&1; then exit 76; fi
run orbit.task.update '{start}' >/dev/null || exit 77
run orbit.task.update '{review}' >/dev/null || exit 78
run orbit.task.update '{done}' >/dev/null || exit 79
printf '{{"ok":true,"output":{{"accepted":"bound-accept-ok"}}}}\n'
"#
    );
    let plugin = install_proposal_plugin(&fixture, &script);
    let request = json!({
        "application_id": "office-app",
        "expires_at": expires_in(7),
        "scopes": [accept_scope(&plugin)],
    });
    let (_, credential) = consent(&fixture, &request, "bound-accept", None);
    let entry = json!({"id": accepted, "sha256": sha256_hex(PAYLOAD.as_bytes())});
    let output = app_command(&fixture, &credential, "proposal.accept", &entry)
        .assert()
        .get_output()
        .clone();
    assert!(
        output.status.success(),
        "bound accept failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("bound-accept-ok"));

    let task = fixture.json(["task", "show", &accepted]);
    assert_eq!(task["status"], "done");
    let shown = task.to_string();
    assert!(shown.contains("Accepted in the app"));
    assert!(shown.contains("application:office-app"));
    assert!(!shown.contains("Never consented"));
    assert_eq!(fixture.json(["task", "show", &other])["status"], "proposed");

    // Each callback is audited under the entry's bindings, and the second
    // task id and the unlisted call are audited refusals.
    for tool in [
        "orbit.task.show",
        "orbit.task.artifact.get",
        "orbit.task.update",
    ] {
        let rows = application_rows(&fixture, tool);
        assert!(
            rows.iter().any(|(status, app)| status == "success"
                && app["callback"] == true
                && app["bindings"]["id"] == accepted.as_str()),
            "{tool}: {rows:?}"
        );
    }
    assert!(
        application_rows(&fixture, "orbit.task.show")
            .iter()
            .any(|(status, app)| status == "denied" && app["callback"] == true)
    );
    let update_denials = application_rows(&fixture, "orbit.task.update")
        .into_iter()
        .filter(|(status, app)| status == "denied" && app["callback"] == true)
        .count();
    assert!(update_denials >= 2, "{update_denials}");

    // Each callback row names the entry dispatch, and `orbit audit show` of
    // that entry lists the callbacks.
    let conn = rusqlite::Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
    let (entry_id, entry_execution_id): (i64, String) = conn
        .query_row(
            "SELECT id, execution_id FROM audit_events \
             WHERE tool_name = 'proposal.accept' AND status = 'success' \
               AND application_json IS NOT NULL \
             ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    for tool in [
        "orbit.task.show",
        "orbit.task.artifact.get",
        "orbit.task.update",
    ] {
        let rows = application_rows(&fixture, tool);
        assert!(
            rows.iter().any(|(status, app)| {
                status == "success"
                    && app["callback"] == true
                    && app["entry_execution_id"] == entry_execution_id
                    && app["scope_name"] == "proposal-accept"
                    && app["match"] == "constrained"
            }),
            "{tool} callback must record the entry execution id {entry_execution_id}: {rows:?}"
        );
    }
    let shown = String::from_utf8(
        fixture
            .command(["audit", "show", &entry_id.to_string()])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        shown.contains("Scope:             proposal-accept (0)"),
        "{shown}"
    );
    assert!(shown.contains("Match:             constrained"), "{shown}");
    assert!(shown.contains("Callbacks:"), "{shown}");
    assert!(shown.contains("orbit.task.show"), "{shown}");
    assert!(shown.contains("orbit.task.artifact.get"), "{shown}");
    assert!(!shown.contains("Entry execution:"), "{shown}");
}

/// Drive callbacks over a real host route without a sandboxed backend: the
/// route is the only way an application callback reaches its consent, so the
/// changed-payload and pin refusals, and the schema 4 session reader, are
/// exercised exactly where a backend's would be.
#[test]
fn application_callbacks_refuse_changed_artifact_and_second_pin_and_read_schema_4_sessions() {
    if std::env::var_os("ORBIT_APPLICATION_CALLBACK_FIXTURE_CHILD").is_none() {
        let output = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let name = "plugin_cli_group::application_callbacks::application_callbacks_refuse_changed_artifact_and_second_pin_and_read_schema_4_sessions";
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        orbit_common::test_env::clear_inherited_authority(|key| {
            child.env_remove(key);
        });
        child
            .args(["--exact", name, "--nocapture"])
            .env("ORBIT_APPLICATION_CALLBACK_FIXTURE_CHILD", "1")
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
    use orbit_types::tool::{
        ApplicationArtifactEvidence, ApplicationInvocation, ApplicationPredicateEvidence,
    };
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
    let bound = proposal_task(&fixture, "Bound payload", PAYLOAD);
    let first = proposal_task(&fixture, "Pinned first", PAYLOAD);
    let second = proposal_task(&fixture, "Pinned second", PAYLOAD);
    let plugin = install_proposal_plugin(&fixture, "#!/bin/sh\nexit 70\n");
    let pin_scope = json!({
        "name": "draft-send",
        "operation": {
            "tool": "proposal.accept",
            "plugin": plugin,
            "fields": {"id": {"token": {"max_len": 64}}, "sha256": {"sha256": {}}}
        },
        "callbacks": [
            {"tool": "orbit.task.show", "plugin": null,
             "fields": {"id": {"task": {"tags_all": ["proposal"], "status": ["proposed"], "pin": true}},
                        "fields": {"subset_of": ["status"]}}}
        ]
    });
    let request = json!({
        "application_id": "office-app",
        "expires_at": expires_in(7),
        "scopes": [accept_scope(&plugin), pin_scope],
    });
    let (grant, _) = consent(&fixture, &request, "callback-route", None);
    let consent_record: orbit_types::tool::ApplicationConsent =
        serde_json::from_value(grant).unwrap();
    let digest = sha256_hex(PAYLOAD.as_bytes());

    // What admission of the entry `{id: bound, sha256}` recorded.
    let mut bound_entry = ApplicationInvocation::new(consent_record.clone(), 0, false);
    bound_entry.bindings.insert("id".into(), json!(bound));
    bound_entry.bindings.insert("sha256".into(), json!(digest));
    bound_entry.predicate = Some(ApplicationPredicateEvidence {
        task: bound.clone(),
        status_seen: "proposed".into(),
        tags_matched: vec!["proposal".into()],
        artifact: Some(ApplicationArtifactEvidence {
            path: "proposal.json".into(),
            sha256: digest.clone(),
        }),
    });
    let mut pin_entry = ApplicationInvocation::new(consent_record, 1, false);
    pin_entry.bindings.insert("id".into(), json!("draft-1"));
    pin_entry.bindings.insert("sha256".into(), json!(digest));

    let global = fixture.home.join(".orbit");
    let runtime = OrbitRuntime::from_roots(&global, &fixture.work.join(".orbit")).unwrap();
    let host: Arc<dyn OrbitToolHost> = Arc::new(Host(runtime));
    let provenance = orbit_types::plugin::PluginProvenance {
        name: "proposal".into(),
        version: "0.1.0".into(),
        manifest_digest: plugin["manifest_digest"].as_str().unwrap().into(),
        grants: vec![],
    };
    let session = |entry: &ApplicationInvocation| {
        let mut callback = PluginCallbackSession::mint(
            &global,
            &provenance,
            &["orbit.task.artifact.get".into(), "orbit.task.show".into()],
        )
        .unwrap();
        callback.bind_application(Some(entry)).unwrap();
        callback.open_host_route(Some(&host)).unwrap();
        callback
    };
    let run = |callback: &PluginCallbackSession, tool: &str, input: &Value| {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_orbit"));
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let credential_fd = callback.credential_fd();
        let route_fd = callback.host_route_fd().expect("an application route");
        command
            .current_dir(&fixture.work)
            .env("HOME", &fixture.home)
            .env("USERPROFILE", &fixture.home)
            .env("ORBIT_PLUGIN_CALLBACK", "descriptor-required")
            .args([
                "tool",
                "run",
                tool,
                "--input",
                &input.to_string(),
                "--format",
                "json",
            ]);
        // SAFETY: dup2 only maps the two live host-owned fds in the child;
        // the session stays alive until output() reaps it.
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
    let succeeds = |output: &std::process::Output| {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let refused = |output: &std::process::Output| {
        assert!(!output.status.success(), "the callback must be refused");
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    // A changed artifact: the payload the entry bound is read once, then the
    // operator replaces it and every bound callback is refused.
    let accept = session(&bound_entry);
    let read = json!({"id": bound, "path": "proposal.json"});
    assert!(
        succeeds(&run(&accept, "orbit.task.artifact.get", &read)).contains("bound-payload-5c1e")
    );
    put_payload(
        &fixture,
        &bound,
        "{\"kind\":\"knowledge\",\"marker\":\"swapped\"}",
    );
    let stderr = refused(&run(&accept, "orbit.task.artifact.get", &read));
    assert!(stderr.contains("bound artifact changed"), "{stderr}");
    assert!(!stderr.contains("swapped"), "{stderr}");
    let stderr = refused(&run(
        &accept,
        "orbit.task.show",
        &json!({"id": bound, "fields": ["status"]}),
    ));
    assert!(stderr.contains("bound artifact changed"), "{stderr}");

    // A pin: the first admitted task binds the invocation; a second task that
    // satisfies the same predicate is refused, the first still admitted.
    let send = session(&pin_entry);
    let show = |id: &str| json!({"id": id, "fields": ["status"]});
    assert!(succeeds(&run(&send, "orbit.task.show", &show(&first))).contains("proposed"));
    let stderr = refused(&run(&send, "orbit.task.show", &show(&second)));
    assert!(
        stderr.contains("completion callback is outside application consent"),
        "{stderr}"
    );
    succeeds(&run(&send, "orbit.task.show", &show(&first)));
    // A fresh session is a fresh invocation, with nothing pinned.
    drop(send);
    let next = session(&pin_entry);
    succeeds(&run(&next, "orbit.task.show", &show(&second)));

    // A backend a pre-upgrade host started holds a schema 4 record, its
    // application binding included. Its child still resolves the record and
    // reaches the host, so a long-lived backend survives the upgrade.
    let mut record: Value = serde_json::from_slice(&std::fs::read(next.path()).unwrap()).unwrap();
    assert_eq!(record["schema_version"], 5);
    record["schema_version"] = json!(4);
    // In place: the child's descriptor and the record's token name one inode.
    std::fs::write(next.path(), record.to_string()).unwrap();
    succeeds(&run(&next, "orbit.task.show", &show(&second)));
}
