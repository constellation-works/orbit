//! Compiling a resolved read profile into workspace grants.
//!
//! These decide grants without applying a ruleset, so they run anywhere. The
//! kernel behaviour they stand for is covered by `tests/linux_landlock.rs`.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use orbit_types::policy::ResolvedFsProfile;

use crate::linux_landlock::{LandlockPathGrant, grants_read, linux_landlock_grants};

const DEFAULT_DENY_READ: &[&str] = &["**/.env", "**/.env.*", "**/*.env", "**/*.env.*"];

fn profile(read: &[&str], denies: &[&str]) -> ResolvedFsProfile {
    let mut rules: Vec<String> = read.iter().map(ToString::to_string).collect();
    rules.extend(denies.iter().map(|rule| format!("!{rule}")));
    ResolvedFsProfile {
        name: "test".to_string(),
        read: rules,
        modify: vec!["**".to_string()],
    }
}

fn compile(workspace: &Path, profile: &ResolvedFsProfile) -> Vec<LandlockPathGrant> {
    linux_landlock_grants(workspace, profile, &[]).expect("compile grants")
}

#[test]
fn a_workspace_profile_grants_the_workspace_and_not_a_host_sibling() {
    let workspace = tempfile::tempdir().expect("workspace");
    let host = tempfile::tempdir().expect("host");
    let visible = workspace.path().join("visible.txt");
    let outside = host.path().join("secret.txt");
    fs::write(&visible, "ok").expect("write visible");
    fs::write(&outside, "secret").expect("write outside");

    let grants = compile(workspace.path(), &profile(&["**"], &[]));

    assert!(grants_read(&grants, &visible), "{grants:?}");
    assert!(!grants_read(&grants, &outside), "{grants:?}");
}

#[test]
fn a_denied_file_keeps_no_readable_ancestor() {
    let workspace = tempfile::tempdir().expect("workspace");
    let allowed = workspace.path().join("src/main.rs");
    let denied = workspace.path().join("src/.env");
    fs::create_dir(workspace.path().join("src")).expect("mkdir src");
    fs::write(&allowed, "code").expect("write allowed");
    fs::write(&denied, "token").expect("write denied");

    let grants = compile(workspace.path(), &profile(&["**"], DEFAULT_DENY_READ));

    assert!(grants_read(&grants, &allowed), "{grants:?}");
    assert!(!grants_read(&grants, &denied), "{grants:?}");
}

#[test]
fn a_profile_that_reads_one_subtree_grants_nothing_beside_it() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::create_dir(workspace.path().join("allowed")).expect("mkdir allowed");
    let inside = workspace.path().join("allowed/visible.txt");
    let beside = workspace.path().join("hidden.txt");
    fs::write(&inside, "visible").expect("write inside");
    fs::write(&beside, "hidden").expect("write beside");

    let grants = compile(workspace.path(), &profile(&["allowed/**"], &[]));

    assert!(grants_read(&grants, &inside), "{grants:?}");
    assert!(!grants_read(&grants, &beside), "{grants:?}");
}

#[test]
fn an_empty_read_profile_grants_no_workspace_path() {
    let workspace = tempfile::tempdir().expect("workspace");
    let file = workspace.path().join("visible.txt");
    fs::write(&file, "ok").expect("write file");

    let grants = compile(
        workspace.path(),
        &ResolvedFsProfile {
            name: "pure_compute".to_string(),
            read: Vec::new(),
            modify: Vec::new(),
        },
    );

    assert!(!grants_read(&grants, &file), "{grants:?}");
}

/// A symlink out of the workspace must not smuggle its target into the
/// ruleset. The child following it is denied where the target really lives,
/// which is where the profile has authority to speak.
#[cfg(unix)]
#[test]
fn a_symlink_leaving_the_workspace_is_not_granted() {
    let workspace = tempfile::tempdir().expect("workspace");
    let host = tempfile::tempdir().expect("host");
    let outside = host.path().join("secret.txt");
    fs::write(&outside, "secret").expect("write outside");
    std::os::unix::fs::symlink(&outside, workspace.path().join("link.txt")).expect("symlink");

    let grants = compile(workspace.path(), &profile(&["**"], &[]));

    assert!(!grants_read(&grants, &outside), "{grants:?}");
}

/// Grant compilation runs before every activity-scoped spawn, so it has to
/// stay proportional to the tree rather than to the rule set. Evaluating the
/// profile's globs per visited path — which rebuilds a regex each time — turned
/// an ordinary repository into a minute of setup per spawn. [ORB-11514]
#[test]
fn grant_compilation_stays_within_budget_on_a_large_workspace() {
    const DIRECTORIES: usize = 30;
    const FILES_PER_DIRECTORY: usize = 100;
    const BUDGET: Duration = Duration::from_secs(10);

    let workspace = tempfile::tempdir().expect("workspace");
    for directory in 0..DIRECTORIES {
        let path = workspace.path().join(format!("bucket{directory}"));
        fs::create_dir(&path).expect("mkdir bucket");
        for file in 0..FILES_PER_DIRECTORY {
            fs::write(path.join(format!("file{file}.txt")), "x").expect("write file");
        }
    }

    let started = Instant::now();
    let grants = compile(workspace.path(), &profile(&["**"], DEFAULT_DENY_READ));
    let elapsed = started.elapsed();

    assert!(!grants.is_empty(), "the workspace should still be granted");
    assert!(
        elapsed < BUDGET,
        "compiling grants for {} files took {elapsed:?}, over the {BUDGET:?} budget",
        DIRECTORIES * FILES_PER_DIRECTORY
    );
}
