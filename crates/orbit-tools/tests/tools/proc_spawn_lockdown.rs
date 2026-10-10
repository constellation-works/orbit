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

/// Command-line aliases must not disguise writes to the primary checkout's
/// shared Git configuration. [ORB-14114]
#[test]
fn command_line_options_cannot_hide_persistent_git_config_writes() {
    let workspace = tempdir().expect("workspace tempdir");
    let ctx = workspace_activity_context(workspace.path(), &["git"]);
    let registry = registry();
    // A regression must fail in an isolated repository, without letting the
    // now-allowed write reach the developer's or runner's shared Git config.
    let init = registry
        .execute(
            "proc.spawn",
            &ctx,
            json!({ "program": "git", "args": ["init", "-q"] }),
        )
        .expect("initialize isolated Git fixture");
    assert_eq!(init["exit_code"], json!(0), "{init:?}");
    let config = workspace.path().join(".git/config");
    let original = fs::read(&config).expect("read initial Git config");
    let cases: &[&[&str]] = &[
        &[
            "-c",
            "alias.x=config",
            "x",
            "remote.origin.url",
            "https://example.invalid/repo",
        ],
        &["-c", "alias.x=remote", "x", "add", "a", "b"],
        &[
            "-calias.x=config",
            "x",
            "remote.origin.url",
            "https://example.invalid/repo",
        ],
        &["-calias.x=remote", "x", "add", "a", "b"],
        &[
            "-c",
            "alias.x=!git config remote.origin.url https://example.invalid/repo",
            "x",
        ],
        &[
            "-c",
            "core.bare=false",
            "-c",
            "ALIAS.x=config",
            "x",
            "remote.origin.url",
            "x",
        ],
        &[
            "--config-env",
            "alias.x=ORBIT_GIT_ALIAS",
            "x",
            "remote.origin.url",
            "x",
        ],
        &["--config-env=alias.x=ORBIT_GIT_ALIAS", "x", "add", "a", "b"],
        &["--config-env=ALIAS.x=ORBIT_GIT_ALIAS", "x"],
        &["-c", "alias.x", "x"],
        &["-c", "alias.x=status", "status"],
        &[
            "--config-env",
            "core.bare=ORBIT_GIT_BARE",
            "config",
            "remote.origin.url",
            "x",
        ],
    ];
    for args in cases {
        let result = registry.execute(
            "proc.spawn",
            &ctx,
            json!({ "program": "git", "args": args }),
        );
        assert!(
            matches!(result, Err(OrbitError::PolicyDenied(_))),
            "command-line options must not bypass the persistent Git config guard: {args:?}: {result:?}"
        );
    }
    assert_eq!(fs::read(config).expect("read final Git config"), original);
}

#[test]
fn direct_git_config_and_remote_reads_remain_available() {
    assert!(
        on_path("git"),
        "Git is required to exercise read-only queries"
    );
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let mut ctx = workspace_activity_context(&workspace_root, &["git"]);
    ctx.proc_spawn_environment
        .as_mut()
        .expect("explicit child environment")
        .push(("ORBIT_GIT_BARE".to_string(), "false".to_string()));
    let registry = registry();
    let init = registry
        .execute(
            "proc.spawn",
            &ctx,
            json!({ "program": "git", "args": ["init", "-q"] }),
        )
        .expect("initialize isolated Git fixture");
    assert_eq!(init["exit_code"], json!(0), "{init:?}");
    let config = workspace_root.join(".git/config");
    let original =
        "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = https://example.invalid/repo\n";
    fs::write(&config, original).expect("seed isolated Git config");

    let cases: &[(&[&str], &str)] = &[
        (
            &["config", "--get", "remote.origin.url"],
            "https://example.invalid/repo",
        ),
        (&["remote", "-v"], "https://example.invalid/repo"),
        (
            &[
                "-c",
                "core.bare=false",
                "config",
                "--get",
                "remote.origin.url",
            ],
            "https://example.invalid/repo",
        ),
        (
            &[
                "--config-env=core.bare=ORBIT_GIT_BARE",
                "config",
                "--get",
                "core.bare",
            ],
            "false",
        ),
    ];
    for &(args, expected) in cases {
        let value = registry
            .execute(
                "proc.spawn",
                &ctx,
                json!({ "program": "git", "args": args }),
            )
            .expect("direct read-only queries must remain available");
        assert_eq!(value["exit_code"], json!(0), "{args:?}: {value:?}");
        assert!(stdout_of(&value).contains(expected), "{args:?}: {value:?}");
    }
    assert_eq!(
        fs::read_to_string(config).expect("read Git config"),
        original
    );
}

