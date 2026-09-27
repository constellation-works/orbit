use super::super::spawn::{
    MAX_CACHED_PROFILES, MacosSandboxSpawnRequest, TEST_PROFILE_CACHE_LOCK,
    cached_profile_tempfile, profile_cache_len, sandbox_exec_path_from, spawn_under_macos_sandbox,
};
#[cfg(target_os = "macos")]
use super::super::test_support::{
    EnvOverrides, NEUTRAL_PROVIDER, ScopeGuard, compile_with_env, profile, sandbox_exec_can_apply,
    sandbox_test_parent, shell_escape,
};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, PoisonError};

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
        inherited_fds: &[],
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
        inherited_fds: &[],
    })
    .expect("spawn sandboxed pty probe");
    let output = child.wait_with_output().expect("wait for pty probe");

    assert!(
        output.status.success(),
        "openpty probe failed under the compiled sandbox profile; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// [DANI-10409] Identical compiled profile text must reuse one process-wide
/// tempfile across spawns (constant `(fs_profile, provider, env)` retried
/// within a run), instead of creating, writing, and unlinking a fresh one
/// every time.
#[cfg(target_os = "macos")]
#[test]
fn spawn_under_macos_sandbox_reuses_tempfile_for_identical_profile_text() {
    if !sandbox_exec_can_apply() {
        return;
    }
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    // Unique clause so this profile text never collides with another test's
    // literal `(allow default)` profile in the shared process-wide cache.
    let profile_text =
        "(version 1)\n(allow default)\n(allow file-read* (literal \"/dani-10409-cache-marker\"))\n";
    let args = ["-c".to_string(), "exit 0".to_string()];

    let spawn_once = || {
        let (child, profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
            profile_text,
            program: "/bin/sh",
            args: &args,
            env: &[],
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
            "sandboxed child should succeed; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        profile_file
    };

    let first = spawn_once();
    let second = spawn_once();

    assert!(
        Arc::ptr_eq(&first, &second),
        "identical profile text should reuse the same process-wide tempfile"
    );
    assert_eq!(first.path(), second.path());
}

/// [ORB-13454] Inactive profile tempfile retention must stay bounded to
/// [`MAX_CACHED_PROFILES`] and evicted inactive tempfiles must be reclaimed
/// from disk when more distinct profiles are requested than the cache bound.
#[test]
fn cached_profile_tempfiles_stay_bounded_and_evict_inactive_entries() {
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    let p0 = cached_profile_tempfile("(version 1)\n(allow default)\n; orb-13454-bound-0\n")
        .expect("cache profile 0");
    let path0 = p0.path().to_path_buf();
    assert!(
        path0.exists(),
        "initial profile tempfile should exist on disk"
    );

    // Drop the strong Arc handle; the file is now inactive and retained only by the cache.
    drop(p0);
    assert!(
        path0.exists(),
        "inactive profile must remain in cache until evicted"
    );

    // Request MAX_CACHED_PROFILES distinct profiles, each dropped immediately so they are inactive.
    for i in 1..=MAX_CACHED_PROFILES {
        let profile_text = format!("(version 1)\n(allow default)\n; orb-13454-bound-{i}\n");
        let p = cached_profile_tempfile(&profile_text).expect("cache profile");
        assert!(p.path().exists());
    }

    // Since MAX_CACHED_PROFILES + 1 distinct profiles were requested, p0 must have been evicted.
    // Because no active caller retains p0, the tempfile descriptor and disk file must be reclaimed.
    assert!(
        !path0.exists(),
        "evicted inactive profile tempfile must be unlinked and reclaimed from disk"
    );
    assert!(
        profile_cache_len() <= MAX_CACHED_PROFILES,
        "profile cache size must stay bounded to MAX_CACHED_PROFILES (len={})",
        profile_cache_len()
    );
}

/// [ORB-13454] Active consumers holding [`Arc<NamedTempFile>`] retain their
/// profile files on disk even after eviction from the cache, and the file is
/// reclaimed as soon as the active handle drops.
#[test]
fn profiles_referenced_by_active_callers_remain_available_until_dropped() {
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    let active_profile =
        cached_profile_tempfile("(version 1)\n(allow default)\n; orb-13454-active-retained\n")
            .expect("cache active profile");
    let active_path = active_profile.path().to_path_buf();
    assert!(
        active_path.exists(),
        "active profile tempfile should exist on disk"
    );

    // Evict active_profile from the cache by inserting MAX_CACHED_PROFILES distinct profiles.
    for i in 1..=MAX_CACHED_PROFILES {
        let profile_text = format!("(version 1)\n(allow default)\n; orb-13454-active-evict-{i}\n");
        let _ = cached_profile_tempfile(&profile_text).expect("cache profile");
    }

    // Even though active_profile was evicted from the process-wide cache, the caller's Arc
    // reference must keep the file alive and readable for the child's lifetime.
    assert!(
        active_path.exists(),
        "profile still referenced by active caller must remain available on disk"
    );
    let content = std::fs::read_to_string(&active_path).expect("read active profile file");
    assert!(
        content.contains("; orb-13454-active-retained"),
        "active profile content must be intact"
    );

    // Once the active caller drops its handle, the temporary file must be reclaimed.
    drop(active_profile);
    assert!(
        !active_path.exists(),
        "profile tempfile must be reclaimed once active handle is dropped"
    );
}

/// [ORB-13454] Identical profiles must reuse the cached tempfile and refresh
/// LRU recency so frequently reused profiles avoid premature eviction.
#[test]
fn identical_profiles_reuse_tempfile_and_refresh_lru_recency() {
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    let profile_a = "(version 1)\n(allow default)\n; orb-13454-reuse-a\n";
    let profile_b = "(version 1)\n(allow default)\n; orb-13454-reuse-b\n";

    let a1 = cached_profile_tempfile(profile_a).expect("cache a1");
    let a2 = cached_profile_tempfile(profile_a).expect("cache a2");
    assert!(
        Arc::ptr_eq(&a1, &a2),
        "repeated identical profile requests must reuse the same Arc<NamedTempFile>"
    );
    assert_eq!(a1.path(), a2.path());
    let path_a = a1.path().to_path_buf();

    let b1 = cached_profile_tempfile(profile_b).expect("cache b1");
    let path_b = b1.path().to_path_buf();

    // Drop all local handles so entries become inactive in the cache.
    drop(a1);
    drop(a2);
    drop(b1);

    // Re-access profile A to refresh its LRU recency (marking it newer than B).
    let a3 = cached_profile_tempfile(profile_a).expect("cache a3");
    drop(a3);

    // Fill cache with MAX_CACHED_PROFILES - 1 new distinct entries.
    // Because B was older than A in LRU order, B will be evicted while A is retained.
    for i in 1..MAX_CACHED_PROFILES {
        let profile_text = format!("(version 1)\n(allow default)\n; orb-13454-reuse-churn-{i}\n");
        let _ = cached_profile_tempfile(&profile_text).expect("cache profile");
    }

    assert!(
        !path_b.exists(),
        "older inactive profile B must have been evicted and reclaimed"
    );
    assert!(
        path_a.exists(),
        "refreshed profile A must still be cached on disk"
    );
}

/// [ORB-13454] Repeated failed spawns cannot grow inactive profile retention
/// without bound.
#[test]
fn repeated_failed_spawns_cannot_grow_inactive_profile_retention_without_bound() {
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    // First failed spawn creates and caches a profile tempfile before failing.
    let first_profile = "(version 1)\n(allow default)\n; orb-13454-failed-spawn-0\n";
    let first_res = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: first_profile,
        program: "/nonexistent/binary/orbit-test-fail-spawn",
        args: &[],
        env: &[],
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
    });
    assert!(
        first_res.is_err(),
        "spawn with nonexistent binary should fail"
    );

    // The profile tempfile was cached prior to the spawn failure.
    let first_cached = cached_profile_tempfile(first_profile).expect("get cached first profile");
    let first_path = first_cached.path().to_path_buf();
    drop(first_cached);
    assert!(
        first_path.exists(),
        "failed spawn profile tempfile was cached and exists on disk"
    );

    // Perform more failed spawns than the cache capacity.
    for i in 1..=(MAX_CACHED_PROFILES + 5) {
        let profile_text = format!("(version 1)\n(allow default)\n; orb-13454-failed-spawn-{i}\n");
        let res = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
            profile_text: &profile_text,
            program: "/nonexistent/binary/orbit-test-fail-spawn",
            args: &[],
            env: &[],
            cwd: None,
            stdin: Stdio::null(),
            stdout: Stdio::piped(),
            stderr: Stdio::piped(),
            inherited_fds: &[],
        });
        assert!(res.is_err(), "spawn with nonexistent binary should fail");
    }

    // The first failed spawn profile must have been evicted and reclaimed.
    assert!(
        !first_path.exists(),
        "first failed spawn profile tempfile must have been evicted and reclaimed"
    );
    assert!(
        profile_cache_len() <= MAX_CACHED_PROFILES,
        "profile cache size must remain bounded after repeated failed spawns (len={})",
        profile_cache_len()
    );
}

