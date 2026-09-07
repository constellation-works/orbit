//! Unit tests for Landlock grant compilation. These do not apply a ruleset.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use orbit_types::policy::ResolvedFsProfile;

use super::{LandlockPathGrant, linux_landlock_read_grants};

fn unrestricted_with_denies(denies: &[&str]) -> ResolvedFsProfile {
    let mut read = vec!["./**".to_string()];
    read.extend(denies.iter().map(|rule| format!("!{rule}")));
    ResolvedFsProfile {
        name: "test".to_string(),
        read,
        modify: vec!["./**".to_string()],
    }
}

fn restricted_allowed_only() -> ResolvedFsProfile {
    ResolvedFsProfile {
        name: "restricted".to_string(),
        read: vec!["allowed/**".to_string()],
        modify: vec!["allowed/**".to_string()],
    }
}

fn grant_reads_file(grants: &[LandlockPathGrant], path: &Path) -> bool {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    grants
        .iter()
        .any(|grant| grant.access.read_file && path.starts_with(&grant.path))
}

#[test]
fn unrestricted_profile_grants_the_workspace_and_not_a_host_sentinel() {
    let workspace = tempfile::tempdir().expect("workspace");
    let host = tempfile::tempdir().expect("host");
    let visible = workspace.path().join("visible.txt");
    let sentinel = host.path().join("sentinel.txt");
    fs::write(&visible, "ok").expect("write visible");
    fs::write(&sentinel, "secret").expect("write sentinel");

    let grants = linux_landlock_read_grants(workspace.path(), &unrestricted_with_denies(&[]))
        .expect("compile grants");

    assert!(
        grant_reads_file(&grants, &visible),
        "workspace file should be readable: {grants:?}"
    );
    assert!(
        !grant_reads_file(&grants, &sentinel),
        "host sentinel must not be readable: {grants:?}"
    );
}

#[test]
fn deny_read_file_is_punched_out_of_the_workspace_grant() {
    let workspace = tempfile::tempdir().expect("workspace");
    let allowed = workspace.path().join("allowed.txt");
    let secret = workspace.path().join(".env");
    fs::write(&allowed, "ok").expect("write allowed");
    fs::write(&secret, "token").expect("write secret");

    let grants =
        linux_landlock_read_grants(workspace.path(), &unrestricted_with_denies(&["**/.env"]))
            .expect("compile grants");

    assert!(
        grant_reads_file(&grants, &allowed),
        "non-denied sibling should stay readable: {grants:?}"
    );
    assert!(
        !grant_reads_file(&grants, &secret),
        "denyRead file must not inherit a parent READ_FILE grant: {grants:?}"
    );
}

#[test]
fn restricted_profile_does_not_grant_a_sibling_outside_the_read_root() {
    let workspace = tempfile::tempdir().expect("workspace");
    let allowed_dir = workspace.path().join("allowed");
    fs::create_dir(&allowed_dir).expect("mkdir allowed");
    let allowed = allowed_dir.join("visible.txt");
    let denied = workspace.path().join("denied.txt");
    fs::write(&allowed, "visible").expect("write allowed");
    fs::write(&denied, "hidden").expect("write denied");

    let grants =
        linux_landlock_read_grants(workspace.path(), &restricted_allowed_only()).expect("grants");

    assert!(
        grant_reads_file(&grants, &allowed),
        "allowed subtree should be readable: {grants:?}"
    );
    assert!(
        !grant_reads_file(&grants, &denied),
        "path outside the read root must not be granted: {grants:?}"
    );
}

/// Grant compilation runs before every activity-scoped spawn, so it must stay
/// proportional to the tree rather than to the rule set. Compiling the read
/// globs per visited path cost roughly a millisecond each in a debug build,
/// which turned an ordinary workspace into a minute of setup per spawn.
#[test]
fn grant_compilation_stays_fast_on_a_large_workspace() {
    const FILES: usize = 3_000;
    const BUDGET: Duration = Duration::from_secs(10);

    let workspace = tempfile::tempdir().expect("workspace");
    for bucket in 0..30 {
        let dir = workspace.path().join(format!("bucket{bucket}"));
        fs::create_dir(&dir).expect("mkdir bucket");
        for file in 0..(FILES / 30) {
            fs::write(dir.join(format!("file{file}.txt")), "x").expect("write file");
        }
    }
    let profile = unrestricted_with_denies(&["**/.env", "**/.env.*", "**/*.env", "**/*.env.*"]);

    let started = Instant::now();
    let grants = linux_landlock_read_grants(workspace.path(), &profile).expect("compile grants");
    let elapsed = started.elapsed();

    assert!(
        !grants.is_empty(),
        "an unrestricted profile should still grant the workspace"
    );
    assert!(
        elapsed < BUDGET,
        "compiling grants for {FILES} files took {elapsed:?}, over the {BUDGET:?} budget"
    );
}

/// A profile that allows nothing needs no workspace grants at all.
#[test]
fn empty_read_profile_grants_no_workspace_path() {
    let workspace = tempfile::tempdir().expect("workspace");
    let file = workspace.path().join("visible.txt");
    fs::write(&file, "ok").expect("write file");
    let profile = ResolvedFsProfile {
        name: "pure_compute".to_string(),
        read: Vec::new(),
        modify: Vec::new(),
    };

    let grants = linux_landlock_read_grants(workspace.path(), &profile).expect("compile grants");

    assert!(
        !grant_reads_file(&grants, &file),
        "an empty read profile must not grant a workspace file: {grants:?}"
    );
}
