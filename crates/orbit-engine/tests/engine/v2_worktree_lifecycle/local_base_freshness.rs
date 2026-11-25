//! [ORB-28152] Owner-base freshness for `base_sync: local` worktree setup.
//!
//! A local run cuts its worktree from the owner checkout's `<base>` branch,
//! which a push from another checkout leaves behind `origin/<base>`. Setup
//! fast-forwards a clean owner to origin before cutting the worktree. A dirty
//! or diverged owner is refused and left unmoved, naming the checkout and both
//! commits.

use super::*;

/// Push one commit to `origin/<BASE>` from a second clone, as another checkout
/// would. Returns the new origin tip.
fn push_to_origin_from_elsewhere(fixture: &Fixture, file: &str) -> String {
    let origin = fixture.root.path().join("origin.git");
    let elsewhere = fixture.root.path().join(format!("elsewhere-{file}"));
    git(
        fixture.root.path(),
        &[
            "clone",
            "--branch",
            BASE,
            path_str(&origin),
            path_str(&elsewhere),
        ],
    );
    git(&elsewhere, &["config", "user.name", "Orbit Test"]);
    git(
        &elsewhere,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    let tip = commit_file(&elsewhere, file, "pushed elsewhere\n");
    git(&elsewhere, &["push", "origin", BASE]);
    tip
}

/// The refusal is the stale-branch hold and names the owner checkout and both
/// commits.
fn assert_owner_base_refusal(error: &OrbitError, checkout: &Path, owner: &str, origin: &str) {
    assert!(
        matches!(
            error,
            OrbitError::DeliveryRefused {
                refusal: orbit_types::workflow::DeliveryRefusal::StaleBranch,
                ..
            }
        ),
        "expected a stale-branch delivery refusal, got {error:?}"
    );
    let checkout = checkout.display().to_string();
    let message = error.to_string();
    for fragment in [checkout.as_str(), owner, origin] {
        assert!(
            message.contains(fragment),
            "the refusal must name {fragment}: {message}"
        );
    }
}

/// Behind and clean: the owner advances to origin, the run records that commit
/// as its base, and the owner's working tree carries the new file.
fn a_clean_owner_behind_origin_is_fast_forwarded_before_the_worktree_is_cut() {
    let (fixture, host) = Fixture::hosted();
    let owner_before = head(&fixture.repo);
    let origin_tip = push_to_origin_from_elsewhere(&fixture, "from-elsewhere.txt");
    assert_ne!(owner_before, origin_tip, "origin moved ahead of the owner");
    host.add_task("T-FF", TaskStatus::Backlog);
    let output = action(&host, "worktree_setup", &setup_input(&["T-FF"], "jrun-ff"))
        .expect("a clean owner that origin fast-forwards is admitted");
    assert_eq!(
        output["base_sha"], origin_tip,
        "the run records the origin commit it was cut from"
    );
    let checkout = Checkout::from_setup(&output);
    assert_eq!(
        head(&checkout.path),
        origin_tip,
        "the worktree is cut from origin"
    );
    assert_eq!(
        head(&fixture.repo),
        origin_tip,
        "the owner advanced by a fast-forward"
    );
    assert_eq!(
        read(fixture.repo.join("from-elsewhere.txt")),
        "pushed elsewhere\n",
        "the owner's working tree carries the fast-forwarded commit"
    );
    assert_eq!(host.admitted().len(), 1, "the clean owner admits the task");
}

/// Behind and dirty: the owner keeps its edit and its commit, and setup refuses
/// before any task is admitted or any worktree is created.
fn a_dirty_owner_behind_origin_is_refused_and_left_unmoved() {
    let (fixture, host) = Fixture::hosted();
    let owner_before = head(&fixture.repo);
    let origin_tip = push_to_origin_from_elsewhere(&fixture, "from-elsewhere.txt");
    fs::write(fixture.repo.join("README.md"), "owner edit\n").unwrap();
    let checkouts = registered_worktrees(&fixture.repo);
    host.add_task("T-DIRTY", TaskStatus::Backlog);
    let error = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-DIRTY"], "jrun-dirty"),
    )
    .expect_err("a dirty owner that origin is ahead of is refused");
    assert_owner_base_refusal(&error, &fixture.repo, &owner_before, &origin_tip);
    assert_eq!(
        head(&fixture.repo),
        owner_before,
        "the dirty owner is not moved"
    );
    assert_eq!(
        read(fixture.repo.join("README.md")),
        "owner edit\n",
        "the owner's edit survives the refusal"
    );
    assert!(
        !fixture.repo.join("from-elsewhere.txt").exists(),
        "the refused owner does not receive origin's commit"
    );
    assert!(host.admitted().is_empty(), "a refused setup admits no task");
    assert_eq!(
        registered_worktrees(&fixture.repo),
        checkouts,
        "a refused setup creates no worktree"
    );
}