/// `git` is on shipped activity program lists. Its child sees exactly the
/// parent's read view. The outer OS sandbox supplies any masks; this fixture
/// runs without one. [ORB-13689]
#[test]
fn git_can_read_a_benign_host_file_visible_to_its_parent() {
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
                    "diff",
                    "--no-index",
                    "/dev/null",
                    sentinel,
                ],
                "timeout_ms": 10000,
            }),
        )
        .expect("Git may read a file visible to its parent");

    assert!(
        stdout_of(&value).contains("HOST_SENTINEL_ORB11514"),
        "the host file should be readable to the child: {value:?}"
    );
}

/// Without an enclosing OS read mask, the activity's `denyRead` no longer
/// governs a subprocess. Other filesystem tools still use that profile.
#[test]
fn git_inherits_parent_access_to_a_deny_read_file() {
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
    let diff = |target: &str| {
        registry()
            .execute(
                "proc.spawn",
                &ctx,
                json!({
                    "program": "git",
                    "args": ["diff", "--no-index", "/dev/null", target],
                    "timeout_ms": 10000,
                }),
            )
            .expect("Git may read a file visible to its parent")
    };

    // Git reads ordinary files and paths denied by
    // the separate activity read profile when no outer mask covers them.
    assert!(
        stdout_of(&diff("notes.txt")).contains("ALLOWED_CONTENT"),
        "Git should still reach an allowed file"
    );
    assert!(
        stdout_of(&diff(".env")).contains("DENY_READ_SECRET"),
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

/// A fake rustup that writes `macro.env.html` when a real default-profile
/// install would. `--profile minimal` and an already-present toolchain do not.
#[cfg(unix)]
const FAKE_RUSTUP: &str = r#"#!/bin/sh
set -eu
home="${RUSTUP_HOME:?}"
mkdir -p "$home"
printf '%s\n' "$@" > "$home/invoked-args"
case "${1:-}" in
  show|which|help|completions) exit 0 ;;
esac
profile=""
docs=0
prev=""
for arg in "$@"; do
  case "$prev" in
    --profile) profile=$arg ;;
    --component|-c)
      case "$arg" in
        *rust-docs*) docs=1 ;;
      esac
      ;;
  esac
  case "$arg" in
    --profile=*) profile=${arg#--profile=} ;;
  esac
  prev=$arg
done
if [ -z "$profile" ] && [ -f "$home/settings.toml" ]; then
  profile=$(awk -F= '/^profile[[:space:]]*=/{sub(/[[:space:]]*#.*/, "", $2); gsub(/[[:space:]"]/, "", $2); print $2; exit}' "$home/settings.toml")
fi
if [ -z "$profile" ]; then
  dir=$(pwd)
  while [ -n "$dir" ] && [ "$dir" != "/" ]; do
    file=""
    if [ -f "$dir/rust-toolchain" ]; then
      file="$dir/rust-toolchain"
    elif [ -f "$dir/rust-toolchain.toml" ]; then
      file="$dir/rust-toolchain.toml"
    fi
    if [ -n "$file" ]; then
      profile=$(awk -F= '/^[[:space:]]*profile[[:space:]]*=/{sub(/[[:space:]]*#.*/, "", $2); gsub(/[[:space:]"]/, "", $2); print $2; exit}' "$file")
      break
    fi
    dir=$(dirname "$dir")
  done
fi
install=0
if [ "${1:-}" = "toolchain" ] && [ "${2:-}" = "install" ]; then install=1; fi
if [ "${1:-}" = "install" ] || [ "${1:-}" = "update" ] || [ "${1:-}" = "default" ]; then install=1; fi
if [ "${1:-}" = "component" ] && [ "${2:-}" = "add" ]; then install=1; fi
if [ "${1:-}" = "run" ]; then install=1; fi
if [ "$profile" = "minimal" ] && [ "$docs" = "0" ]; then
  exit 0
fi
if [ "$install" = "0" ]; then
  for dir in "$home/toolchains"/*; do
    if [ -e "$dir" ]; then
      exit 0
    fi
  done
fi
marker="$home/toolchains/fake/share/doc/rust/html/core"
mkdir -p "$marker"
printf docs > "$marker/macro.env.html"
"#;

#[cfg(unix)]
fn install_fake_rustup(dir: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir).expect("bin dir");
    let rustup = dir.join("rustup");
    fs::write(&rustup, FAKE_RUSTUP).expect("fake rustup");
    let mut permissions = fs::metadata(&rustup).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&rustup, permissions).expect("executable");
    rustup
}

#[cfg(unix)]
fn docs_marker(home: &std::path::Path) -> PathBuf {
    home.join("toolchains/fake/share/doc/rust/html/core/macro.env.html")
}

#[cfg(unix)]
fn rustup_env(home: &std::path::Path, path: &std::path::Path) -> Vec<(String, String)> {
    vec![
        (
            "PATH".to_string(),
            format!("{}:/usr/bin:/bin", path.display()),
        ),
        ("RUSTUP_HOME".to_string(), home.display().to_string()),
        ("HOME".to_string(), path.display().to_string()),
    ]
}

#[cfg(unix)]
fn rustup_context(
    workspace: &std::path::Path,
    program: &std::path::Path,
    env: Vec<(String, String)>,
) -> ToolContext {
    ToolContext {
        workspace_root: Some(workspace.to_path_buf()),
        proc_allowed_programs: vec![program.display().to_string()],
        proc_spawn_activity_scoped: true,
        proc_spawn_environment: Some(env),
        ..Default::default()
    }
}

#[cfg(unix)]
fn assert_install_refused(err: OrbitError) {
    let message = err.to_string();
    assert!(
        matches!(err, OrbitError::PolicyDenied(_)),
        "expected PolicyDenied, got {message}"
    );
    assert!(
        message.contains("default-profile rustup toolchain install"),
        "error must name the install: {message}"
    );
    assert!(
        message.contains("denyModify") && message.contains("**/*.env.*"),
        "error must name the denyModify rule **/*.env.*: {message}"
    );
}

