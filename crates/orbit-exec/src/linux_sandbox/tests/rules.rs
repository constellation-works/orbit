use super::*;

/// Every rule sharing a search root is matched from one walk: each rule's
/// own set, and their union, equal what the rules match one at a time.
#[test]
fn rules_sharing_a_root_expand_from_one_walk_to_the_same_set() {
    let temp = tree();
    let root = temp.path().canonicalize().expect("canonical root");
    let prefix = root.to_string_lossy().replace('\\', "/");
    let rules: Vec<String> = ["**/.env", "**/.env.*", "**/*.env", "**/*.env.*"]
        .iter()
        .map(|glob| format!("{prefix}/{glob}"))
        .collect();

    let each = expand_each_rule(rules.iter().map(String::as_str)).expect("expand each");
    let together = expand_rules(&rules).expect("expand together");
    let mut one_at_a_time = BTreeSet::new();
    for rule in &rules {
        let alone = expand_rules(std::slice::from_ref(rule)).expect("expand one");
        assert_eq!(
            each.get(rule.as_str()),
            Some(&alone),
            "per-rule set for {rule}"
        );
        one_at_a_time.extend(alone);
    }
    assert_eq!(together, one_at_a_time);

    let expected: BTreeSet<PathBuf> = [
        ".env",
        "a/.env",
        "a/deep/.env.local",
        "b/settings.env",
        "target/debug/build.env.bak",
    ]
    .iter()
    .map(|rel| canonical(&root, rel))
    .collect();
    assert_eq!(together, expected);
}

/// The walk lists each path exactly once: directories used to be pushed on
/// entry and again from their parent's listing.
#[test]
fn walk_lists_every_path_once() {
    let temp = tree();
    let root = temp.path().canonicalize().expect("canonical root");
    let mut paths = Vec::new();
    walk_paths(&root, &mut paths).expect("walk");
    let unique: BTreeSet<&PathBuf> = paths.iter().collect();
    assert_eq!(unique.len(), paths.len(), "duplicates in {paths:?}");
    // 1 root + 4 dirs (a, a/deep, b, target, target/debug = 5) + 6 files.
    assert_eq!(paths.len(), 1 + 5 + 6);
    assert_eq!(paths[0], root);
}

#[test]
fn capture_watches_absent_exact_and_subtree_denies() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let secrets = workspace.join("secrets");
    let lock = workspace.join("Cargo.lock");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", secrets.display()),
        format!("!{}", lock.display()),
    ]);

    let guard = LinuxBwrapPostRunGuard::capture(&resolved)
        .expect("capture")
        .expect("absent exact/subtree denies must be guarded");
    fs::create_dir_all(&secrets).expect("create secrets");
    fs::write(secrets.join("x"), b"k").expect("write secret");
    fs::write(&lock, b"k").expect("write lock");

    let error = guard
        .verify()
        .expect_err("creating an absent deny root must fail closed");
    assert!(
        matches!(error, OrbitError::PolicyDenied(_)),
        "expected PolicyDenied, got {error}"
    );
}

/// macOS commonly reaches `/private/var` through the `/var` symlink. The
/// guard must match rules written through that spelling even though its walk
/// canonicalizes the search root.
#[cfg(unix)]
#[test]
fn capture_watches_absent_denies_through_a_symlinked_workspace_path() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let real_workspace = temp.path().join("real-workspace");
    let workspace = temp.path().join("workspace-link");
    fs::create_dir_all(&real_workspace).expect("real workspace");
    symlink(&real_workspace, &workspace).expect("workspace symlink");

    let secrets = workspace.join("secrets");
    let lock = workspace.join("Cargo.lock");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", secrets.display()),
        format!("!{}", lock.display()),
    ]);

    let guard = LinuxBwrapPostRunGuard::capture(&resolved)
        .expect("capture")
        .expect("absent exact/subtree denies must be guarded");
    fs::create_dir_all(&secrets).expect("create secrets");
    fs::write(secrets.join("x"), b"k").expect("write secret");
    fs::write(&lock, b"k").expect("write lock");

    let error = guard
        .verify()
        .expect_err("creating a deny root through a symlink must fail closed");
    assert!(
        matches!(error, OrbitError::PolicyDenied(_)),
        "expected PolicyDenied, got {error}"
    );
}

