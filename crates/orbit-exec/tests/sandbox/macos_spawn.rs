#[cfg(target_os = "macos")]
use super::macos_audit_support::{
    EnvOverrides, NEUTRAL_PROVIDER, compile_with_env, profile, shell_escape,
};
#[cfg(target_os = "macos")]
use orbit_exec::{MacosSandboxSpawnRequest, spawn_under_macos_sandbox};
#[cfg(target_os = "macos")]
use std::process::Stdio;

#[cfg(target_os = "macos")]
#[test]
fn spawn_under_macos_sandbox_ignores_fake_sandbox_exec_on_path() {
    if !orbit_exec::macos_sandbox_test_guard(
        "spawn_under_macos_sandbox_ignores_fake_sandbox_exec_on_path",
    ) {
        return;
    }
    if super::audit_support::run_isolated(
        "macos_spawn::spawn_under_macos_sandbox_ignores_fake_sandbox_exec_on_path",
    ) {
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
    let child = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: "(version 1)\n(allow default)\n",
        program: "/bin/sh",
        args: &args,
        env: &env,
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
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
    if !orbit_exec::macos_sandbox_test_guard(
        "spawn_under_macos_sandbox_gives_the_child_only_the_supplied_environment",
    ) {
        return;
    }
    if super::audit_support::run_isolated(
        "macos_spawn::spawn_under_macos_sandbox_gives_the_child_only_the_supplied_environment",
    ) {
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
    let child = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: "(version 1)\n(allow default)\n",
        program: "/bin/sh",
        args: &args,
        env: &env,
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
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

/// [ORB-12470] Regression probe for the missing `(allow pseudo-tty)` clause.
/// Runs under the real compiled profile (not a permissive `(allow default)`
/// test profile) so the assertion exercises exactly what a worker's own PTY
/// smoke test would hit. Never run directly by `cargo test`; spawned by
/// [`pty_allocation_is_allowed_under_compiled_profile`] as a sandboxed child
/// via its `--exact` module path, so renaming this test without updating that
/// caller fails the parent's child-test execution guard.
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
    if !orbit_exec::macos_sandbox_test_guard("pty_allocation_is_allowed_under_compiled_profile") {
        return;
    }
    if super::audit_support::run_isolated(
        "macos_spawn::pty_allocation_is_allowed_under_compiled_profile",
    ) {
        return;
    }

    let resolved = profile("pty-probe", &[], &[]);
    let profile_text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    let current_exe = std::env::current_exe().expect("current test binary path");
    let args = [
        "--exact".to_string(),
        "macos_spawn::pty_probe_process".to_string(),
        "--ignored".to_string(),
    ];
    let env = [("PATH".to_string(), "/usr/bin:/bin".to_string())];
    let child = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: &profile_text,
        program: current_exe.to_str().expect("utf8 test binary path"),
        args: &args,
        env: &env,
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
    })
    .expect("spawn sandboxed pty probe");
    let output = child.wait_with_output().expect("wait for pty probe");

    orbit_common::test_env::assert_child_test_passed(
        "macos_spawn::pty_probe_process",
        output.status,
        &output.stdout,
        &output.stderr,
    );
}
