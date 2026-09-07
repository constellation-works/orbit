#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::print_stdout, clippy::unwrap_used)]
#![cfg(target_os = "linux")]

use std::fs;
use std::path::PathBuf;

use orbit_exec::{
    EnvironmentMode, ExecRequest, StdinMode, linux_landlock_read_grants, probe_landlock,
    spawn_under_linux_landlock,
};
use orbit_types::policy::ResolvedFsProfile;

fn unrestricted_with_denies(denies: &[&str]) -> ResolvedFsProfile {
    let mut read = vec!["./**".to_string()];
    read.extend(denies.iter().map(|rule| format!("!{rule}")));
    ResolvedFsProfile {
        name: "test".to_string(),
        read,
        modify: vec!["./**".to_string()],
    }
}

fn cat_request(workspace: PathBuf, path: &str) -> ExecRequest {
    ExecRequest {
        program: "/bin/cat".to_string(),
        args: vec![path.to_string()],
        current_dir: Some(workspace.display().to_string()),
        timeout_ms: Some(5_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(vec![(
            "PATH".to_string(),
            "/usr/bin:/bin".to_string(),
        )]),
        debug: false,
    }
}

fn skip_without_landlock() -> bool {
    let probe = probe_landlock();
    if probe.available {
        return false;
    }
    println!("skipping real Landlock test: {}", probe.detail);
    true
}

#[test]
fn sandboxed_child_cannot_read_a_host_sentinel() {
    if skip_without_landlock() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let host = tempfile::tempdir().expect("host");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let visible = workspace_root.join("visible.txt");
    let sentinel = host
        .path()
        .canonicalize()
        .expect("canonical host")
        .join("sentinel.txt");
    fs::write(&visible, "workspace-ok").expect("write visible");
    fs::write(&sentinel, "HOST_SENTINEL_ORB11514").expect("write sentinel");
    let profile = unrestricted_with_denies(&[]);

    let allowed = spawn_under_linux_landlock(
        &cat_request(workspace_root.clone(), &visible.display().to_string()),
        &workspace_root,
        &profile,
    )
    .expect("spawn cat of workspace file")
    .wait_with_output()
    .expect("wait workspace cat");
    assert!(
        allowed.status.success(),
        "workspace cat should succeed: status={:?} stderr={}",
        allowed.status,
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&allowed.stdout), "workspace-ok");

    let denied = spawn_under_linux_landlock(
        &cat_request(workspace_root.clone(), &sentinel.display().to_string()),
        &workspace_root,
        &profile,
    )
    .expect("spawn cat of host sentinel")
    .wait_with_output()
    .expect("wait sentinel cat");
    let stdout = String::from_utf8_lossy(&denied.stdout);
    assert!(
        !denied.status.success(),
        "host sentinel cat must not succeed: stdout={stdout:?} stderr={}",
        String::from_utf8_lossy(&denied.stderr)
    );
    assert!(
        !stdout.contains("HOST_SENTINEL_ORB11514"),
        "sentinel must not appear in stdout: {stdout:?}"
    );
}

#[test]
fn sandboxed_child_cannot_read_a_deny_read_file() {
    if skip_without_landlock() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let allowed = workspace_root.join("allowed.txt");
    let secret = workspace_root.join(".env");
    fs::write(&allowed, "visible").expect("write allowed");
    fs::write(&secret, "DENY_READ_SECRET").expect("write secret");
    let profile = unrestricted_with_denies(&["**/.env"]);

    let grants = linux_landlock_read_grants(&workspace_root, &profile).expect("grants");
    assert!(
        grants
            .iter()
            .any(|grant| grant.access.read_file && allowed.starts_with(&grant.path)),
        "allowed file should have a READ_FILE ancestor: {grants:?}"
    );
    assert!(
        grants
            .iter()
            .all(|grant| { !(grant.access.read_file && secret.starts_with(&grant.path)) }),
        ".env must not have a READ_FILE ancestor: {grants:?}"
    );

    let allowed_out = spawn_under_linux_landlock(
        &cat_request(workspace_root.clone(), &allowed.display().to_string()),
        &workspace_root,
        &profile,
    )
    .expect("spawn allowed cat")
    .wait_with_output()
    .expect("wait allowed cat");
    assert!(allowed_out.status.success());
    assert_eq!(String::from_utf8_lossy(&allowed_out.stdout), "visible");

    let denied_out = spawn_under_linux_landlock(
        &cat_request(workspace_root.clone(), &secret.display().to_string()),
        &workspace_root,
        &profile,
    )
    .expect("spawn denied cat")
    .wait_with_output()
    .expect("wait denied cat");
    let stdout = String::from_utf8_lossy(&denied_out.stdout);
    assert!(!denied_out.status.success());
    assert!(
        !stdout.contains("DENY_READ_SECRET"),
        "denyRead contents must not be returned: {stdout:?}"
    );
}
