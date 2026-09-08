//! Production-shaped managed nested Orbit commands under the resolved macOS
//! child-runtime profile. [ORB-11055] [ORB-11066] [ORB-11070]

#[cfg(target_os = "macos")]
use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};

#[cfg(target_os = "macos")]
use orbit_engine::RuntimeHost;
#[cfg(target_os = "macos")]
use orbit_exec::{
    MacosSandboxSpawnRequest, compile_macos_sandbox_profile, sandbox_exec_available,
    sandbox_exec_path, spawn_under_macos_sandbox,
};
#[cfg(target_os = "macos")]
use serde_json::Value;

#[cfg(target_os = "macos")]
use orbit_engine::activity_job::load_activity_asset;
#[cfg(target_os = "macos")]
use orbit_types::workflow::ActivityV2Spec;

#[cfg(target_os = "macos")]
use crate::OrbitRuntime;
#[cfg(target_os = "macos")]
use crate::adapter::engine_host::v2_host::test_support::seed_executor;
#[cfg(target_os = "macos")]
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

/// A managed child launched from a disposable linked worktree carries the same
/// registry locator and provenance the CLI runner emits. Both a capability
/// preflight and a task-store lookup must reach tool dispatch without granting
/// the whole global Orbit root or bootstrapping workspace layout there.
#[cfg(target_os = "macos")]
#[test]
fn managed_nested_orbit_dispatches_from_linked_worktree_under_sandbox() {
    if !sandbox_exec_can_apply() {
        return;
    }
    let Some(orbit_bin) = locate_orbit_cli_binary() else {
        return;
    };

    let parent = sandbox_test_parent("nested-task-update");
    let _cleanup = ScopeGuard(parent.clone());
    let home = parent.join("home");
    let child_home = parent.join("provider-home");
    let repo = parent.join("repo");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&child_home).expect("create provider home");
    std::fs::create_dir_all(&repo).expect("create repo");
    init_git_repo(&repo);

    // `workspace init` freezes crew availability from PATH. Hosted macOS
    // runners do not provide an agent CLI, which otherwise seeds an explicit
    // empty `[crews]` registry while shipped auto-tasks still name the
    // `system` crew. A disposable executable keeps this fixture
    // deterministic without dispatching an agent.
    let provider_bin = home.join("stub-bin");
    plant_agent_cli_stub(&provider_bin, "codex");
    let provider_path = stub_first_path(&provider_bin);

    run_orbit(
        &orbit_bin,
        &repo,
        &home,
        &["workspace", "init", "--name", "orb-11055-nested-audit"],
        "workspace init",
        Some(&provider_path),
    );

    let minted = run_orbit(
        &orbit_bin,
        &repo,
        &home,
        &["auto-task", "mint", "qa-sweep", "--json"],
        "auto-task mint",
        None,
    );
    let minted: Value = serde_json::from_slice(&minted.stdout).expect("mint JSON");
    let minted_id = minted["id"].as_str().expect("task id").to_string();
    assert!(
        minted["required_tools"]
            .as_array()
            .is_some_and(|tools| tools.is_empty()),
        "shipped qa-sweep must mint without extra required_tools: {}",
        minted["required_tools"]
    );

    let github_capable = run_orbit(
        &orbit_bin,
        &repo,
        &home,
        &[
            "task",
            "add",
            "--title",
            "Nested GitHub-capable implementation",
            "--description",
            "Hand-authored neighbor carrying the five GitHub reads so nested dispatch stays task-scoped.",
            "--acceptance-criteria",
            "github.auth.status returns a structured capability result",
            "--complexity",
            "low",
            "--required-tool",
            "github.auth.status",
            "--required-tool",
            "github.pr.list",
            "--required-tool",
            "github.run.list",
            "--required-tool",
            "github.run.logs",
            "--required-tool",
            "github.run.view",
            "--json",
        ],
        "github-capable task add",
        None,
    );
    let github_capable: Value =
        serde_json::from_slice(&github_capable.stdout).expect("github-capable task JSON");
    let task_id = github_capable["id"]
        .as_str()
        .expect("github-capable task id")
        .to_string();
    assert_eq!(
        github_capable["required_tools"],
        serde_json::json!([
            "github.auth.status",
            "github.pr.list",
            "github.run.list",
            "github.run.logs",
            "github.run.view"
        ])
    );

    let ordinary = run_orbit(
        &orbit_bin,
        &repo,
        &home,
        &[
            "task",
            "add",
            "--title",
            "Ordinary nested implementation",
            "--description",
            "Empty required_tools neighbor proving GitHub reads stay task-scoped.",
            "--acceptance-criteria",
            "github.auth.status remains policy-denied",
            "--complexity",
            "low",
            "--json",
        ],
        "ordinary task add",
        None,
    );
    let ordinary: Value = serde_json::from_slice(&ordinary.stdout).expect("ordinary task JSON");
    let ordinary_id = ordinary["id"]
        .as_str()
        .expect("ordinary task id")
        .to_string();
    assert!(
        ordinary["required_tools"]
            .as_array()
            .is_some_and(|tools| tools.is_empty())
    );

    let worktree = repo.join(".orbit/state/worktrees/jrun-orb-11066");
    let worktree_output = Command::new("git")
        .current_dir(&repo)
        .args(["worktree", "add", "--detach"])
        .arg(&worktree)
        .output()
        .expect("create linked worktree");
    assert!(
        worktree_output.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&worktree_output.stderr)
    );

    let global = home.join(".orbit");
    let workspace_orbit = repo.join(".orbit");
    let global_str = global
        .canonicalize()
        .unwrap_or_else(|_| global.clone())
        .display()
        .to_string();
    let _ = std::fs::remove_dir_all(global.join("state/audit"));

    let runtime = OrbitRuntime::from_roots(&global, &workspace_orbit).expect("runtime");
    seed_executor(
        &runtime,
        "gemini",
        Some(orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec),
    );
    let resolved = runtime
        .resolve_executor_sandbox("gemini", None, Some(&repo))
        .expect("resolve")
        .expect("descriptor");
    let modify = &resolved.fs_profile.modify;
    let audit_root = format!("{global_str}/state/audit/**");
    let deny_orbit = format!("!{}/**", workspace_orbit.display());
    let deny_pos = modify
        .iter()
        .position(|entry| entry == &deny_orbit)
        .unwrap_or_else(|| panic!("workspace .orbit deny missing from {modify:?}"));
    let allow_pos = modify
        .iter()
        .position(|entry| entry == &audit_root)
        .unwrap_or_else(|| panic!("global audit grant missing from {modify:?}"));
    assert!(
        deny_pos < allow_pos,
        "global audit grant must follow workspace .orbit deny: {modify:?}"
    );
    assert!(
        !modify.iter().any(|entry| entry == &global_str),
        "must not blanket-reallow the global Orbit root: {modify:?}"
    );
    let home_str = home.to_string_lossy().into_owned();
    assert!(
        !modify.iter().any(|entry| entry == &home_str),
        "must not blanket-reallow the user's home: {modify:?}"
    );
    assert!(
        !modify
            .iter()
            .any(|entry| entry == &workspace_orbit.display().to_string()),
        "must not blanket-reallow the workspace .orbit tree: {modify:?}"
    );

    let _env = orbit_common::test_env::scoped([("HOME", Some(home_str.as_str()))]);
    let profile_text = compile_macos_sandbox_profile(&resolved.fs_profile, "gemini")
        .expect("compile nested orbit sandbox profile");
    let (_, implement_yaml) = DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "agent_implement")
        .expect("shipped agent_implement");
    let implement = load_activity_asset(implement_yaml).expect("parse agent_implement");
    let ActivityV2Spec::AgentLoop(implement_spec) = implement.spec.spec else {
        panic!("agent_implement must remain an agent_loop activity");
    };
    let minted_tools = RuntimeHost::resolve_activity_tools(
        &runtime,
        std::slice::from_ref(&task_id),
        &implement_spec.tools,
    )
    .expect("resolve GitHub-capable tools");
    assert_eq!(
        minted_tools.requested_tools,
        [
            "github.auth.status",
            "github.pr.list",
            "github.run.list",
            "github.run.logs",
            "github.run.view"
        ]
    );
    assert!(
        minted_tools
            .effective_tools
            .starts_with(implement_spec.tools.as_slice()),
        "effective tools must keep the production agent_implement baseline: {:?}",
        minted_tools.effective_tools
    );
    assert!(
        minted_tools
            .effective_tools
            .iter()
            .any(|tool| tool == "github.auth.status")
    );
    let minted_auto_tools = RuntimeHost::resolve_activity_tools(
        &runtime,
        std::slice::from_ref(&minted_id),
        &implement_spec.tools,
    )
    .expect("resolve minted qa-sweep tools");
    assert!(minted_auto_tools.requested_tools.is_empty());
    assert_eq!(minted_auto_tools.effective_tools, implement_spec.tools);
    let ordinary_tools = RuntimeHost::resolve_activity_tools(
        &runtime,
        std::slice::from_ref(&ordinary_id),
        &implement_spec.tools,
    )
    .expect("resolve ordinary tools");
    assert_eq!(ordinary_tools.effective_tools, implement_spec.tools);
    assert!(
        !ordinary_tools
            .effective_tools
            .iter()
            .any(|tool| tool.starts_with("github.")),
        "DANI-10056 missing-requirements behavior must remain denied for ordinary tasks"
    );

    let child_home_str = child_home.to_string_lossy().into_owned();
    let orbit_bin_str = orbit_bin.to_string_lossy().into_owned();
    let env = managed_nested_env(
        &child_home_str,
        &orbit_bin_str,
        &global_str,
        &task_id,
        &minted_tools.effective_tools,
        &resolved.fs_profile.name,
    );

    let output = run_sandboxed_orbit(
        &orbit_bin,
        &profile_text,
        &env,
        &worktree,
        &["tool", "run", "github.auth.status"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sandboxed github.auth.status failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("file-write-create") && !stderr.contains("Operation not permitted"),
        "nested Orbit must not hit a sandbox bootstrap denial: {stderr}"
    );
    let capability: Value = serde_json::from_slice(&output.stdout).expect("capability JSON");
    assert!(
        capability
            .get("available")
            .and_then(Value::as_bool)
            .is_some()
            && capability
                .get("authenticated")
                .and_then(Value::as_bool)
                .is_some()
            && capability
                .get("detail")
                .and_then(Value::as_str)
                .is_some_and(|detail| !detail.is_empty()),
        "github.auth.status must return its structured capability result: {capability}"
    );
    assert!(
        !stdout.contains("policy_denied") && !stderr.contains("policy_denied"),
        "computed GitHub-capable tools must not reproduce DANI-10056 policy_denied: {stderr}"
    );

    let ordinary_env = managed_nested_env(
        &child_home_str,
        &orbit_bin_str,
        &global_str,
        &ordinary_id,
        &ordinary_tools.effective_tools,
        &resolved.fs_profile.name,
    );
    let denied = run_sandboxed_orbit(
        &orbit_bin,
        &profile_text,
        &ordinary_env,
        &worktree,
        &["tool", "run", "github.auth.status"],
    );
    let denied_out = format!(
        "{}\n{}",
        String::from_utf8_lossy(&denied.stdout),
        String::from_utf8_lossy(&denied.stderr)
    );
    assert!(
        !denied.status.success(),
        "ordinary agent_implement baseline must deny GitHub reads\n{denied_out}"
    );
    assert!(
        denied_out.contains("policy_denied")
            || denied_out.contains("not in the activity allowlist"),
        "DANI-10056 missing-requirements denial must stay policy_denied: {denied_out}"
    );

    let input = serde_json::json!({ "id": task_id, "model": "grok" }).to_string();
    let shown = run_sandboxed_orbit(
        &orbit_bin,
        &profile_text,
        &env,
        &worktree,
        &["tool", "run", "orbit.task.show", "--input", &input],
    );
    assert!(
        shown.status.success(),
        "sandboxed orbit.task.show failed: {}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let task: Value = serde_json::from_slice(&shown.stdout).expect("task show JSON");
    assert_eq!(task["id"], task_id);
    for workspace_only in [
        "state/job-runs",
        "state/diagnostics",
        "state/scoreboard",
        "state/worktrees",
        "knowledge",
    ] {
        assert!(
            !global.join(workspace_only).exists(),
            "managed registry discovery must not create global workspace-only path {workspace_only}"
        );
    }
}

#[cfg(target_os = "macos")]
fn managed_nested_env(
    child_home: &str,
    orbit_bin: &str,
    registry_root: &str,
    task_id: &str,
    effective_tools: &[String],
    fs_profile: &str,
) -> Vec<(String, String)> {
    vec![
        ("HOME".to_string(), child_home.to_string()),
        ("USERPROFILE".to_string(), child_home.to_string()),
        (
            "PATH".to_string(),
            "/usr/bin:/bin:/usr/local/bin".to_string(),
        ),
        ("TMPDIR".to_string(), "/tmp".to_string()),
        ("ORBIT_BIN".to_string(), orbit_bin.to_string()),
        ("ORBIT_REGISTRY_ROOT".to_string(), registry_root.to_string()),
        ("ORBIT_MANAGED_RUN_CONTEXT".to_string(), "1".to_string()),
        ("ORBIT_RUN_ID".to_string(), "jrun-orb-11066".to_string()),
        ("ORBIT_TASK_ID".to_string(), task_id.to_string()),
        ("ORBIT_ACTIVE_TASK_ID".to_string(), task_id.to_string()),
        ("ORBIT_TASK_ACTOR_KIND".to_string(), "agent".to_string()),
        (
            "ORBIT_ACTIVITY_TOOLS".to_string(),
            effective_tools.join(","),
        ),
        (
            "ORBIT_ACTIVITY_FS_PROFILE".to_string(),
            fs_profile.to_string(),
        ),
        ("ORBIT_AGENT_NAME".to_string(), "grok".to_string()),
        ("ORBIT_AGENT_MODEL".to_string(), "grok".to_string()),
    ]
}

#[cfg(target_os = "macos")]
fn run_sandboxed_orbit(
    bin: &Path,
    profile_text: &str,
    env: &[(String, String)],
    cwd: &Path,
    args: &[&str],
) -> std::process::Output {
    let args = args
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    let (child, _profile) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text,
        program: bin.to_str().expect("orbit path utf8"),
        args: &args,
        env,
        cwd: Some(cwd),
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
    })
    .expect("spawn nested Orbit command");
    child.wait_with_output().expect("wait nested Orbit")
}

