#![allow(missing_docs)]
// Integration coverage for activity-scoped `proc.spawn` program policy and
// inherited parent read access. Fixture setup uses unwrap/expect for readability.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_policy::PolicyEngine;
use orbit_tools::{ToolContext, ToolRegistry};
use orbit_types::policy::{FsProfile, PolicyDef};
use serde_json::{Value, json};
use tempfile::tempdir;

/// The `denyRead` set shipped in `crates/orbit-core/assets/policies/default.yaml`.
const DEFAULT_DENY_READ: &[&str] = &["**/.env", "**/.env.*", "**/*.env", "**/*.env.*"];

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    registry
}

fn unrestricted_activity_context(programs: Vec<String>) -> ToolContext {
    let workspace_root = std::env::current_dir()
        .expect("current directory")
        .canonicalize()
        .expect("canonical current directory");
    ToolContext {
        workspace_root: Some(workspace_root),
        policy_engine: Some(Arc::new(
            PolicyEngine::from_def(&policy_with_profile(
                "unrestricted",
                vec!["./**".to_string()],
            ))
            .expect("unrestricted policy"),
        )),
        fs_profile: Some("unrestricted".to_string()),
        proc_allowed_programs: programs,
        proc_spawn_activity_scoped: true,
        ..Default::default()
    }
}

#[test]
fn disallowed_program_denied_when_activity_scoped() {
    let ctx = ToolContext {
        proc_allowed_programs: vec!["git".to_string()],
        proc_spawn_activity_scoped: true,
        ..Default::default()
    };
    let err = registry()
        .execute("proc.spawn", &ctx, json!({ "program": "sh" }))
        .expect_err("disallowed program must be denied");
    assert!(matches!(err, OrbitError::PolicyDenied(_)));
}

#[test]
fn empty_allowlist_denies_every_program_when_scoped() {
    let ctx = ToolContext {
        proc_allowed_programs: Vec::new(),
        proc_spawn_activity_scoped: true,
        ..Default::default()
    };
    let err = registry()
        .execute("proc.spawn", &ctx, json!({ "program": "git" }))
        .expect_err("empty scoped allowlist must deny");
    assert!(matches!(err, OrbitError::PolicyDenied(_)));
}

#[test]
fn missing_filesystem_policy_does_not_add_a_child_read_boundary() {
    let ctx = ToolContext {
        proc_allowed_programs: vec!["/bin/echo".to_string()],
        proc_spawn_activity_scoped: true,
        ..Default::default()
    };
    let value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({ "program": "/bin/echo", "args": ["must-not-run"] }),
        )
        .expect("proc.spawn does not need an inner filesystem profile");
    assert_eq!(value["stdout"], json!("must-not-run\n"));
}

#[test]
fn allowed_program_runs_under_lockdown() {
    let ctx = ToolContext {
        proc_spawn_environment: Some(vec![("PATH".to_string(), "/usr/bin:/bin".to_string())]),
        ..unrestricted_activity_context(vec!["echo".to_string(), "/bin/echo".to_string()])
    };
    let value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({
                "program": "/bin/echo",
                "args": ["ok"],
                "timeout_ms": 5000,
            }),
        )
        .expect("allowed program should run");
    let stdout = value["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.contains("ok"),
        "expected `ok` in stdout, got: {stdout:?}"
    );
}

#[test]
fn ambient_credential_is_excluded_unless_policy_admits_it() {
    let ctx = ToolContext {
        proc_spawn_environment: Some(vec![("PATH".to_string(), "/usr/bin:/bin".to_string())]),
        ..unrestricted_activity_context(vec!["/usr/bin/env".to_string()])
    };
    let value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({ "program": "/usr/bin/env", "timeout_ms": 5000 }),
        )
        .expect("allowlisted env should run");
    assert!(
        !value["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("DATABASE_URL=")
    );

    let admitted = ToolContext {
        proc_spawn_environment: Some(vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("DATABASE_URL".to_string(), "test-sentinel".to_string()),
        ]),
        ..ctx
    };
    let value = registry()
        .execute(
            "proc.spawn",
            &admitted,
            json!({ "program": "/usr/bin/env", "timeout_ms": 5000 }),
        )
        .expect("explicitly admitted env should run");
    assert!(
        value["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("DATABASE_URL=test-sentinel")
    );
}

#[test]
fn legacy_unrestricted_path_preserved_when_not_scoped() {
    let ctx = ToolContext {
        // No allowlist, not activity-scoped — legacy v1/direct-CLI behavior.
        ..Default::default()
    };
    registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({
                "program": "/bin/echo",
                "args": ["legacy"],
                "timeout_ms": 5000,
            }),
        )
        .expect("legacy unrestricted path should still permit echo");
}

