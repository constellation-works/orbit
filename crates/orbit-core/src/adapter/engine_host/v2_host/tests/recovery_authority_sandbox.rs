//! Live Bubblewrap checks that a managed leaf cannot reach the recovery
//! authority through any path shape it can actually construct.
//!
//! These run a real sandbox rather than inspecting rules, because the four
//! vectors below are enforced by mount semantics, not by glob matching: a
//! read-only bind (`EROFS`), a mountpoint that cannot be renamed aside
//! (`EBUSY`), and symlink resolution landing back on the same mount.
//!
//! Bubblewrap needs unprivileged user namespaces, which some hosts disable.
//! When the probe reports the capability missing, the live test says so and
//! returns instead of passing, so a skip is never mistaken for enforcement.
//! [`the_compiled_sandbox_mounts_the_authority_read_only`] covers the same
//! mounts at the plan level and needs no such capability.
#![allow(clippy::print_stdout)]

use std::fs;
use std::os::unix::fs::symlink;
use std::process::Stdio;

use orbit_engine::RuntimeHost;
use orbit_exec::{
    LINUX_STABLE_WORKSPACE_MOUNT, LinuxBwrapSpawnRequest, compile_linux_bwrap_argv,
    prepare_linux_bwrap_write_grants, probe_bwrap, spawn_under_linux_bwrap,
};
use serde_json::json;

use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, seed_executor,
};
use crate::runtime::recovery_authority::RecoveryAuthority;

/// Each probe writes one `<name>=<outcome>` line so a failure names the vector
/// that got through instead of only the exit status.
const PROBE_SCRIPT: &str = r#"
probe() {
  if : > "$2" 2>/dev/null; then echo "$1=WRITABLE"; else echo "$1=denied"; fi
}
rename() {
  if mv "$2" "$2.moved" 2>/dev/null; then echo "$1=REPLACED"; else echo "$1=denied"; fi
}
plant() {
  if ln -s /tmp "$2" 2>/dev/null; then echo "$1=PLANTED"; else echo "$1=denied"; fi
}
probe original "$AUTHORITY/authority.db"
probe wal_sidecar "$AUTHORITY/authority.db-wal"
probe shm_sidecar "$AUTHORITY/authority.db-shm"
probe stable_alias "$ALIAS/authority.db"
rename authority_root "$AUTHORITY"
rename authority_parent "$GLOBAL/state"
probe authority_parent_entry "$GLOBAL/state/planted-file"
plant authority_root_redirect "$GLOBAL/state/planted-link"
plant global_child_redirect "$GLOBAL/planted-link"
probe leaf_task "$GLOBAL/tasks/leaf-write.json"
probe leaf_audit "$GLOBAL/state/audit/leaf-write.jsonl"
"#;