#[cfg(unix)]
fn host_triple() -> String {
    // Keep this in step with `rustup_host_triple` in `proc/rustup_install.rs`.
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "linux" => {
            let abi = if cfg!(target_env = "musl") {
                "musl"
            } else {
                "gnu"
            };
            format!("{arch}-unknown-linux-{abi}")
        }
        "macos" => format!("{arch}-apple-darwin"),
        other => format!("{arch}-unknown-{other}"),
    }
}

/// [ORB-14337] A default-profile install rooted in the workspace is refused
/// before rust-docs can create `macro.env.html`. The same fake rustup writes
/// that file when it is actually executed.
#[cfg(unix)]
#[test]
fn default_profile_rustup_install_inside_workspace_is_refused_before_rust_docs() {
    let workspace = tempdir().expect("workspace");
    let workspace = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let bin = workspace.join("bin");
    let rustup = install_fake_rustup(&bin);
    let control = workspace.join("control-home");
    fs::create_dir_all(&control).expect("control home");
    let status = std::process::Command::new(&rustup)
        .args(["toolchain", "install", "1.96.0"])
        .env("RUSTUP_HOME", &control)
        .status()
        .expect("control install");
    assert!(status.success(), "control install failed: {status}");
    assert!(
        docs_marker(&control).is_file(),
        "the fixture must write macro.env.html when the install runs"
    );

    let home = workspace.join(".orbit/tmp/cross-target-rustup");
    fs::create_dir_all(&home).expect("rustup home");
    let err = registry()
        .execute(
            "proc.spawn",
            &rustup_context(&workspace, &rustup, rustup_env(&home, &bin)),
            json!({
                "program": rustup.display().to_string(),
                "args": ["toolchain", "install", "1.96.0"],
                "timeout_ms": 5000,
            }),
        )
        .expect_err("default-profile install inside the workspace must be refused");
    assert_install_refused(err);
    assert!(
        !docs_marker(&home).exists(),
        "refusal must happen before macro.env.html is created"
    );
    assert!(
        !home.join("invoked-args").exists(),
        "the rustup child must not start"
    );
}

