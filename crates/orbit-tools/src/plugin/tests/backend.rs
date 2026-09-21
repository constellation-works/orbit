//! The granted profile is what the kernel enforces: a write outside it is
//! refused, `requires.programs` is bounded by the caller's allowlist, and
//! `unsandboxed` is the only way around either.

use orbit_types::plugin::{
    PluginFsPermissions, PluginGrant, PluginNetworkPermission, PluginPermissions, PluginSandbox,
};
use serde_json::json;

use super::support::{context, sandbox_unavailable, spec, stub_backend, tool};
use crate::{Tool, ToolContext};

/// Writes `$1`-style paths handed in via the envelope input: `inside` under
/// the granted state directory, `outside` beside the plugin root.
const WRITER_BACKEND: &str = "#!/bin/sh\ncat >/dev/null\nresult=ok\nif ! echo inside > \"$ORBIT_PLUGIN_STATE/inside.txt\" 2>/dev/null; then result=inside_denied; fi\nif echo outside > \"$OUTSIDE\" 2>/dev/null; then result=\"$result,outside_written\"; fi\nprintf '{\"ok\":true,\"output\":{\"result\":\"%s\"}}\\n' \"$result\"\n";

fn fs_state_permissions() -> PluginPermissions {
    PluginPermissions {
        fs: PluginFsPermissions {
            read: vec![],
            write: vec!["{{plugin_state}}".into()],
        },
        ..PluginPermissions::default()
    }
}

#[cfg(unix)]
#[test]
fn a_write_outside_the_granted_fs_profile_is_denied_under_the_sandbox() {
    if sandbox_unavailable() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    std::fs::create_dir_all(&root).expect("plugin root");
    let outside = temp.path().join("outside.txt");
    let command = stub_backend(&root, WRITER_BACKEND);
    let tool = tool(
        spec(command, &root, fs_state_permissions(), &[PluginGrant::Fs]),
        None,
    );
    let ctx = ToolContext {
        proc_spawn_environment: Some(vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "OUTSIDE".to_string(),
                outside.to_string_lossy().into_owned(),
            ),
        ]),
        ..context(temp.path())
    };
    let output = tool.execute(&ctx, json!({})).expect("backend runs");
    assert_eq!(
        output["result"], "ok",
        "inside the grant writes; outside does not"
    );
    assert!(
        root.join("state/inside.txt").exists(),
        "the granted state directory was created and written"
    );
    assert!(
        !outside.exists(),
        "a write outside the granted profile must not reach the disk"
    );
}

#[cfg(unix)]
#[test]
fn unsandboxed_needs_the_grant_and_then_confines_nothing() {
    if sandbox_unavailable() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    std::fs::create_dir_all(&root).expect("plugin root");
    let outside = temp.path().join("outside.txt");
    let command = stub_backend(&root, WRITER_BACKEND);
    let ctx = ToolContext {
        proc_spawn_environment: Some(vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "OUTSIDE".to_string(),
                outside.to_string_lossy().into_owned(),
            ),
        ]),
        ..context(temp.path())
    };

    // `sandbox: none` without the grant still runs confined: the loader
    // would have refused this plugin, and the backend spec fails closed.
    let mut confined = (*spec(
        command.clone(),
        &root,
        fs_state_permissions(),
        &[PluginGrant::Fs],
    ))
    .clone();
    confined.sandbox = PluginSandbox::None;
    let profile = confined.sandbox_profile(None).expect("profile");
    assert!(!profile.unsandboxed, "no grant, no escape");

    let mut unsandboxed = confined.clone();
    unsandboxed.grants.push(PluginGrant::Unsandboxed);
    let profile = unsandboxed.sandbox_profile(None).expect("profile");
    assert!(profile.unsandboxed);
    let output = tool(std::sync::Arc::new(unsandboxed), None)
        .execute(&ctx, json!({}))
        .expect("backend runs");
    assert_eq!(output["result"], "ok,outside_written");
    assert!(outside.exists(), "an unsandboxed backend writes anywhere");
}

#[test]
fn the_profile_follows_the_grants_not_the_requests() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let permissions = PluginPermissions {
        fs: PluginFsPermissions {
            read: vec!["{{workspace}}".into()],
            write: vec!["{{workspace}}/.cache".into(), "{{plugin_state}}".into()],
        },
        network: PluginNetworkPermission::Any,
        ..PluginPermissions::default()
    };
    let ungranted = spec(root.join("bin"), &root, permissions.clone(), &[]);
    let profile = ungranted
        .sandbox_profile(Some(temp.path()))
        .expect("profile");
    assert_eq!(profile.read, vec![root.clone()], "only the plugin root");
    assert!(profile.write.is_empty());
    assert_eq!(profile.network, PluginNetworkPermission::None);

    let granted = spec(
        root.join("bin"),
        &root,
        permissions,
        &[PluginGrant::Fs, PluginGrant::Network],
    );
    let profile = granted.sandbox_profile(Some(temp.path())).expect("profile");
    assert_eq!(profile.read, vec![root.clone(), temp.path().to_path_buf()]);
    assert_eq!(
        profile.write,
        vec![temp.path().join(".cache"), root.join("state")]
    );
    assert_eq!(profile.network, PluginNetworkPermission::Any);

    // `{{workspace}}` with no workspace is a refusal, not an empty path.
    let error = granted.sandbox_profile(None).unwrap_err().to_string();
    assert!(error.contains("no workspace"), "{error}");
}

#[test]
fn declared_programs_are_bounded_by_a_restricted_caller() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    let mut declared = (*spec(root.join("bin"), &root, PluginPermissions::default(), &[])).clone();
    declared.programs = vec!["git".into(), "rg".into()];

    // An unrestricted caller (direct CLI) imposes nothing.
    declared
        .enforce_programs(&context(temp.path()), "demo.hello")
        .expect("unrestricted");

    let restricted = ToolContext {
        proc_allowed_programs: vec!["git".into()],
        proc_spawn_activity_scoped: true,
        ..context(temp.path())
    };
    let error = declared
        .enforce_programs(&restricted, "demo.hello")
        .unwrap_err()
        .to_string();
    assert!(error.contains("'rg'"), "{error}");

    let env = declared.child_environment(&restricted, "/tmp", Some("demo.hello"));
    let programs = env
        .iter()
        .find(|(key, _)| key == "ORBIT_PROC_ALLOWED_PROGRAMS")
        .map(|(_, value)| value.as_str());
    assert_eq!(programs, Some("git,rg"));
    let allowed = env
        .iter()
        .find(|(key, _)| key == "ORBIT_ALLOWED_TOOLS")
        .map(|(_, value)| value.as_str());
    assert_eq!(allowed, Some(""), "always stamped, empty without the grant");
}
