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

/// Directory symlinks come back as their canonical targets, so `loop -> .`
/// and a two-directory cycle revisit a directory the walk is already in. The
/// canonical path does not grow, and the kernel `ELOOP` limit never trips;
/// compilation has to finish anyway. The fixture runs in a child so a
/// regression overflows that process instead of the test runner.
///
/// In-bound aliases still resolve to the target's grant. A link that leaves
/// the workspace does not gain one.
#[cfg(unix)]
#[test]
fn a_directory_symlink_cycle_finishes_grant_compilation() {
    const CHILD: &str = "ORBIT_LANDLOCK_WORKSPACE_CYCLE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .arg("a_directory_symlink_cycle_finishes_grant_compilation")
            .env(CHILD, "1")
            .output()
            .expect("spawn cycle fixture");
        assert!(
            output.status.success(),
            "cycle fixture failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    self_link_with_an_exclusion_keeps_allowed_and_refuses_denied();
    two_directory_cycle_keeps_allowed_and_refuses_denied();
    in_bound_alias_keeps_the_target_grant();
    disallowed_root_with_a_self_link_grants_only_the_kept_subtree();
    carve_walkers_stay_bounded_and_keep_grants();
}

#[cfg(unix)]
fn self_link_with_an_exclusion_keeps_allowed_and_refuses_denied() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let allowed = workspace.path().join("ok.txt");
    let denied = workspace.path().join("vault/token.txt");
    let outside_file = outside.path().join("secret.txt");
    fs::write(&allowed, "ok").expect("write allowed");
    fs::create_dir(workspace.path().join("vault")).expect("mkdir vault");
    fs::write(&denied, "token").expect("write denied");
    fs::write(&outside_file, "secret").expect("write outside");
    std::os::unix::fs::symlink(".", workspace.path().join("loop")).expect("symlink loop");
    std::os::unix::fs::symlink(&outside_file, workspace.path().join("escape"))
        .expect("symlink escape");
    std::os::unix::fs::symlink("ok.txt", workspace.path().join("alias.txt"))
        .expect("symlink alias");

    let grants = compile(workspace.path(), &profile(&["**"], &["vault/**"]));

    assert!(grants_read(&grants, &allowed), "allowed file: {grants:?}");
    assert!(
        grants_read(&grants, &workspace.path().join("alias.txt")),
        "in-bound file alias: {grants:?}"
    );
    assert!(!grants_read(&grants, &denied), "denied file: {grants:?}");
    assert!(
        !grants_read(&grants, &outside_file),
        "outside file: {grants:?}"
    );
}

#[cfg(unix)]
fn two_directory_cycle_keeps_allowed_and_refuses_denied() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    fs::create_dir_all(workspace.path().join("a/vault")).expect("mkdir a/vault");
    fs::create_dir_all(workspace.path().join("b/vault")).expect("mkdir b/vault");
    let a_ok = workspace.path().join("a/ok.txt");
    let b_ok = workspace.path().join("b/ok.txt");
    let a_denied = workspace.path().join("a/vault/token.txt");
    let b_denied = workspace.path().join("b/vault/token.txt");
    let outside_file = outside.path().join("secret.txt");
    fs::write(&a_ok, "a").expect("write a");
    fs::write(&b_ok, "b").expect("write b");
    fs::write(&a_denied, "no").expect("write a token");
    fs::write(&b_denied, "no").expect("write b token");
    fs::write(&outside_file, "secret").expect("write outside");
    std::os::unix::fs::symlink("../b", workspace.path().join("a/to_b")).expect("symlink a to b");
    std::os::unix::fs::symlink("../a", workspace.path().join("b/to_a")).expect("symlink b to a");
    std::os::unix::fs::symlink(&outside_file, workspace.path().join("a/escape"))
        .expect("symlink escape");

    let grants = compile(
        workspace.path(),
        &profile(&["**"], &["a/vault/**", "b/vault/**"]),
    );

    assert!(grants_read(&grants, &a_ok), "a allowed: {grants:?}");
    assert!(grants_read(&grants, &b_ok), "b allowed: {grants:?}");
    assert!(!grants_read(&grants, &a_denied), "a denied: {grants:?}");
    assert!(!grants_read(&grants, &b_denied), "b denied: {grants:?}");
    assert!(
        !grants_read(&grants, &outside_file),
        "outside file: {grants:?}"
    );
}

