//! Script-step environment separation [ORB-20661].

use std::path::PathBuf;

use orbit_engine::ScriptStepRequest;
use orbit_types::workflow::activity_job::{ScriptGrant, ScriptGrantLimits};

use crate::application::job::scripts::execution::{script_env, script_launcher_env_from};

fn env_value<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// Regression for jrun-20261101-0131-t1: on dk-server-2 every contained
/// script step exited 1 in ~2 ms because the `systemd-run --user` launcher
/// was started from an allowlisted environment carrying neither user-bus
/// locator, while availability had been probed in the Orbit process's own
/// environment. The launcher must receive both locators (when the parent
/// holds them); the script's own environment must receive neither.
#[test]
fn script_launcher_env_carries_user_bus_locators_but_script_env_does_not() {
    let parent: Vec<(String, String)> = [
        ("PATH", "/usr/bin:/bin"),
        ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
        ("DATABASE_URL", "postgres://not-admitted"),
    ]
    .iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect();

    let launcher_env = script_launcher_env_from(&parent);
    assert_eq!(
        env_value(&launcher_env, "XDG_RUNTIME_DIR"),
        Some("/run/user/1000"),
        "jrun-20261101-0131-t1: the systemd-run launcher must receive \
         XDG_RUNTIME_DIR or it cannot reach the user manager"
    );
    assert_eq!(
        env_value(&launcher_env, "DBUS_SESSION_BUS_ADDRESS"),
        Some("unix:path=/run/user/1000/bus"),
        "jrun-20261101-0131-t1: the systemd-run launcher must receive \
         DBUS_SESSION_BUS_ADDRESS or it cannot reach the user manager"
    );
    assert_eq!(
        env_value(&launcher_env, "DATABASE_URL"),
        None,
        "the launcher environment stays allowlisted: unadmitted ambient \
         variables must not cross to the launcher"
    );

    // A locator the parent does not hold is simply absent, never invented.
    let partial: Vec<(String, String)> = [("XDG_RUNTIME_DIR", "/run/user/1000")]
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let launcher_env = script_launcher_env_from(&partial);
    assert_eq!(
        env_value(&launcher_env, "XDG_RUNTIME_DIR"),
        Some("/run/user/1000")
    );
    assert_eq!(env_value(&launcher_env, "DBUS_SESSION_BUS_ADDRESS"), None);

    let request = ScriptStepRequest {
        run_id: "run-1".to_string(),
        step_id: "step-1".to_string(),
        script: "fixture".to_string(),
        input: serde_json::json!({}),
        workspace_path: None,
        timeout_seconds: None,
    };
    let grant = ScriptGrant {
        grant_id: "sg-fixture".to_string(),
        workspace_id: "ws-fixture".to_string(),
        name: "fixture".to_string(),
        digest: "digest".to_string(),
        manifest_digest: "manifest".to_string(),
        entry_digest: "entry".to_string(),
        interpreter_paths: vec![PathBuf::from("/usr/bin/python3")],
        network: Default::default(),
        read_roots: Vec::new(),
        write_roots: Vec::new(),
        env_pass: Vec::new(),
        limits: ScriptGrantLimits::default(),
        host_role: Default::default(),
        grantor: "fixture".to_string(),
        granted_at: "2026-11-01T00:00:00Z".to_string(),
        provenance: "fixture".to_string(),
        expires_at: None,
    };
    let script_env = script_env(
        &request,
        "digest",
        &grant,
        std::path::Path::new(orbit_exec::SCRIPT_SANDBOX_MOUNT),
    );
    for name in ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"] {
        assert_eq!(
            env_value(&script_env, name),
            None,
            "jrun-20261101-0131-t1: {name} is for the systemd-run launcher \
             only and must never reach the script's own environment"
        );
    }
}

