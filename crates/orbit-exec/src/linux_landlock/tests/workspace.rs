//! Compiling a resolved read profile into workspace grants.
//!
//! These decide grants without applying a ruleset, so they run anywhere. The
//! kernel behaviour they stand for is covered by `tests/linux_landlock.rs`.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use orbit_types::policy::ResolvedFsProfile;

use crate::linux_landlock::{LandlockPathGrant, grants_read, linux_landlock_read_boundary};

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
    linux_landlock_read_boundary(workspace, profile, &[])
        .expect("compile grants")
        .grants
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

/// The gap this module closed: a bounded exclusion has to shape the ruleset
/// even when nothing matches it yet, or the same profile enforces a secret
/// that already exists and hands over the identical secret written a moment
/// after the ruleset was compiled. [F2026-09-054]
#[test]
fn a_directory_a_deny_rule_can_still_name_into_is_not_granted_as_a_tree() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::create_dir(workspace.path().join("src")).expect("mkdir src");
    fs::write(workspace.path().join("src/main.rs"), "code").expect("write allowed");

    let grants = compile(workspace.path(), &profile(&["**"], &["secrets/**"]));

    // `secrets/` does not exist, so nothing is carved out of the tree that
    // exists — but the root can still gain it.
    let appears_later = workspace.path().join("secrets/token.txt");
    assert!(!grants_read(&grants, &appears_later), "{grants:?}");
    assert!(
        !grants_read(&grants, &workspace.path().join("root-generated.txt")),
        "{grants:?}"
    );
}

/// The cost of that carve-out has to stay where the rule points. A directory
/// the exclusion cannot name into keeps its whole-tree grant, which is what
/// lets a build read the files it produces.
#[test]
fn a_directory_out_of_reach_keeps_its_tree_grant() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::create_dir(workspace.path().join("src")).expect("mkdir src");
    fs::write(workspace.path().join("src/main.rs"), "code").expect("write allowed");

    let grants = compile(workspace.path(), &profile(&["**"], &["secrets/**"]));

    let generated = workspace.path().join("src/build.out");
    assert!(grants_read(&grants, &generated), "{grants:?}");
    assert!(
        grants_read(&grants, &workspace.path().join("src/main.rs")),
        "{grants:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_missing_child_uses_the_canonical_identity_of_its_parent() {
    let workspace = tempfile::tempdir().expect("workspace");
    let alias_parent = tempfile::tempdir().expect("alias parent");
    fs::create_dir(workspace.path().join("src")).expect("mkdir src");

    let alias_root = alias_parent.path().join("workspace-alias");
    std::os::unix::fs::symlink(workspace.path(), &alias_root).expect("symlink workspace");
    let grants = compile(&alias_root, &profile(&["**"], &[]));

    assert!(
        grants_read(&grants, &alias_root.join("src/generated.out")),
        "{grants:?}"
    );
}

/// A `*` cannot cross a separator, so it narrows the reach to one level rather
/// than opening the whole subtree.
#[test]
fn a_segment_wildcard_reaches_only_the_directory_it_names() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::create_dir_all(workspace.path().join("config/nested")).expect("mkdir config/nested");
    fs::write(workspace.path().join("config/nested/app.key"), "k").expect("write nested");

    let grants = compile(workspace.path(), &profile(&["**"], &["config/*.key"]));

    assert!(
        !grants_read(&grants, &workspace.path().join("config/app.key")),
        "{grants:?}"
    );
    assert!(
        grants_read(&grants, &workspace.path().join("config/nested/app.key")),
        "{grants:?}"
    );
}

/// An exclusion whose `**` crosses directories can name a path beneath every
/// directory in the workspace. Carving that out would withdraw read access
/// from every generated file, so it is reported instead of enforced — and
/// reporting it is what stops another layer describing this boundary as
/// whole-contract enforcement.
#[test]
fn an_unbounded_exclusion_is_reported_rather_than_silently_dropped() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::create_dir(workspace.path().join("src")).expect("mkdir src");
    fs::write(workspace.path().join("src/main.rs"), "code").expect("write allowed");

    let boundary =
        linux_landlock_read_boundary(workspace.path(), &profile(&["**"], DEFAULT_DENY_READ), &[])
            .expect("compile boundary");

    assert_eq!(boundary.unenforced_exclusions, DEFAULT_DENY_READ);
    assert!(
        grants_read(&boundary.grants, &workspace.path().join("src/build.out")),
        "{:?}",
        boundary.grants
    );
}

/// A bounded exclusion in the same profile is still enforced; the two classes
/// are decided per rule rather than per profile.
#[test]
fn a_bounded_exclusion_is_enforced_alongside_an_unbounded_one() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::create_dir(workspace.path().join("src")).expect("mkdir src");
    fs::write(workspace.path().join("src/main.rs"), "code").expect("write allowed");

    let mut denies = DEFAULT_DENY_READ.to_vec();
    denies.push("vault/**");
    let boundary = linux_landlock_read_boundary(workspace.path(), &profile(&["**"], &denies), &[])
        .expect("compile boundary");

    assert_eq!(boundary.unenforced_exclusions, DEFAULT_DENY_READ);
    assert!(
        !grants_read(&boundary.grants, &workspace.path().join("vault/token")),
        "{:?}",
        boundary.grants
    );
    assert!(
        grants_read(&boundary.grants, &workspace.path().join("src/main.rs")),
        "{:?}",
        boundary.grants
    );
}

/// A profile with no exclusion at all reports none, and pays nothing for the
/// analysis.
#[test]
fn a_profile_without_exclusions_reports_nothing_unenforced() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::write(workspace.path().join("visible.txt"), "ok").expect("write file");

    let boundary = linux_landlock_read_boundary(workspace.path(), &profile(&["**"], &[]), &[])
        .expect("compile boundary");

    assert!(boundary.unenforced_exclusions.is_empty());
}