/// The compile hands out the same snapshot `capture` would walk for, from the
/// walk it already made for the deny mounts; a direct invocation carries none.
#[test]
fn compile_reuses_its_walk_for_the_post_run_guard() {
    let temp = tree();
    let root = temp.path().canonicalize().expect("canonical root");
    let prefix = root.to_string_lossy().replace('\\', "/");
    let resolved = profile(vec![
        format!("{prefix}/**"),
        format!("!{prefix}/**/.env"),
        format!("!{prefix}/**/*.env"),
        format!("!{prefix}/secrets/**"),
    ]);

    let captured = LinuxBwrapPostRunGuard::capture(&resolved)
        .expect("capture")
        .expect("guarded");
    let mut managed = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&root), true)
        .expect("compile managed");
    assert_eq!(managed.take_post_run_guard(), Some(captured));
    assert_eq!(
        managed.take_post_run_guard(),
        None,
        "the guard is handed out once"
    );

    // A profile whose non-subtree denies stay outside the writable root is a
    // legal direct invocation, and it still carries no post-run guard.
    let outside = tempfile::tempdir().expect("outside");
    let outside_prefix = outside
        .path()
        .canonicalize()
        .expect("canonical outside")
        .to_string_lossy()
        .replace('\\', "/");
    let direct = profile(vec![
        format!("{prefix}/**"),
        format!("!{outside_prefix}/**/.env"),
    ]);
    let mut direct = compile_linux_bwrap_argv(&direct, "/bin/true", &[], Some(&root), false)
        .expect("compile direct");
    assert_eq!(direct.take_post_run_guard(), None);
}

/// The process-level probe is stable across calls. On a host without the
/// trusted binary the outcome is the deterministic unavailable message, which
/// is exactly the case a per-dispatch re-probe kept paying for.
#[test]
fn probe_bwrap_returns_the_same_outcome_on_repeat() {
    let first = probe_bwrap();
    let second = probe_bwrap();
    assert_eq!(first, second);
}

/// A capability probe that ran and exited non-zero is not remembered, so a
/// later call re-probes and can report available without restarting.
#[test]
fn probe_bwrap_reprobes_after_a_failed_capability_probe() {
    let settled = OnceLock::new();
    let calls = Cell::new(0);
    let failed = synthetic_probe(
        false,
        "Bubblewrap capability probe failed: setting up uid map: No space left on device",
    );
    let available = synthetic_probe(true, "capability probe succeeded");

    let first = probe_bwrap_with(&settled, || {
        calls.set(calls.get() + 1);
        BwrapProbeMemo::Unsettled(failed.clone())
    });
    assert_eq!(first, failed);
    assert_eq!(calls.get(), 1);
    assert!(
        settled.get().is_none(),
        "a failed capability probe must not pin the process memo"
    );

    let second = probe_bwrap_with(&settled, || {
        calls.set(calls.get() + 1);
        BwrapProbeMemo::Settled(available.clone())
    });
    assert_eq!(second, available);
    assert_eq!(calls.get(), 2);
    assert_eq!(settled.get(), Some(&available));
}

/// Settled host properties stay cached so dispatch does not re-spawn two
/// processes per call.
#[test]
fn probe_bwrap_memoises_settled_outcomes() {
    let cases = [
        synthetic_probe(true, "capability probe succeeded"),
        synthetic_probe(false, "trusted Bubblewrap not available at /usr/bin/bwrap"),
        synthetic_probe(
            false,
            "Bubblewrap does not support the required --bind-fd object-authority mount",
        ),
    ];
    for expected in cases {
        let settled = OnceLock::new();
        let calls = Cell::new(0);
        let first = probe_bwrap_with(&settled, || {
            calls.set(calls.get() + 1);
            BwrapProbeMemo::Settled(expected.clone())
        });
        let second = probe_bwrap_with(&settled, || {
            calls.set(calls.get() + 1);
            panic!("settled outcome must not re-probe: {}", expected.detail);
        });
        assert_eq!(first, expected);
        assert_eq!(second, expected);
        assert_eq!(calls.get(), 1, "cached outcome still probed: {expected:?}");
        assert_eq!(settled.get(), Some(&expected));
    }
}

#[test]
fn capture_skips_absent_deny_whose_nested_reallow_will_create_the_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let orbit = workspace.join(".orbit");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", orbit.display()),
        format!("{}/**", orbit.join("auto_tasks").display()),
    ]);

    assert!(
        LinuxBwrapPostRunGuard::capture(&resolved)
            .expect("capture")
            .is_none(),
        "grant preparation will create .orbit, so watching it would false-positive"
    );
}