#[cfg(target_os = "macos")]
fn run_orbit(
    bin: &Path,
    cwd: &Path,
    home: &Path,
    args: &[&str],
    label: &str,
    path: Option<&OsStr>,
) -> std::process::Output {
    let mut command = Command::new(bin);
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_ROOT")
        .env_remove("ORBIT_REGISTRY_ROOT")
        .env_remove("ORBIT_MANAGED_RUN_CONTEXT")
        .env_remove("ORBIT_AGENT_NAME")
        .env_remove("ORBIT_AGENT_MODEL")
        .args(args);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let output = command
        .output()
        .unwrap_or_else(|err| panic!("{label}: spawn orbit: {err}"));
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[cfg(target_os = "macos")]
fn plant_agent_cli_stub(bin: &Path, name: &str) {
    std::fs::create_dir_all(bin).expect("create stub CLI directory");
    let stub = bin.join(name);
    std::fs::write(&stub, "#!/bin/sh\nexit 0\n").expect("write stub agent CLI");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("mark stub agent CLI executable");
    }
}

#[cfg(target_os = "macos")]
fn stub_first_path(bin: &Path) -> std::ffi::OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![bin.to_path_buf()];
    entries.extend(std::env::split_paths(&inherited));
    std::env::join_paths(entries).expect("join PATH entries")
}