#[test]
fn restrictive_fs_profile_does_not_narrow_the_parent_read_view() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root: PathBuf = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace path");

    let ctx = ToolContext {
        workspace_root: Some(workspace_root.clone()),
        policy_engine: Some(Arc::new(
            PolicyEngine::from_def(&restricted_policy()).expect("policy"),
        )),
        fs_profile: Some("restricted".to_string()),
        proc_allowed_programs: vec!["/bin/cat".to_string()],
        proc_spawn_activity_scoped: true,
        ..Default::default()
    };

    let denied = workspace_root.join("denied.txt");
    fs::write(&denied, "must stay private").expect("write denied fixture");

    let value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({ "program": "/bin/cat", "args": [denied], "timeout_ms": 5000 }),
        )
        .expect("child inherits its parent's read view");
    assert_eq!(value["stdout"], json!("must stay private"));
}

#[test]
fn allowed_program_can_read_an_allowed_path() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let allowed_dir = workspace_root.join("allowed");
    fs::create_dir(&allowed_dir).expect("create allowed fixture dir");
    let allowed = allowed_dir.join("visible.txt");
    fs::write(&allowed, "visible").expect("write allowed fixture");
    let ctx = ToolContext {
        workspace_root: Some(workspace_root),
        policy_engine: Some(Arc::new(
            PolicyEngine::from_def(&restricted_policy()).expect("policy"),
        )),
        fs_profile: Some("restricted".to_string()),
        proc_allowed_programs: vec!["/bin/cat".to_string()],
        proc_spawn_activity_scoped: true,
        ..Default::default()
    };

    let value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({ "program": "/bin/cat", "args": [allowed], "timeout_ms": 5000 }),
        )
        .expect("allowed path should be readable through allowed program");
    assert_eq!(value["stdout"].as_str().unwrap_or_default(), "visible");
}

#[test]
fn spawn_runs_inside_workspace_root() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root: PathBuf = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace path");

    let ctx = ToolContext {
        workspace_root: Some(workspace_root.clone()),
        policy_engine: Some(Arc::new(
            PolicyEngine::from_def(&policy_with_profile(
                "unrestricted",
                vec!["./**".to_string()],
            ))
            .expect("policy"),
        )),
        fs_profile: Some("unrestricted".to_string()),
        proc_allowed_programs: vec!["/bin/pwd".to_string(), "pwd".to_string()],
        proc_spawn_activity_scoped: true,
        ..Default::default()
    };

    let value: Value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({ "program": "/bin/pwd", "timeout_ms": 5000 }),
        )
        .expect("pwd should run");
    let stdout = value["stdout"].as_str().unwrap_or_default().trim();
    let observed: PathBuf = PathBuf::from(stdout)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(stdout));
    assert_eq!(
        observed, workspace_root,
        "expected pwd inside workspace_root ({workspace_root:?}), got {observed:?}"
    );
}

/// An activity context over `workspace_root` with the shipped `denyRead` set,
/// so the regressions below reason about the profile agents actually run with.
fn workspace_activity_context(workspace_root: &std::path::Path, programs: &[&str]) -> ToolContext {
    let mut policy = policy_with_profile("unrestricted", vec!["./**".to_string()]);
    policy.deny_read = DEFAULT_DENY_READ.iter().map(ToString::to_string).collect();
    ToolContext {
        workspace_root: Some(workspace_root.to_path_buf()),
        policy_engine: Some(Arc::new(PolicyEngine::from_def(&policy).expect("policy"))),
        fs_profile: Some("unrestricted".to_string()),
        proc_allowed_programs: programs.iter().map(ToString::to_string).collect(),
        proc_spawn_activity_scoped: true,
        proc_spawn_environment: Some(vec![("PATH".to_string(), path_env())]),
        ..Default::default()
    }
}

fn path_env() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string())
}

fn on_path(program: &str) -> bool {
    std::env::split_paths(&path_env()).any(|dir| dir.join(program).is_file())
}

fn stdout_of(value: &Value) -> String {
    value["stdout"].as_str().unwrap_or_default().to_string()
}

fn restricted_policy() -> PolicyDef {
    policy_with_profile("restricted", vec!["./allowed/**".to_string()])
}