/// [ORB-13454] Spawning active children under macOS sandbox retains their profile
/// tempfiles while running even after eviction, and reclaims them once the active
/// child drops its profile handle.
#[cfg(target_os = "macos")]
#[test]
fn spawn_under_macos_sandbox_evicts_inactive_profiles_while_active_child_runs() {
    if !sandbox_exec_can_apply() {
        return;
    }
    let _lock = TEST_PROFILE_CACHE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    let profile_text = "(version 1)\n(allow default)\n(allow file-read* (literal \"/orb-13454-active-child-marker\"))\n";
    let args = ["-c".to_string(), "sleep 0.5".to_string()];
    let (child, profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text,
        program: "/bin/sh",
        args: &args,
        env: &[],
        cwd: None,
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds: &[],
    })
    .expect("spawn sandboxed child");
    let child_profile_path = profile_file.path().to_path_buf();
    assert!(
        child_profile_path.exists(),
        "active child profile file should exist on disk"
    );

    // Evict child's profile from the cache by requesting MAX_CACHED_PROFILES + 5 distinct profiles.
    for i in 0..(MAX_CACHED_PROFILES + 5) {
        let p_text = format!("(version 1)\n(allow default)\n; orb-13454-active-child-evict-{i}\n");
        let _ = cached_profile_tempfile(&p_text).expect("cache profile");
    }

    // Profile path must still exist while child is running and profile_file is retained!
    assert!(
        child_profile_path.exists(),
        "profile file must remain available while active child is running"
    );

    let output = child.wait_with_output().expect("wait for child");
    assert!(
        output.status.success(),
        "child should complete successfully"
    );

    // Drop the active profile file handle.
    drop(profile_file);
    assert!(
        !child_profile_path.exists(),
        "child profile file must be reclaimed after handle is dropped"
    );
}