#[cfg(target_os = "macos")]
fn locate_orbit_cli_binary() -> Option<PathBuf> {
    if let Some(path) = option_env!("CARGO_BIN_EXE_orbit") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    for profile in ["debug", "release"] {
        let candidate = target_root.join(profile).join("orbit");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    std::env::var_os("ORBIT_BIN")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

#[cfg(target_os = "macos")]
fn sandbox_exec_can_apply() -> bool {
    if !sandbox_exec_available() {
        return false;
    }
    let Some(path) = sandbox_exec_path() else {
        return false;
    };
    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-probe-")
        .suffix(".sb")
        .tempfile()
        .expect("probe profile");
    use std::io::Write;
    profile_file
        .write_all(b"(version 1)\n(allow default)\n")
        .expect("write probe profile");
    profile_file.flush().expect("flush probe profile");
    Command::new(path)
        .arg("-f")
        .arg(profile_file.path())
        .arg("/usr/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn sandbox_test_parent(label: &str) -> PathBuf {
    let roots = [
        Some(std::env::current_dir().expect("current dir")),
        std::env::var_os("HOME").map(PathBuf::from),
    ];
    let suffix = std::process::id().to_string();
    let mut attempts = Vec::new();
    for root in roots.into_iter().flatten() {
        if is_default_write_allow_root(&root) {
            attempts.push(format!(
                "{} is under a broad sandbox write allow",
                root.display()
            ));
            continue;
        }
        let parent = root.join(format!(".orbit-sandbox-test-{suffix}-{label}"));
        match std::fs::create_dir_all(&parent) {
            Ok(()) => return parent,
            Err(err) => attempts.push(format!("{}: {err}", parent.display())),
        }
    }
    panic!(
        "no writable macOS sandbox test parent outside broad write allows: {}",
        attempts.join("; ")
    );
}

#[cfg(target_os = "macos")]
fn is_default_write_allow_root(path: &Path) -> bool {
    let mut roots = vec![
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/private/var/folders"),
        PathBuf::from("/dev"),
    ];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        roots.push(home.join("Library/Caches"));
        roots.push(home.join(".orbit/state/logs"));
    }
    let matches = |candidate: &Path| roots.iter().any(|root| candidate.starts_with(root));
    if matches(path) {
        return true;
    }
    path.canonicalize()
        .map(|canonical| matches(&canonical))
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn init_git_repo(repo: &Path) {
    for args in [
        ["init"].as_slice(),
        ["config", "user.name", "Orbit Test"].as_slice(),
        ["config", "user.email", "orbit-test@example.com"].as_slice(),
        ["config", "commit.gpgsign", "false"].as_slice(),
    ] {
        let status = Command::new("git")
            .current_dir(repo)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed: {status:?}");
    }
    std::fs::write(repo.join("README.md"), "# fixture\n").expect("write readme");
    let add = Command::new("git")
        .current_dir(repo)
        .args(["add", "README.md"])
        .status()
        .expect("git add");
    assert!(add.success(), "git add failed");
    let commit = Command::new("git")
        .current_dir(repo)
        .args(["commit", "-m", "fixture"])
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed");
}

#[cfg(target_os = "macos")]
struct ScopeGuard(PathBuf);

#[cfg(target_os = "macos")]
impl Drop for ScopeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
