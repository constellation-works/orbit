#![cfg(target_os = "linux")]
use super::linux_sandbox::profile;
use orbit_common::OrbitError;
use orbit_exec::{
    LINUX_STABLE_WORKSPACE_MOUNT, LinuxBwrapMask, compile_linux_bwrap_argv_with_authority,
};
use std::{fs, path::PathBuf};

#[cfg(target_os = "linux")]
struct MaskFixture {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    sentinel: PathBuf,
}

#[cfg(target_os = "linux")]
fn mask_fixture() -> MaskFixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let workspace = root.join("workspace");
    let sentinel = root.join("global/state/plugin-broker/masked");
    let targets = vec![
        root.join("global/state/plugins"),
        root.join("global/state/plugin-secrets"),
    ];
    for dir in [&workspace, &sentinel, &workspace.join("target")]
        .into_iter()
        .chain(&targets)
    {
        fs::create_dir_all(dir).expect("create dir");
    }
    fs::write(sentinel.join(".orbit-brokered"), b"masked").expect("sentinel file");
    MaskFixture {
        _temp: temp,
        workspace,
        sentinel,
    }
}

/// A tree inside the managed worktree is reachable again through the stable
/// `/tmp/orbit-workspace` alias, so one mount cannot hide it.
#[cfg(target_os = "linux")]
#[test]
fn mask_refuses_a_tree_the_plan_aliases_elsewhere() {
    if super::audit_support::run_isolated(
        "linux_masks::mask_refuses_a_tree_the_plan_aliases_elsewhere",
    ) {
        return;
    }
    let fixture = mask_fixture();
    let inside = fixture.workspace.join("state/plugins");
    fs::create_dir_all(&inside).expect("tree in worktree");
    let resolved = profile(vec![format!("{}/**", fixture.workspace.display())]);
    let mask = LinuxBwrapMask {
        sentinel: fixture.sentinel.clone(),
        targets: vec![inside],
        files: Vec::new(),
    };

    let error = compile_linux_bwrap_argv_with_authority(
        &resolved,
        "/bin/true",
        &[],
        Some(&fixture.workspace),
        true,
        Vec::new(),
        Some(&mask),
    )
    .expect_err("aliased tree must refuse the plan");

    let OrbitError::PolicyDenied(message) = error else {
        panic!("expected a policy refusal, got {error:?}");
    };
    assert!(
        message.contains(&format!("{LINUX_STABLE_WORKSPACE_MOUNT}/state/plugins")),
        "the refusal names the alias: {message}"
    );
}