#[cfg(unix)]
fn in_bound_alias_keeps_the_target_grant() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::create_dir_all(workspace.path().join("real/vault")).expect("mkdir real/vault");
    let allowed = workspace.path().join("real/ok.txt");
    let denied = workspace.path().join("real/vault/token.txt");
    fs::write(&allowed, "ok").expect("write allowed");
    fs::write(&denied, "token").expect("write denied");
    std::os::unix::fs::symlink("real", workspace.path().join("alias")).expect("symlink alias");
    std::os::unix::fs::symlink(".", workspace.path().join("loop")).expect("symlink loop");

    let grants = compile(workspace.path(), &profile(&["**"], &["real/vault/**"]));

    assert!(grants_read(&grants, &allowed), "real file: {grants:?}");
    assert!(
        grants_read(&grants, &workspace.path().join("alias/ok.txt")),
        "alias file: {grants:?}"
    );
    assert!(!grants_read(&grants, &denied), "real denied: {grants:?}");
    assert!(
        !grants_read(&grants, &workspace.path().join("alias/vault/token.txt")),
        "alias denied: {grants:?}"
    );
}

#[cfg(unix)]
fn disallowed_root_with_a_self_link_grants_only_the_kept_subtree() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::create_dir_all(workspace.path().join("kept/vault")).expect("mkdir kept");
    fs::create_dir(workspace.path().join("a")).expect("mkdir a");
    fs::create_dir(workspace.path().join("b")).expect("mkdir b");
    let allowed = workspace.path().join("kept/ok.txt");
    let denied = workspace.path().join("kept/vault/token.txt");
    let beside = workspace.path().join("beside.txt");
    let hidden = workspace.path().join("a/hidden.txt");
    fs::write(&allowed, "ok").expect("write allowed");
    fs::write(&denied, "token").expect("write denied");
    fs::write(&beside, "beside").expect("write beside");
    fs::write(&hidden, "hidden").expect("write hidden");
    std::os::unix::fs::symlink(".", workspace.path().join("loop")).expect("symlink loop");
    std::os::unix::fs::symlink("../b", workspace.path().join("a/to_b")).expect("symlink a to b");
    std::os::unix::fs::symlink("../a", workspace.path().join("b/to_a")).expect("symlink b to a");

    let grants = compile(workspace.path(), &profile(&["kept/**"], &["kept/vault/**"]));

    assert!(grants_read(&grants, &allowed), "kept file: {grants:?}");
    assert!(!grants_read(&grants, &denied), "kept denied: {grants:?}");
    assert!(!grants_read(&grants, &beside), "beside file: {grants:?}");
    assert!(!grants_read(&grants, &hidden), "cycle file: {grants:?}");
}