#[test]
fn a_leaf_cannot_reach_the_recovery_authority_through_any_path_it_can_build() {
    let probe = probe_bwrap();
    if !probe.available {
        println!(
            "skipping live recovery authority sandbox test: {}",
            probe.detail
        );
        return;
    }

    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    let worktree = repo_root.join(".orbit/state/worktrees/orbit-jrun-authority");
    fs::create_dir_all(&worktree).expect("managed worktree");

    // Hold a certified record open so the database and both SQLite sidecars
    // exist on disk while the leaf probes them.
    let global = runtime
        .paths()
        .global_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().global_dir.clone());
    let authority = RecoveryAuthority::open(&global).expect("open authority");
    authority
        .issue(
            "jrun-authority-1",
            "sync_base",
            &json!({
                "run_id": "jrun-authority-1",
                "step_id": "sync_base",
                "workspace_path": worktree,
                "head_sha": "aaaa",
                "base_sha": "bbbb",
            }),
        )
        .expect("issue certificate");
    let authority_root = global.join("state/recovery-authority");

    // The alias a leaf can actually build: a symlink inside its own writable
    // worktree, reached through the stable workspace mount rather than the
    // real path.
    symlink(&authority_root, worktree.join("authority-alias")).expect("alias symlink");

    let resolved = runtime
        .resolve_executor_sandbox("claude", None, Some(&worktree))
        .expect("resolve Linux sandbox")
        .expect("descriptor");
    assert!(
        resolved.managed_worktree,
        "the stable alias mount only exists for a managed worktree",
    );
    prepare_linux_bwrap_write_grants(&resolved.fs_profile, &worktree).expect("prepare grants");
    let plan = compile_linux_bwrap_argv(
        &resolved.fs_profile,
        "/bin/sh",
        &["-c".to_string(), PROBE_SCRIPT.to_string()],
        Some(&worktree),
        resolved.managed_worktree,
    )
    .expect("compile bwrap argv");

    let env = [
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        (
            "AUTHORITY".to_string(),
            authority_root.display().to_string(),
        ),
        ("GLOBAL".to_string(), global.display().to_string()),
        (
            "ALIAS".to_string(),
            format!("{LINUX_STABLE_WORKSPACE_MOUNT}/authority-alias"),
        ),
    ];
    let child = spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
        plan: &plan,
        env: &env,
        cwd: Some(&worktree),
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
    })
    .expect("spawn probe");
    let output = child.wait_with_output().expect("probe output");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "probe script failed: {stdout}\n{stderr}"
    );

    for vector in [
        "original",
        "wal_sidecar",
        "shm_sidecar",
        "stable_alias",
        "authority_root",
        "authority_parent",
        // Redirection preconditions: the module refuses a symlinked component
        // below the trusted root, and the sandbox denies a leaf the write that
        // would let it plant one in the first place.
        "authority_parent_entry",
        "authority_root_redirect",
        "global_child_redirect",
    ] {
        assert!(
            stdout.contains(&format!("{vector}=denied")),
            "leaf reached the recovery authority through `{vector}`: {stdout}"
        );
    }
    for admitted in ["leaf_task", "leaf_audit"] {
        assert!(
            stdout.contains(&format!("{admitted}=WRITABLE")),
            "admitted leaf write `{admitted}` regressed: {stdout}\n{stderr}"
        );
    }

    // Denial has to mean the host record is intact, and admission has to mean
    // the bytes actually landed outside the sandbox rather than on its tmpfs.
    assert!(authority_root.join("authority.db").is_file());
    assert!(!authority_root.with_extension("moved").exists());
    for planted in [
        global.join("state/planted-file"),
        global.join("state/planted-link"),
        global.join("planted-link"),
    ] {
        assert!(
            planted.symlink_metadata().is_err(),
            "leaf planted `{}` on the way to the authority",
            planted.display(),
        );
    }
    assert!(global.join("tasks/leaf-write.json").is_file());
    assert!(global.join("state/audit/leaf-write.jsonl").is_file());
}

/// Plan-level counterpart to the live probe, for hosts that cannot create a
/// user namespace. It asserts the mounts the kernel would enforce: the
/// authority root is bound read-only — which is what makes its contents
/// unwritable and the directory itself unrenameable — and no read-write bind
/// covers it, while the admitted leaf write roots are still bound read-write.
#[test]
fn the_compiled_sandbox_mounts_the_authority_read_only() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_executor(
        &runtime,
        "claude",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    let worktree = repo_root.join(".orbit/state/worktrees/orbit-jrun-authority");
    fs::create_dir_all(&worktree).expect("managed worktree");

    let global = runtime
        .paths()
        .global_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().global_dir.clone());
    let authority_root = global.join("state/recovery-authority");

    let resolved = runtime
        .resolve_executor_sandbox("claude", None, Some(&worktree))
        .expect("resolve Linux sandbox")
        .expect("descriptor");
    prepare_linux_bwrap_write_grants(&resolved.fs_profile, &worktree).expect("prepare grants");
    let plan = compile_linux_bwrap_argv(
        &resolved.fs_profile,
        "/bin/true",
        &[],
        Some(&worktree),
        resolved.managed_worktree,
    )
    .expect("compile bwrap argv");

    let mounts = plan
        .args
        .windows(3)
        .filter(|args| matches!(args[0].as_str(), "--bind" | "--ro-bind"))
        .map(|args| (args[0].clone(), args[1].clone(), args[2].clone()))
        .collect::<Vec<_>>();
    let authority = authority_root.display().to_string();

    assert!(
        mounts
            .iter()
            .any(|(kind, source, target)| kind == "--ro-bind"
                && source == &authority
                && target == &authority),
        "the authority root must be a read-only mountpoint: {mounts:?}"
    );
    assert!(
        !mounts.iter().any(|(kind, _, target)| kind == "--bind"
            && (authority.starts_with(target.as_str()) || target.starts_with(&authority))
            && target != "/"),
        "no read-write bind may cover the authority root: {mounts:?}"
    );
    for admitted in ["tasks", "state/audit"] {
        let path = global.join(admitted).display().to_string();
        assert!(
            mounts.iter().any(|(kind, source, target)| kind == "--bind"
                && source == &path
                && target == &path),
            "admitted leaf write root `{admitted}` must stay read-write: {mounts:?}"
        );
    }
    assert!(
        !plan
            .dropped_grants
            .iter()
            .any(|grant| grant.anchor.starts_with(&authority_root)),
        "the authority is denied outright, never a dropped grant: {:?}",
        plan.dropped_grants
    );
}