fn policy_with_profile(name: &str, read: Vec<String>) -> PolicyDef {
    let mut fs_profiles = HashMap::new();
    fs_profiles.insert(
        name.to_string(),
        FsProfile {
            read: read.clone(),
            modify: read,
        },
    );

    PolicyDef {
        name: "test".to_string(),
        description: None,
        deny_read: Vec::new(),
        deny_modify: Vec::new(),
        fs_profiles,
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
    }
}

/// `git` is on shipped activity program lists and runs a `!` alias through a
/// shell. That child sees exactly the parent's read view. The outer OS sandbox
/// supplies any masks; this fixture runs without one. [ORB-13689]
#[test]
fn a_git_shell_alias_can_read_a_benign_host_file_visible_to_its_parent() {
    if !on_path("git") {
        return;
    }
    let workspace = tempdir().expect("workspace tempdir");
    let host = tempdir().expect("host tempdir");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let sentinel = host
        .path()
        .canonicalize()
        .expect("canonical host")
        .join("sentinel.txt");
    fs::write(&sentinel, "HOST_SENTINEL_ORB11514").expect("write host sentinel");

    let ctx = workspace_activity_context(&workspace_root, &["git"]);
    let value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({
                "program": "git",
                "args": [
                    "-c",
                    format!("alias.orbitsecurityprobe=!cat {}", sentinel.display()),
                    "orbitsecurityprobe",
                ],
                "timeout_ms": 10000,
            }),
        )
        .expect("the alias may run");

    assert!(
        stdout_of(&value).contains("HOST_SENTINEL_ORB11514"),
        "the host file should be readable to the child: {value:?}"
    );
}

/// Without an enclosing OS read mask, the activity's `denyRead` no longer
/// governs a subprocess. Other filesystem tools still use that profile.
#[test]
fn a_git_shell_alias_inherits_parent_access_to_a_deny_read_file() {
    if !on_path("git") {
        return;
    }
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    fs::write(workspace_root.join(".env"), "DENY_READ_SECRET").expect("write secret");
    fs::write(workspace_root.join("notes.txt"), "ALLOWED_CONTENT").expect("write allowed");

    let ctx = workspace_activity_context(&workspace_root, &["git"]);
    let alias = |target: &str| {
        registry()
            .execute(
                "proc.spawn",
                &ctx,
                json!({
                    "program": "git",
                    "args": [
                        "-c",
                        format!("alias.orbitsecurityprobe=!cat {target}"),
                        "orbitsecurityprobe",
                    ],
                    "timeout_ms": 10000,
                }),
            )
            .expect("the alias may run")
    };

    // The trampoline itself works for ordinary files and for paths denied by
    // the separate activity read profile when no outer mask covers them.
    assert!(
        stdout_of(&alias("notes.txt")).contains("ALLOWED_CONTENT"),
        "the alias mechanism should still reach an allowed file"
    );
    assert!(
        stdout_of(&alias(".env")).contains("DENY_READ_SECRET"),
        "the child did not inherit the parent's read access"
    );
}

/// A direct path argument is no longer filtered by an activity read profile.
#[test]
fn an_explicit_benign_path_outside_the_workspace_is_readable() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let host = tempdir().expect("host tempdir");
    let outside = host.path().join("config.toml");
    fs::write(&outside, "benign host config").expect("write host fixture");
    let ctx = workspace_activity_context(&workspace_root, &["/bin/cat"]);

    let value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({
                "program": "/bin/cat",
                "args": [outside],
                "timeout_ms": 5000,
            }),
        )
        .expect("the outer OS sandbox decides access");
    assert_eq!(value["stdout"], json!("benign host config"));
}

/// Confinement is only useful if the allowlisted tools still work.
#[test]
fn allowlisted_git_and_rg_still_work_inside_the_workspace() {
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    fs::write(workspace_root.join("haystack.txt"), "needle\n").expect("write haystack");
    let ctx = workspace_activity_context(&workspace_root, &["git", "rg"]);

    if on_path("git") {
        let value = registry()
            .execute(
                "proc.spawn",
                &ctx,
                json!({ "program": "git", "args": ["init", "-q", "."], "timeout_ms": 10000 }),
            )
            .expect("git should run");
        assert_eq!(value["exit_code"], json!(0), "{value:?}");
        assert!(
            workspace_root.join(".git").is_dir(),
            "git init made no repo"
        );
    }

    if on_path("rg") {
        let value = registry()
            .execute(
                "proc.spawn",
                &ctx,
                json!({
                    "program": "rg",
                    "args": ["needle", "haystack.txt"],
                    "timeout_ms": 10000,
                }),
            )
            .expect("rg should run");
        assert!(stdout_of(&value).contains("needle"), "{value:?}");
    }
}
