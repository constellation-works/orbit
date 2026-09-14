use super::super::spawn::sandbox_exec_path_from;
#[cfg(target_os = "macos")]
use super::super::spawn::{MacosSandboxSpawnRequest, spawn_under_macos_sandbox};
#[cfg(target_os = "macos")]
use super::super::test_support::{
    EnvOverrides, NEUTRAL_PROVIDER, ScopeGuard, compile_with_env, profile, sandbox_exec_can_apply,
    sandbox_test_parent, shell_escape,
};
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::Stdio;

#[test]
fn sandbox_exec_path_from_uses_trusted_absolute_candidate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("sandbox-exec");
    std::fs::write(&bin, "#!/bin/sh\nexit 0\n").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("perms");
    }
    assert_eq!(sandbox_exec_path_from([bin.as_path()]), Some(bin));
}

#[test]
fn sandbox_exec_path_from_rejects_relative_candidates() {
    let bin = Path::new("sandbox-exec");
    assert_eq!(sandbox_exec_path_from([bin]), None);
}

#[cfg(target_os = "macos")]
#[test]
fn spawn_under_macos_sandbox_ignores_fake_sandbox_exec_on_path() {
    if !sandbox_exec_can_apply() {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let fake_dir = temp.path().join("fake-bin");
    std::fs::create_dir_all(&fake_dir).expect("fake dir");
    let marker = temp.path().join("fake-used");
    let fake = fake_dir.join("sandbox-exec");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho fake > {}\nexit 77\n",
            shell_escape(&marker)
        ),
    )
    .expect("write fake sandbox-exec");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
            .expect("fake perms");
    }

    let poisoned_path = format!("{}:/usr/bin:/bin", fake_dir.display());
    let args = ["-c".to_string(), "exit 0".to_string()];
    let env = [("PATH".to_string(), poisoned_path)];
    let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: "(version 1)\n(allow default)\n",
        program: "/bin/sh",
        args: &args,
        env: &env,
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
    })
    .expect("spawn sandboxed child");
    let output = child.wait_with_output().expect("wait for child");

    assert!(
        output.status.success(),
        "trusted sandbox-exec should run child despite fake PATH entry; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !marker.exists(),
        "fake sandbox-exec on PATH should not have been executed"
    );
}

/// [ORB-10917] `sandbox-exec` hands its own environment to the confined
/// program, so the launcher must apply exactly the environment the dispatcher
/// composed. The ambient variables are set here rather than read from the
/// developer's shell, and none carries a credential-shaped name — a denylist
/// would forward every one of them.
#[cfg(target_os = "macos")]
#[test]
fn spawn_under_macos_sandbox_gives_the_child_only_the_supplied_environment() {
    if !sandbox_exec_can_apply() {
        return;
    }

    let _ambient = orbit_common::test_env::scoped([
        ("DATABASE_URL", Some("postgres://svc:hunter2@db.internal")),
        ("BILLING_ENDPOINT", Some("https://billing.internal.example")),
        ("ORB_10917_AMBIENT", Some("leaked")),
        ("ANTHROPIC_API_KEY", Some("sk-ant-000000000000000000000")),
    ]);
    let args = ["-c".to_string(), "env".to_string()];
    let env = [
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("ORB_10917_SUPPLIED".to_string(), "present".to_string()),
    ];
    let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: "(version 1)\n(allow default)\n",
        program: "/bin/sh",
        args: &args,
        env: &env,
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
    })
    .expect("spawn sandboxed child");
    let output = child.wait_with_output().expect("wait for child");
    let child_env = String::from_utf8_lossy(&output.stdout);

    assert!(
        child_env.contains("ORB_10917_SUPPLIED=present"),
        "supplied vars must reach the confined child: {child_env}"
    );
    for leaked in [
        "DATABASE_URL",
        "BILLING_ENDPOINT",
        "ORB_10917_AMBIENT",
        "ANTHROPIC_API_KEY",
    ] {
        assert!(
            !child_env.contains(leaked),
            "{leaked} must not reach a sandbox-exec provider child: {child_env}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn spawn_under_macos_sandbox_runs_program_in_provided_cwd() {
    if !sandbox_exec_can_apply() {
        return;
    }

    let parent = sandbox_test_parent("cwd");
    let _cleanup = ScopeGuard(parent.clone());
    let dir = tempfile::Builder::new()
        .prefix("sandbox-cwd-")
        .tempdir_in(&parent)
        .expect("cwd tempdir");
    let cwd = dir.path().canonicalize().expect("canonical cwd");
    let args = ["-c".to_string(), "pwd".to_string()];
    let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: "(version 1)\n(allow default)\n",
        program: "/bin/sh",
        args: &args,
        env: &[],
        cwd: Some(&cwd),
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
    })
    .expect("spawn sandboxed child");
    let output = child.wait_with_output().expect("wait for child");

    assert!(
        output.status.success(),
        "sandboxed pwd should succeed; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout utf8"),
        format!("{}\n", cwd.display())
    );
}

/// [ORB-12470] Regression probe for the missing `(allow pseudo-tty)` clause.
/// Runs under the real compiled profile (not a permissive `(allow default)`
/// test profile) so the assertion exercises exactly what a worker's own PTY
/// smoke test would hit. Never run directly by `cargo test`; spawned by
/// [`pty_allocation_is_allowed_under_compiled_profile`] as a sandboxed child
/// via its `--exact` module path, so renaming this test without updating that
/// caller silently breaks the probe.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "spawned under sandbox-exec by pty_allocation_is_allowed_under_compiled_profile"]
fn pty_probe_process() {
    let mut controller: libc::c_int = -1;
    let mut follower: libc::c_int = -1;
    let rc = unsafe {
        libc::openpty(
            &mut controller,
            &mut follower,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(
        rc,
        0,
        "openpty failed under the compiled sandbox profile: {}",
        std::io::Error::last_os_error()
    );
    unsafe {
        libc::close(controller);
        libc::close(follower);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn pty_allocation_is_allowed_under_compiled_profile() {
    if !sandbox_exec_can_apply() {
        return;
    }

    let resolved = profile("pty-probe", &[], &[]);
    let profile_text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    let current_exe = std::env::current_exe().expect("current test binary path");
    let args = [
        "--exact".to_string(),
        "macos_sandbox::tests::spawn::pty_probe_process".to_string(),
        "--ignored".to_string(),
    ];
    let env = [("PATH".to_string(), "/usr/bin:/bin".to_string())];
    let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: &profile_text,
        program: current_exe.to_str().expect("utf8 test binary path"),
        args: &args,
        env: &env,
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
    })
    .expect("spawn sandboxed pty probe");
    let output = child.wait_with_output().expect("wait for pty probe");

    assert!(
        output.status.success(),
        "openpty probe failed under the compiled sandbox profile; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}