#[cfg(unix)]
fn carve_walkers_stay_bounded_and_keep_grants() {
    use std::collections::BTreeSet;

    use super::super::workspace::{carve_out, carve_out_boundary, carve_out_unlistable};

    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let outside_root = outside.path().canonicalize().expect("canonical outside");
    let allowed = root.join("ok.txt");
    let denied = root.join("vault/token.txt");
    let a_ok = root.join("a/ok.txt");
    let b_ok = root.join("b/ok.txt");
    let a_denied = root.join("a/token.txt");
    let outside_file = outside_root.join("secret.txt");
    fs::write(&allowed, "ok").expect("write allowed");
    fs::create_dir(root.join("vault")).expect("mkdir vault");
    fs::write(&denied, "token").expect("write denied");
    fs::create_dir(root.join("a")).expect("mkdir a");
    fs::create_dir(root.join("b")).expect("mkdir b");
    fs::write(&a_ok, "a").expect("write a");
    fs::write(&b_ok, "b").expect("write b");
    fs::write(&a_denied, "no").expect("write a token");
    fs::write(&outside_file, "secret").expect("write outside");
    std::os::unix::fs::symlink(".", root.join("loop")).expect("symlink loop");
    std::os::unix::fs::symlink("../b", root.join("a/to_b")).expect("symlink a to b");
    std::os::unix::fs::symlink("../a", root.join("b/to_a")).expect("symlink b to a");
    std::os::unix::fs::symlink(&outside_file, root.join("escape")).expect("symlink escape");

    let denied_paths = BTreeSet::from([denied.clone(), a_denied.clone()]);
    let carved = carve_out(&root, &denied_paths).expect("carve_out");
    assert!(
        grants_read(&carved, &allowed),
        "carve_out allowed: {carved:?}"
    );
    assert!(grants_read(&carved, &a_ok), "carve_out a: {carved:?}");
    assert!(grants_read(&carved, &b_ok), "carve_out b: {carved:?}");
    assert!(
        !grants_read(&carved, &denied),
        "carve_out denied: {carved:?}"
    );
    assert!(
        !grants_read(&carved, &a_denied),
        "carve_out a denied: {carved:?}"
    );
    assert!(
        !grants_read(&carved, &outside_file),
        "carve_out outside: {carved:?}"
    );

    let unlisted = carve_out_unlistable(&root, &denied_paths).expect("carve_out_unlistable");
    assert!(
        grants_read(&unlisted, &allowed),
        "unlistable allowed: {unlisted:?}"
    );
    assert!(grants_read(&unlisted, &a_ok), "unlistable a: {unlisted:?}");
    assert!(grants_read(&unlisted, &b_ok), "unlistable b: {unlisted:?}");
    assert!(
        !grants_read(&unlisted, &denied),
        "unlistable denied: {unlisted:?}"
    );
    assert!(
        !grants_read(&unlisted, &a_denied),
        "unlistable a denied: {unlisted:?}"
    );
    assert!(
        !grants_read(&unlisted, &outside_file),
        "unlistable outside: {unlisted:?}"
    );

    let listable = BTreeSet::from([denied.clone()]);
    let unlistable = BTreeSet::from([a_denied.clone()]);
    let boundary = carve_out_boundary(&root, &unlistable, &listable).expect("carve_out_boundary");
    assert!(
        grants_read(&boundary, &allowed),
        "boundary allowed: {boundary:?}"
    );
    assert!(grants_read(&boundary, &a_ok), "boundary a: {boundary:?}");
    assert!(grants_read(&boundary, &b_ok), "boundary b: {boundary:?}");
    assert!(
        !grants_read(&boundary, &denied),
        "boundary listable deny: {boundary:?}"
    );
    assert!(
        !grants_read(&boundary, &a_denied),
        "boundary unlistable deny: {boundary:?}"
    );
    assert!(
        !grants_read(&boundary, &outside_file),
        "boundary outside: {boundary:?}"
    );
}

/// Grant compilation walks real filenames. A recursive deny has to exclude a
/// name that contains a newline, and a recursive grant has to include one
/// whose path only matches because `.*` consumes that newline.
#[test]
fn newline_filenames_follow_recursive_grants_and_denies() {
    let workspace = tempfile::tempdir().expect("workspace");
    let secrets = workspace.path().join("secrets");
    fs::create_dir(&secrets).expect("secrets");
    let sibling = secrets.join("ab");
    let newline_secret = secrets.join("a\nb");
    fs::write(&sibling, "s").expect("sibling");
    fs::write(&newline_secret, "n").expect("newline secret");

    let nested_dir = workspace.path().join("a\nb");
    let nested_leaf = nested_dir.join("leaf");
    let nested_other = nested_dir.join("other");
    fs::create_dir(&nested_dir).expect("newline dir");
    fs::write(&nested_leaf, "leaf").expect("leaf");
    fs::write(&nested_other, "other").expect("other");

    let vault_token = nested_dir.join("vault").join("token");
    fs::create_dir(vault_token.parent().expect("vault parent")).expect("vault");
    fs::write(&vault_token, "token").expect("token");
    let other_token = nested_dir.join("other-dir").join("token");
    fs::create_dir(other_token.parent().expect("other parent")).expect("other-dir");
    fs::write(&other_token, "token").expect("other token");

    let denied = compile(workspace.path(), &profile(&["secrets/*"], &["secrets/**"]));
    assert!(
        !grants_read(&denied, &sibling),
        "ordinary sibling stays denied: {denied:?}"
    );
    assert!(
        !grants_read(&denied, &newline_secret),
        "newline filename stays denied: {denied:?}"
    );

    let leading = compile(workspace.path(), &profile(&["**/leaf"], &[]));
    assert!(
        grants_read(&leading, &nested_leaf),
        "**/leaf grants a newline directory: {leading:?}"
    );
    assert!(
        !grants_read(&leading, &nested_other),
        "**/leaf does not grant the sibling: {leading:?}"
    );

    let trailing = compile(workspace.path(), &profile(&["**/vault/**"], &[]));
    assert!(
        grants_read(&trailing, &vault_token),
        "trailing /** grants through a newline segment: {trailing:?}"
    );
    assert!(
        !grants_read(&trailing, &other_token),
        "trailing /** does not grant a different directory: {trailing:?}"
    );
}