/// The CodeQL scratch install stays allowed: absolute `RUSTUP_HOME` under the
/// workspace, `--profile minimal`, and no rust-docs component.
#[cfg(unix)]
#[test]
fn minimal_profile_install_on_codeql_scratch_path_succeeds() {
    let workspace = tempdir().expect("workspace");
    let workspace = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let bin = workspace.join("bin");
    let rustup = install_fake_rustup(&bin);
    let home = workspace.join(".orbit/tmp/codeql-rust-local.abc123/rustup");
    fs::create_dir_all(&home).expect("scratch rustup home");
    let args = [
        "toolchain",
        "install",
        "1.97.0",
        "--profile",
        "minimal",
        "--component",
        "rust-src",
        "--no-self-update",
    ];
    let value = registry()
        .execute(
            "proc.spawn",
            &rustup_context(&workspace, &rustup, rustup_env(&home, &bin)),
            json!({
                "program": rustup.display().to_string(),
                "args": args,
                "timeout_ms": 5000,
            }),
        )
        .expect("minimal install must run");
    assert_eq!(value["exit_code"], json!(0), "{value:?}");
    assert!(
        !docs_marker(&home).exists(),
        "minimal install must not write rust-docs"
    );
    let invoked = fs::read_to_string(home.join("invoked-args")).expect("rustup ran");
    assert_eq!(invoked, args.join("\n") + "\n");

    let err = registry()
        .execute(
            "proc.spawn",
            &rustup_context(&workspace, &rustup, rustup_env(&home, &bin)),
            json!({
                "program": rustup.display().to_string(),
                "args": ["toolchain", "install", "1.97.0", "--profile", "minimal", "--component", "rust-docs"],
                "timeout_ms": 5000,
            }),
        )
        .expect_err("minimal profile plus rust-docs still writes macro.env.html");
    assert_install_refused(err);
}

/// Cargo's rustup proxy auto-installs a missing toolchain with the default
/// profile. That path is the one that wrote `macro.env.html` under the worktree.
#[cfg(unix)]
#[test]
fn rustup_proxy_auto_install_inside_workspace_is_refused_until_toolchain_exists() {
    let workspace = tempdir().expect("workspace");
    let workspace = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let bin = workspace.join("bin");
    let rustup = install_fake_rustup(&bin);
    let cargo = bin.join("cargo");
    std::os::unix::fs::symlink(&rustup, &cargo).expect("cargo proxy");
    let home = workspace.join(".orbit/tmp/proxy-rustup");
    fs::create_dir_all(&home).expect("rustup home");

    let control = workspace.join("proxy-control");
    fs::create_dir_all(&control).expect("control");
    let status = std::process::Command::new(&cargo)
        .args(["+1.96.0", "clippy"])
        .env("RUSTUP_HOME", &control)
        .status()
        .expect("control proxy");
    assert!(status.success(), "control proxy failed: {status}");
    assert!(
        docs_marker(&control).is_file(),
        "proxy fixture must write docs"
    );

    let err = registry()
        .execute(
            "proc.spawn",
            &rustup_context(&workspace, &cargo, rustup_env(&home, &bin)),
            json!({
                "program": cargo.display().to_string(),
                "args": ["+1.96.0", "clippy"],
                "timeout_ms": 5000,
            }),
        )
        .expect_err("missing-toolchain proxy auto-install must be refused");
    assert_install_refused(err);
    assert!(!docs_marker(&home).exists());
    assert!(!home.join("invoked-args").exists());

    let installed = home
        .join("toolchains")
        .join(format!("1.96.0-{}", host_triple()));
    fs::create_dir_all(&installed).expect("preprovisioned toolchain");
    let value = registry()
        .execute(
            "proc.spawn",
            &rustup_context(&workspace, &cargo, rustup_env(&home, &bin)),
            json!({
                "program": cargo.display().to_string(),
                "args": ["+1.96.0", "clippy"],
                "timeout_ms": 5000,
            }),
        )
        .expect("already installed minimal toolchain must run");
    assert_eq!(value["exit_code"], json!(0), "{value:?}");
    assert!(!docs_marker(&home).exists());
    assert!(home.join("invoked-args").is_file());
}

#[cfg(unix)]
#[test]
fn minimal_settings_profile_auto_install_inside_workspace_succeeds() {
    let workspace = tempdir().expect("workspace");
    let workspace = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let bin = workspace.join("bin");
    let rustup = install_fake_rustup(&bin);
    let home = workspace.join(".orbit/tmp/minimal-profile");
    fs::create_dir_all(&home).expect("rustup home");
    fs::write(
        home.join("settings.toml"),
        "profile = \"minimal\" # rust-docs are not needed\n",
    )
    .expect("settings");
    let value = registry()
        .execute(
            "proc.spawn",
            &rustup_context(&workspace, &rustup, rustup_env(&home, &bin)),
            json!({
                "program": rustup.display().to_string(),
                "args": ["toolchain", "install", "1.97.0", "--no-self-update"],
                "timeout_ms": 5000,
            }),
        )
        .expect("settings profile minimal must run");
    assert_eq!(value["exit_code"], json!(0), "{value:?}");
    assert!(!docs_marker(&home).exists());
}

