//! Script-step environment separation [ORB-20661].

use std::path::PathBuf;

use orbit_engine::ScriptStepRequest;
use orbit_types::workflow::activity_job::{ScriptGrant, ScriptGrantLimits};

use crate::application::job::script_step::{script_env, script_launcher_env_from};

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
    let script_env = script_env(&request, "digest", &grant);
    for name in ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"] {
        assert_eq!(
            env_value(&script_env, name),
            None,
            "jrun-20261101-0131-t1: {name} is for the systemd-run launcher \
             only and must never reach the script's own environment"
        );
    }
}
