#![allow(missing_docs)]
// ORB-00262: integration coverage for the activity-scoped `proc.spawn`
// allowlist. Fixture setup uses unwrap/expect for readability.
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
fn missing_filesystem_policy_denies_an_allowed_scoped_program() {
    let ctx = ToolContext {
        proc_allowed_programs: vec!["/bin/echo".to_string()],
        proc_spawn_activity_scoped: true,
        ..Default::default()
    };
    let err = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({ "program": "/bin/echo", "args": ["must-not-run"] }),
        )
        .expect_err("missing activity fsProfile must fail closed");
    assert!(matches!(err, OrbitError::PolicyDenied(_)));
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
fn restrictive_fs_profile_not_bypassed_via_proc_spawn() {
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

    let err = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({ "program": "/bin/cat", "args": [denied], "timeout_ms": 5000 }),
        )
        .expect_err("allowed program must not read a denied path");
    assert!(matches!(err, OrbitError::PolicyDenied(_)));
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

fn restricted_policy() -> PolicyDef {
    policy_with_profile("restricted", vec!["./allowed/**".to_string()])
}

fn policy_with_profile(name: &str, read: Vec<String>) -> PolicyDef {
    policy_with_denies(name, read, Vec::new())
}

fn policy_with_denies(name: &str, read: Vec<String>, deny_read: Vec<String>) -> PolicyDef {
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
        deny_read,
        deny_modify: Vec::new(),
        fs_profiles,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn scoped_workspace_context(
    workspace_root: PathBuf,
    programs: Vec<String>,
    deny_read: Vec<String>,
) -> ToolContext {
    ToolContext {
        workspace_root: Some(workspace_root),
        policy_engine: Some(Arc::new(
            PolicyEngine::from_def(&policy_with_denies(
                "unrestricted",
                vec!["./**".to_string()],
                deny_read,
            ))
            .expect("policy"),
        )),
        fs_profile: Some("unrestricted".to_string()),
        proc_allowed_programs: programs,
        proc_spawn_activity_scoped: true,
        proc_spawn_environment: Some(vec![(
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
        )]),
        ..Default::default()
    }
}

fn git_init(workspace: &std::path::Path) {
    let status = std::process::Command::new("git")
        .args(["init"])
        .current_dir(workspace)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run git init");
    assert!(status.success(), "git init failed: {status:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn git_shell_alias_cannot_read_a_host_sentinel() {
    let workspace = tempdir().expect("workspace");
    let host = tempdir().expect("host");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    git_init(&workspace_root);
    let sentinel = host
        .path()
        .canonicalize()
        .expect("canonical host")
        .join("sentinel.txt");
    fs::write(&sentinel, "HOST_SENTINEL_ORB11514").expect("write sentinel");

    let ctx = scoped_workspace_context(workspace_root, vec!["git".to_string()], Vec::new());
    let alias = format!("alias.orbitsecurityprobe=!cat {}", sentinel.display());
    let value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({
                "program": "git",
                "args": ["-c", alias, "orbitsecurityprobe"],
                "timeout_ms": 5000,
            }),
        )
        .expect("git alias should be admitted as a program");
    let stdout = value["stdout"].as_str().unwrap_or_default();
    assert!(
        !stdout.contains("HOST_SENTINEL_ORB11514"),
        "git shell alias must not return the host sentinel, got {value:?}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn git_shell_alias_cannot_read_a_deny_read_file() {
    let workspace = tempdir().expect("workspace");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    git_init(&workspace_root);
    let secret = workspace_root.join(".env");
    fs::write(&secret, "DENY_READ_SECRET").expect("write secret");

    let ctx = scoped_workspace_context(
        workspace_root.clone(),
        vec!["git".to_string()],
        vec!["**/.env".to_string()],
    );
    let alias = format!("alias.orbitsecurityprobe=!cat {}", secret.display());
    let value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({
                "program": "git",
                "args": ["-c", alias, "orbitsecurityprobe"],
                "timeout_ms": 5000,
            }),
        )
        .expect("git alias should be admitted as a program");
    let stdout = value["stdout"].as_str().unwrap_or_default();
    assert!(
        !stdout.contains("DENY_READ_SECRET"),
        "git shell alias must not return denyRead contents, got {value:?}"
    );
}

#[test]
fn git_minus_c_etc_remains_policy_denied() {
    let workspace = tempdir().expect("workspace");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let ctx = scoped_workspace_context(workspace_root, vec!["git".to_string()], Vec::new());
    let err = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({
                "program": "git",
                "args": ["-C", "/etc", "rev-parse", "--show-toplevel"],
                "timeout_ms": 5000,
            }),
        )
        .expect_err("git -C /etc must stay policy_denied");
    assert!(matches!(err, OrbitError::PolicyDenied(_)), "{err:?}");
    let message = err.to_string();
    assert!(
        message.contains("/etc") && message.contains("<outside workspace>"),
        "direct /etc control should name the path and outside-workspace rule, got {message}"
    );
}

#[test]
fn allowlisted_git_rev_parse_inside_workspace_still_works() {
    let workspace = tempdir().expect("workspace");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    git_init(&workspace_root);
    let ctx = scoped_workspace_context(workspace_root.clone(), vec!["git".to_string()], Vec::new());
    let value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({
                "program": "git",
                "args": ["rev-parse", "--show-toplevel"],
                "timeout_ms": 5000,
            }),
        )
        .expect("git rev-parse should run inside the workspace");
    let stdout = value["stdout"].as_str().unwrap_or_default().trim();
    let observed = PathBuf::from(stdout)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(stdout));
    assert_eq!(observed, workspace_root);
}

#[test]
fn allowlisted_rg_still_reads_workspace_files() {
    let rg = rg_program().expect("rg must be installed for the proc.spawn regression");
    let workspace = tempdir().expect("workspace");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let needle = workspace_root.join("needle.txt");
    fs::write(&needle, "rg-visible-token").expect("write needle");
    let ctx = scoped_workspace_context(workspace_root, vec![rg.clone()], Vec::new());
    let value = registry()
        .execute(
            "proc.spawn",
            &ctx,
            json!({
                "program": rg,
                "args": ["rg-visible-token", "needle.txt"],
                "timeout_ms": 5000,
            }),
        )
        .expect("rg should run inside the workspace");
    let stdout = value["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.contains("rg-visible-token"),
        "rg should return the workspace match, got {value:?}"
    );
}

fn rg_program() -> Option<String> {
    for candidate in ["/usr/bin/rg", "/bin/rg"] {
        if PathBuf::from(candidate).is_file() {
            return Some(candidate.to_string());
        }
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join("rg");
        candidate.is_file().then(|| candidate.display().to_string())
    })
}