// Unit admission (test_strategy criterion 3, fail-closed validation): the
// attach action attaches only bytes that still match their recorded digest,
// driven at the public `RuntimeHost::run_deterministic` boundary. The
// crate-private output publisher and run-definition snapshot writer are its
// only fixtures; a crate-root replay would need a real script run, which the
// nested kernel sandbox refuses in agent lanes.
#[test]
#[cfg(unix)]
fn attach_script_files_attaches_verified_outputs_and_refuses_changed_bytes() {
    use orbit_engine::{RuntimeHost, ScriptStepEnd, ScriptStepOutputs, ScriptStepRan};
    use orbit_types::workflow::activity_job::{ScriptFileOutputs, script_sha256_hex};
    use orbit_types::workflow::{JobRunState, PipelineState};
    use serde_json::json;

    use crate::application::task::TaskAddParams;

    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &attach_script_files_attaches_verified_outputs_and_refuses_changed_bytes,
    )) {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let repo = root.path().join("repo");
    let global = root.path().join("home/.orbit");
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::create_dir_all(&global).unwrap();
    let runtime = crate::OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let task = |title: &str| {
        runtime
            .add_task(TaskAddParams {
                title: title.to_string(),
                ..TaskAddParams::default()
            })
            .unwrap()
            .id
    };
    let reviewed = task("Review the rendered report");

    let row = runtime
        .stores()
        .jobs()
        .insert_job_run("attach_job", 1, chrono::Utc::now(), Some(json!({})), None)
        .unwrap();
    let run_id = row.run_id.clone();
    let grant = ScriptGrant {
        grant_id: "sg-fixture".to_string(),
        workspace_id: runtime.workspace_id().unwrap(),
        name: "render".to_string(),
        digest: "digest".to_string(),
        manifest_digest: "manifest".to_string(),
        entry_digest: "entry".to_string(),
        interpreter_paths: Vec::new(),
        network: Default::default(),
        read_roots: Vec::new(),
        write_roots: Vec::new(),
        env_pass: Vec::new(),
        limits: ScriptGrantLimits::default(),
        host_role: Default::default(),
        grantor: "fixture".to_string(),
        granted_at: "2026-11-01T00:00:00Z".to_string(),
        provenance: "fixture".to_string(),
        expires_at: None,
    };
    let out = root.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("report.json"), br#"{"ok":true}"#).unwrap();
    let png: &[u8] = &[137, 80, 78, 71, 13, 10, 26, 10, 0, 1, 2, 3];
    std::fs::write(out.join("shot.png"), png).unwrap();
    let request = ScriptStepRequest {
        run_id: run_id.clone(),
        step_id: "render".to_string(),
        script: "render".to_string(),
        input: json!({}),
        workspace_path: None,
        timeout_seconds: None,
    };
    let mut ran = ScriptStepRan {
        end: ScriptStepEnd::Exited(0),
        stdout: b"{}".to_vec(),
        stdout_over_cap: false,
        output_schema: serde_json::Value::Null,
        log: Vec::new(),
        record: Default::default(),
        outputs: ScriptStepOutputs::None,
    };
    crate::application::job::script_outputs::keep_declared_outputs(
        &runtime,
        &ScriptFileOutputs {
            max_files: Some(8),
            max_file_bytes: Some(1024),
        },
        &grant,
        &request,
        &out,
        &mut ran,
    );
    let ScriptStepOutputs::Kept(set) = ran.outputs else {
        panic!("outputs kept: {:?}", ran.outputs);
    };

    let yaml = "schemaVersion: 2\nkind: Job\nmetadata:\n  name: attach_job\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    \
                - id: render\n      target: script:render\n    - id: attach\n      \
                target: activity:attach_script_files\n";
    runtime
        .write_run_definition_snapshot(&run_id, yaml)
        .unwrap();
    let mut state = PipelineState::new(run_id.clone(), "attach_job".to_string(), json!({}));
    state.record_step(0, JobRunState::Success, Some(json!({})), None);
    state.next_step_index = 1;
    state
        .script_output_sets
        .insert(0, [("render".to_string(), set.clone())].into());
    runtime.write_run_state(&run_id, &state).unwrap();
    let context = orbit_tools::ToolContext {
        reservation_owner: Some(orbit_tools::ReservationOwnerContext {
            owner_run_id: run_id.clone(),
            owner_metadata: None,
        }),
        ..Default::default()
    };
    let attach = |input: serde_json::Value| {
        RuntimeHost::run_deterministic(
            &runtime,
            "attach_script_files",
            &json!({}),
            &input,
            context.clone(),
        )
    };

    let output = attach(json!({"task_id": reviewed, "step": "render", "path_prefix": "render/"}))
        .expect("attach the verified outputs");
    assert_eq!(output["set_id"], set.set_id);
    let mut artifacts = runtime.get_task_artifacts(&reviewed).unwrap();
    artifacts.sort_by(|a, b| a.path.cmp(&b.path));
    let paths: Vec<_> = artifacts.iter().map(|a| a.path.as_str()).collect();
    assert_eq!(paths, ["render/report.json", "render/shot.png"]);
    for (artifact, file) in artifacts.iter().zip(&set.files) {
        assert_eq!(script_sha256_hex(&artifact.content), file.sha256);
        assert_eq!(artifact.media_type, file.media_type);
    }
    assert_eq!(artifacts[1].content, png);

    // A selected name outside the set, or a step that kept no files, refuses.
    let refused = attach(json!({"task_id": reviewed, "step": "render", "files": ["other.txt"]}))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("not an output file"), "{refused}");
    let refused = attach(json!({"task_id": reviewed, "step": "attach"}))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("not a script step"), "{refused}");

    // Bytes that no longer match the recorded digest attach nothing.
    let later = task("Review a tampered report");
    std::fs::write(&set.files[1].path, b"not the png").unwrap();
    let refused = attach(json!({"task_id": later, "step": "render"}))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("changed"), "{refused}");
    assert!(runtime.get_task_artifacts(&later).unwrap().is_empty());
}