#[cfg(unix)]
#[test]
fn install_rooted_outside_the_workspace_is_not_refused() {
    let workspace = tempdir().expect("workspace");
    let workspace = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let outside = tempdir().expect("outside");
    let outside = outside.path().canonicalize().expect("canonical outside");
    let bin = workspace.join("bin");
    let rustup = install_fake_rustup(&bin);
    let home = outside.join("rustup");
    fs::create_dir_all(&home).expect("outside home");
    let value = registry()
        .execute(
            "proc.spawn",
            &rustup_context(&workspace, &rustup, rustup_env(&home, &bin)),
            json!({
                "program": rustup.display().to_string(),
                "args": ["toolchain", "install", "stable"],
                "timeout_ms": 5000,
            }),
        )
        .expect("install outside the workspace must run");
    assert_eq!(value["exit_code"], json!(0), "{value:?}");
    assert!(docs_marker(&home).is_file());
    assert!(!docs_marker(&workspace).exists());

    let link_home = workspace.join(".orbit/tmp/escaped");
    fs::create_dir_all(link_home.parent().expect("parent")).expect("tmp");
    std::os::unix::fs::symlink(&home, &link_home).expect("symlink home outside");
    fs::remove_file(docs_marker(&home)).expect("clear docs");
    let value = registry()
        .execute(
            "proc.spawn",
            &rustup_context(&workspace, &rustup, rustup_env(&link_home, &bin)),
            json!({
                "program": rustup.display().to_string(),
                "args": ["toolchain", "install", "stable"],
                "timeout_ms": 5000,
            }),
        )
        .expect("a symlink whose target is outside the workspace must run");
    assert_eq!(value["exit_code"], json!(0), "{value:?}");
    assert!(
        docs_marker(&home).is_file(),
        "the install follows the symlink and writes outside the workspace"
    );
    assert!(!docs_marker(&workspace).exists());
}

#[cfg(unix)]
#[test]
fn toolchain_file_minimal_profile_allows_proxy_auto_install() {
    let workspace = tempdir().expect("workspace");
    let workspace = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let bin = workspace.join("bin");
    let rustup = install_fake_rustup(&bin);
    let cargo = bin.join("cargo");
    std::os::unix::fs::symlink(&rustup, &cargo).expect("cargo proxy");
    fs::write(
        workspace.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.97.0\"\nprofile = \"minimal\"\ncomponents = [\"rust-src\"]\n",
    )
    .expect("toolchain file");
    let home = workspace.join(".orbit/tmp/file-profile");
    fs::create_dir_all(&home).expect("rustup home");
    let value = registry()
        .execute(
            "proc.spawn",
            &rustup_context(&workspace, &cargo, rustup_env(&home, &bin)),
            json!({
                "program": cargo.display().to_string(),
                "args": ["build"],
                "timeout_ms": 5000,
            }),
        )
        .expect("toolchain-file minimal profile must run");
    assert_eq!(value["exit_code"], json!(0), "{value:?}");
    assert!(!docs_marker(&home).exists());
    assert!(home.join("invoked-args").is_file());
}

#[cfg(unix)]
#[test]
fn valid_toml_table_comments_do_not_bypass_workspace_install_refusal() {
    for scenario in ["toolchain-file", "settings-overrides"] {
        let workspace = tempdir().expect("workspace");
        let workspace = workspace
            .path()
            .canonicalize()
            .expect("canonical workspace");
        let bin = workspace.join("bin");
        let rustup = install_fake_rustup(&bin);
        let cargo = bin.join("cargo");
        std::os::unix::fs::symlink(&rustup, &cargo).expect("cargo proxy");

        let home = workspace.join(format!(".orbit/tmp/toml-{scenario}"));
        fs::create_dir_all(&home).expect("rustup home");
        match scenario {
            "toolchain-file" => fs::write(
                workspace.join("rust-toolchain.toml"),
                "[ toolchain ] # valid TOML comment\nchannel = \"1.97.0\"\nprofile = \"default\"\n",
            )
            .expect("toolchain file"),
            "settings-overrides" => fs::write(
                home.join("settings.toml"),
                format!(
                    "profile = \"default\"\n[ overrides ] # valid TOML comment\n\"{}\" = \"1.97.0\"\n",
                    workspace.display()
                ),
            )
            .expect("settings"),
            _ => unreachable!("known scenario"),
        }

        let err = registry()
            .execute(
                "proc.spawn",
                &rustup_context(&workspace, &cargo, rustup_env(&home, &bin)),
                json!({
                    "program": cargo.display().to_string(),
                    "args": ["build"],
                    "timeout_ms": 5000,
                }),
            )
            .expect_err("valid rustup settings must not bypass the default-profile refusal");
        assert_install_refused(err);
        assert!(!docs_marker(&home).exists());
        assert!(!home.join("invoked-args").exists());
    }
}