/// Diverged: the owner holds a commit origin lacks, and origin holds one the
/// owner lacks. Neither side is moved.
fn a_diverged_owner_base_is_refused_and_left_unmoved() {
    let (fixture, host) = Fixture::hosted();
    let owner_before = commit_file(&fixture.repo, "owner-only.txt", "owner commit\n");
    let origin_tip = push_to_origin_from_elsewhere(&fixture, "from-elsewhere.txt");
    host.add_task("T-DIVERGED", TaskStatus::Backlog);
    let error = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-DIVERGED"], "jrun-diverged"),
    )
    .expect_err("a diverged owner base is refused");
    assert_owner_base_refusal(&error, &fixture.repo, &owner_before, &origin_tip);
    assert_eq!(
        head(&fixture.repo),
        owner_before,
        "the diverged owner is not moved"
    );
    assert!(host.admitted().is_empty(), "a refused setup admits no task");
}

/// Without an origin remote, or with one that cannot be fetched, local setup
/// keeps cutting from the owner's own branch.
fn local_setup_keeps_the_owner_base_when_origin_is_absent_or_unreachable() {
    let (fixture, host) = Fixture::hosted();
    let owner = head(&fixture.repo);
    git(&fixture.repo, &["remote", "remove", "origin"]);
    host.add_task("T-NO-ORIGIN", TaskStatus::Backlog);
    let output = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-NO-ORIGIN"], "jrun-no-origin"),
    )
    .expect("local setup without an origin remote is admitted");
    assert_eq!(
        output["base_sha"], owner,
        "no origin: the owner's branch is the base"
    );

    let (fixture, host) = Fixture::hosted();
    let owner = head(&fixture.repo);
    let missing = fixture.root.path().join("missing.git");
    git(
        &fixture.repo,
        &["remote", "set-url", "origin", path_str(&missing)],
    );
    host.add_task("T-UNREACHABLE", TaskStatus::Backlog);
    let output = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-UNREACHABLE"], "jrun-unreachable"),
    )
    .expect("local setup with an unreachable origin is admitted");
    assert_eq!(
        output["base_sha"], owner,
        "unreachable origin: the owner's branch is the base"
    );
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn local_base_freshness_contracts() {
    isolated_in(module_path!(), "local_base_freshness_contracts", || {
        crate::git_fixture::run_scenarios(&[
            (
                "a_clean_owner_behind_origin_is_fast_forwarded_before_the_worktree_is_cut",
                a_clean_owner_behind_origin_is_fast_forwarded_before_the_worktree_is_cut,
            ),
            (
                "a_dirty_owner_behind_origin_is_refused_and_left_unmoved",
                a_dirty_owner_behind_origin_is_refused_and_left_unmoved,
            ),
            (
                "a_diverged_owner_base_is_refused_and_left_unmoved",
                a_diverged_owner_base_is_refused_and_left_unmoved,
            ),
            (
                "local_setup_keeps_the_owner_base_when_origin_is_absent_or_unreachable",
                local_setup_keeps_the_owner_base_when_origin_is_absent_or_unreachable,
            ),
        ]);
    });
}
