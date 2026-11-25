//! [ORB-28152] Owner-base freshness for `base_sync: local` worktree setup.
//!
//! A local run cuts its worktree from the owner checkout's `<base>` branch,
//! which a push from another checkout leaves behind `origin/<base>`. Setup
//! selects the fetched commit without moving the owner. A dirty
//! or diverged owner is refused and left unmoved, naming the checkout and both
//! commits.

use super::*;

#[cfg(unix)]
fn linked_owner_with_newline_is_used_for_admission() {
    let (fixture, host) = Fixture::hosted();
    let owner = head(&fixture.repo);
    git(&fixture.repo, &["switch", "-c", "other"]);
    let linked = fixture.root.path().join("base\ncheckout");
    git(&fixture.repo, &["worktree", "add", path_str(&linked), BASE]);
    let origin = push_to_origin_from_elsewhere(&fixture, "newline-owner.txt");
    fs::write(linked.join("README.md"), "owner edit\n").unwrap();
    host.add_task("T-NEWLINE-DIRTY", TaskStatus::Backlog);
    let error = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-NEWLINE-DIRTY"], "jrun-newline-dirty"),
    )
    .expect_err("the actual linked owner is dirty");
    assert_owner_base_refusal(&error, &linked, &owner, &origin);
    assert!(host.admitted().is_empty());
    git(&linked, &["restore", "README.md"]);
    host.add_task("T-NEWLINE-CLEAN", TaskStatus::Backlog);
    let output = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-NEWLINE-CLEAN"], "jrun-newline-clean"),
    )
    .expect("the clean linked owner is admitted");
    assert_eq!(output["base_sha"], origin);
    assert_eq!(head(&Checkout::from_setup(&output).path), origin);
    assert_eq!(head(&linked), owner);
    assert_eq!(host.admitted().len(), 1);
}

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
    for fragment in [
        checkout.as_str(),
        owner,
        origin,
        "refusing stale base",
        "Reconcile the checkout with origin",
    ] {
        assert!(
            message.contains(fragment),
            "the refusal must name {fragment}: {message}"
        );
    }
}

/// Behind and clean: the run starts at origin while the owner stays intact.
fn a_clean_owner_behind_origin_starts_at_origin_without_moving_owner() {
    let (fixture, host) = Fixture::hosted();
    let owner_before = head(&fixture.repo);
    let origin_tip = push_to_origin_from_elsewhere(&fixture, "from-elsewhere.txt");
    assert_ne!(owner_before, origin_tip, "origin moved ahead of the owner");
    let index_before = git(&fixture.repo, &["ls-files", "--stage"]);
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
    assert_eq!(head(&fixture.repo), owner_before, "owner ref is unchanged");
    assert!(!fixture.repo.join("from-elsewhere.txt").exists());
    assert_eq!(git(&fixture.repo, &["ls-files", "--stage"]), index_before);
    assert_eq!(host.admitted().len(), 1, "the clean owner admits the task");
}

/// A selected branch named `origin/train` refreshes that exact remote ref,
/// even when a separate `train` branch has a different tip.
fn a_literal_origin_prefixed_base_fetches_its_exact_remote_branch() {
    let (fixture, host) = Fixture::hosted();
    let owner_tip = head(&fixture.repo);
    git(&fixture.repo, &["branch", "origin/train", &owner_tip]);
    git(
        &fixture.repo,
        &[
            "push",
            "origin",
            "refs/heads/origin/train:refs/heads/origin/train",
        ],
    );

    git(&fixture.repo, &["branch", "train", &owner_tip]);
    git(&fixture.repo, &["switch", "train"]);
    let train_tip = commit_file(&fixture.repo, "train-only.txt", "decoy train branch\n");
    git(&fixture.repo, &["push", "origin", "train"]);
    git(&fixture.repo, &["switch", BASE]);

    let origin = fixture.root.path().join("origin.git");
    let elsewhere = fixture.root.path().join("elsewhere-origin-train");
    git(
        fixture.root.path(),
        &[
            "clone",
            "--branch",
            "origin/train",
            path_str(&origin),
            path_str(&elsewhere),
        ],
    );
    git(&elsewhere, &["config", "user.name", "Orbit Test"]);
    git(
        &elsewhere,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    let literal_origin_tip = commit_file(
        &elsewhere,
        "literal-origin-train.txt",
        "literal origin/train branch\n",
    );
    git(
        &elsewhere,
        &["push", "origin", "HEAD:refs/heads/origin/train"],
    );

    host.add_task("T-ORIGIN-TRAIN", TaskStatus::Backlog);
    let mut input = setup_input(&["T-ORIGIN-TRAIN"], "jrun-origin-train");
    input["base"] = json!("origin/origin/train");
    let output = action(&host, "worktree_setup", &input)
        .expect("the literal origin/train base is fetched and admitted");

    assert_eq!(output["base_sha"], literal_origin_tip);
    assert_eq!(
        head(&Checkout::from_setup(&output).path),
        literal_origin_tip,
        "the run starts from the advanced literal origin/train branch"
    );
    assert_ne!(literal_origin_tip, train_tip, "the decoy train tip differs");
    assert_eq!(
        git(
            &fixture.repo,
            &["rev-parse", "refs/remotes/origin/origin/train"]
        ),
        literal_origin_tip,
        "fetch refreshes the exact remote-tracking ref"
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "refs/remotes/origin/train"]),
        train_tip,
        "the separate train branch remains a distinct ref"
    );
    assert_eq!(
        head(&fixture.repo),
        owner_tip,
        "the owner branch is unchanged"
    );
}

/// A large set of incoming paths must not be captured during admission.
fn a_large_clean_advance_is_admitted() {
    let (fixture, host) = Fixture::hosted();
    let owner = head(&fixture.repo);
    let origin = fixture.root.path().join("origin.git");
    let elsewhere = fixture.root.path().join("large-advance");
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

    // Keep each component portable while making the old NUL-delimited Git
    // output exceed the process supervisor's 1 MiB capture limit.
    let prefix = (0..5)
        .map(|n| format!("{n}{}", "x".repeat(99)))
        .collect::<Vec<_>>()
        .join("/");
    let directory = elsewhere.join(&prefix);
    fs::create_dir_all(&directory).unwrap();
    for n in 0..2100 {
        fs::write(directory.join(format!("file-{n:04}")), "").unwrap();
    }
    git(&elsewhere, &["add", "--", &prefix]);
    git(&elsewhere, &["commit", "-m", "add many paths"]);
    let tip = head(&elsewhere);
    let added = crate::git_test_support::command()
        .current_dir(&elsewhere)
        .args([
            "diff",
            "--name-only",
            "--no-renames",
            "--diff-filter=A",
            "-z",
            &owner,
            &tip,
        ])
        .output()
        .unwrap();
    assert!(added.status.success());
    assert!(
        added.stdout.len() > 1024 * 1024,
        "fixture must cross the capture limit: {}",
        added.stdout.len()
    );
    assert!(
        !fixture.repo.join(&prefix).exists(),
        "no incoming path exists in the owner"
    );
    git(&elsewhere, &["push", "origin", BASE]);

    host.add_task("T-LARGE", TaskStatus::Backlog);
    let output = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-LARGE"], "jrun-large"),
    )
    .expect("a clean owner admits a large incoming change");
    assert_eq!(output["base_sha"], tip);
    assert_eq!(head(&Checkout::from_setup(&output).path), tip);
    assert_eq!(head(&fixture.repo), owner);
    assert!(!fixture.repo.join(&prefix).exists());
    assert_eq!(host.admitted().len(), 1);
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

/// Diverged and checked out nowhere: the repository's own checkout sits on
/// another branch. The refusal names the selected branch and both commits, says
/// no checkout holds it, and never names the unrelated checkout.
fn a_diverged_base_checked_out_nowhere_names_the_branch_not_a_checkout() {
    let (fixture, host) = Fixture::hosted();
    let owner = commit_file(&fixture.repo, "owner-only.txt", "owner commit\n");
    git(&fixture.repo, &["switch", "-c", "other"]);
    let origin = push_to_origin_from_elsewhere(&fixture, "from-elsewhere.txt");
    let checkouts = registered_worktrees(&fixture.repo);
    host.add_task("T-UNATTACHED", TaskStatus::Backlog);
    let error = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-UNATTACHED"], "jrun-unattached"),
    )
    .expect_err("a diverged base that no checkout holds is refused");
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
    let message = error.to_string();
    for fragment in [BASE, owner.as_str(), origin.as_str(), "no checkout holds"] {
        assert!(
            message.contains(fragment),
            "the refusal must name {fragment}: {message}"
        );
    }
    let unrelated = fixture.repo.display().to_string();
    assert!(
        !message.contains(&unrelated),
        "the unrelated checkout must not be named as the holder: {message}"
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", BASE]),
        owner,
        "the diverged base is not moved"
    );
    assert_eq!(git(&fixture.repo, &["branch", "--show-current"]), "other");
    assert_eq!(git(&fixture.repo, &["status", "--porcelain"]), "");
    assert_eq!(
        registered_worktrees(&fixture.repo),
        checkouts,
        "a refused setup creates no worktree"
    );
    assert!(host.admitted().is_empty(), "a refused setup admits no task");
}

/// Diverged and attached to a linked checkout while the repository's own
/// checkout sits on another branch: the refusal names that linked checkout.
fn a_diverged_base_attached_elsewhere_names_its_real_worktree() {
    let (fixture, host) = Fixture::hosted();
    let owner = commit_file(&fixture.repo, "owner-only.txt", "owner commit\n");
    git(&fixture.repo, &["switch", "-c", "other"]);
    let linked = fixture.root.path().join("linked-base");
    git(&fixture.repo, &["worktree", "add", path_str(&linked), BASE]);
    let linked = canonical(&linked);
    let origin = push_to_origin_from_elsewhere(&fixture, "from-elsewhere.txt");
    host.add_task("T-ATTACHED-ELSEWHERE", TaskStatus::Backlog);
    let error = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-ATTACHED-ELSEWHERE"], "jrun-attached-elsewhere"),
    )
    .expect_err("a diverged base attached to a linked checkout is refused");
    assert_owner_base_refusal(&error, &linked, &owner, &origin);
    assert_eq!(head(&linked), owner, "the linked checkout is not moved");
    assert_eq!(git(&fixture.repo, &["branch", "--show-current"]), "other");
    assert_eq!(git(&fixture.repo, &["status", "--porcelain"]), "");
    assert!(host.admitted().is_empty(), "a refused setup admits no task");
}

/// Ignore `file` in the owner without committing a `.gitignore` change, and
/// give it unpublished bytes.
fn ignore_owner_file(fixture: &Fixture, file: &str, contents: &str) {
    let exclude = fixture.repo.join(".git").join("info").join("exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    let mut rules = fs::read_to_string(&exclude).unwrap_or_default();
    rules.push_str(&format!("/{file}\n"));
    fs::write(&exclude, rules).unwrap();
    fs::write(fixture.repo.join(file), contents).unwrap();
}

/// An ignored file at a path newly tracked by origin stays untouched.
fn an_ignored_owner_file_at_a_new_origin_path_is_preserved() {
    let (fixture, host) = Fixture::hosted();
    let owner = head(&fixture.repo);
    ignore_owner_file(&fixture, "local-notes.txt", "unpublished bytes\n");
    let origin = push_to_origin_from_elsewhere(&fixture, "local-notes.txt");
    let index = git(&fixture.repo, &["ls-files", "--stage"]);
    host.add_task("T-IGNORED", TaskStatus::Backlog);
    let output = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-IGNORED"], "jrun-ignored"),
    )
    .unwrap();
    assert_eq!(output["base_sha"], origin);
    assert_eq!(head(&fixture.repo), owner);
    assert_eq!(git(&fixture.repo, &["ls-files", "--stage"]), index);
    assert_eq!(
        read(fixture.repo.join("local-notes.txt")),
        "unpublished bytes\n"
    );
}

/// Clone origin elsewhere, let `prepare` stage a change there, then push it.
/// Returns the new origin tip.
fn push_staged_change_to_origin(
    fixture: &Fixture,
    name: &str,
    prepare: impl FnOnce(&Path),
) -> String {
    let origin = fixture.root.path().join("origin.git");
    let elsewhere = fixture.root.path().join(format!("elsewhere-{name}"));
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
    prepare(&elsewhere);
    git(&elsewhere, &["commit", "-m", &format!("stage {name}")]);
    git(&elsewhere, &["push", "origin", BASE]);
    git(&elsewhere, &["rev-parse", "HEAD"])
}

/// Admit from `origin` while the ignored owner file `file` stays byte-identical
/// and the owner branch and index stay put.
fn assert_admission_preserves_owner_file(
    fixture: &Fixture,
    host: &LifecycleHost,
    file: &str,
    origin: &str,
) {
    let owner = head(&fixture.repo);
    let index = git(&fixture.repo, &["ls-files", "--stage"]);
    host.add_task("T-IGNORED-COLLISION", TaskStatus::Backlog);
    let output = action(
        host,
        "worktree_setup",
        &setup_input(&["T-IGNORED-COLLISION"], "jrun-ignored-collision"),
    )
    .expect("an ignored owner file never blocks admission from an immutable commit");
    assert_eq!(output["base_sha"], origin);
    assert_eq!(head(&Checkout::from_setup(&output).path), origin);
    assert_eq!(head(&fixture.repo), owner, "owner ref is unchanged");
    assert_eq!(git(&fixture.repo, &["ls-files", "--stage"]), index);
    assert_eq!(
        read(fixture.repo.join(file)),
        "unpublished bytes\n",
        "the ignored owner file is byte-identical"
    );
}

/// An ignored owner file that is an ancestor of a path origin adds stays put.
fn an_ignored_owner_file_at_an_incoming_ancestor_path_is_preserved() {
    let (fixture, host) = Fixture::hosted();
    ignore_owner_file(&fixture, "dir", "unpublished bytes\n");
    let origin = push_staged_change_to_origin(&fixture, "ancestor", |clone| {
        fs::create_dir_all(clone.join("dir")).unwrap();
        fs::write(clone.join("dir/new"), "published\n").unwrap();
        git(clone, &["add", "dir/new"]);
    });
    assert_admission_preserves_owner_file(&fixture, &host, "dir", &origin);
}

/// An ignored regular owner file at a path origin adds as a gitlink stays put,
/// even though Orbit's git flags hide submodule differences.
fn an_ignored_owner_file_at_a_new_gitlink_path_is_preserved() {
    let (fixture, host) = Fixture::hosted();
    ignore_owner_file(&fixture, "sub", "unpublished bytes\n");
    let origin = push_staged_change_to_origin(&fixture, "gitlink", |clone| {
        let commit = git(clone, &["rev-parse", "HEAD"]);
        git(
            clone,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{commit},sub"),
            ],
        );
    });
    assert_admission_preserves_owner_file(&fixture, &host, "sub", &origin);
}

/// An ignored owner file at an unrelated path stays byte-identical and does not
/// stop admission.
fn an_unrelated_ignored_owner_file_does_not_block_admission() {
    let (fixture, host) = Fixture::hosted();
    ignore_owner_file(&fixture, "local-notes.txt", "unpublished bytes\n");
    let origin_tip = push_to_origin_from_elsewhere(&fixture, "from-elsewhere.txt");
    host.add_task("T-UNRELATED", TaskStatus::Backlog);
    let output = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-UNRELATED"], "jrun-unrelated"),
    )
    .expect("an unrelated ignored file does not block admission");
    assert_eq!(output["base_sha"], origin_tip, "the run is cut from origin");
    assert_ne!(head(&fixture.repo), origin_tip, "the owner is unchanged");
    assert_eq!(
        read(fixture.repo.join("local-notes.txt")),
        "unpublished bytes\n",
        "the unrelated ignored file is byte-identical"
    );
}

fn landing_preserves_ignored_owner_files() {
    for scenario in ["origin", "candidate", "unrelated"] {
        let (fixture, host) = Fixture::hosted();
        let owner = head(&fixture.repo);
        ignore_owner_file(&fixture, "local-notes.txt", "unpublished bytes\n");
        let incoming = if scenario == "origin" {
            "local-notes.txt"
        } else {
            "from-elsewhere.txt"
        };
        let origin = push_to_origin_from_elsewhere(&fixture, incoming);
        host.add_task("T-LANDING-IGNORED", TaskStatus::Backlog);
        let run_id = "jrun-landing-ignored";
        let output = action(
            &host,
            "worktree_setup",
            &setup_input(&["T-LANDING-IGNORED"], run_id),
        )
        .unwrap();
        assert_eq!(output["base_sha"], origin);
        let checkout = Checkout::from_setup(&output);
        if scenario == "candidate" {
            commit_tracked_record(&checkout.path, "local-notes.txt", "candidate bytes\n");
        }
        let index = git(&fixture.repo, &["ls-files", "--stage"]);
        let result = merge(&host, run_id, BASE, &checkout.path);
        if scenario == "unrelated" {
            result.expect("unrelated ignored file must not block landing");
            assert_eq!(head(&fixture.repo), head(&checkout.path));
        } else {
            let error = result.expect_err("conflicting ignored file must refuse landing");
            assert!(error.to_string().contains("local-notes.txt"), "{error}");
            assert_eq!(head(&fixture.repo), owner);
            assert_eq!(git(&fixture.repo, &["ls-files", "--stage"]), index);
        }
        assert_eq!(
            read(fixture.repo.join("local-notes.txt")),
            "unpublished bytes\n"
        );
    }
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

/// Equal and locally ahead bases retain the owner's commit.
fn equal_and_ahead_owner_bases_are_used() {
    for ahead in [false, true] {
        let (fixture, host) = Fixture::hosted();
        let owner = if ahead {
            commit_file(&fixture.repo, "owner-only.txt", "owner commit\n")
        } else {
            head(&fixture.repo)
        };
        host.add_task("T-OWNER", TaskStatus::Backlog);
        let output = action(
            &host,
            "worktree_setup",
            &setup_input(&["T-OWNER"], "jrun-owner"),
        )
        .unwrap();
        assert_eq!(output["base_sha"], owner);
        assert_eq!(head(&fixture.repo), owner);
        assert_eq!(head(&Checkout::from_setup(&output).path), owner);
    }
}

// Intercept the first holder listing after Git has produced it, then change
// attachment before returning the observed listing to setup.
#[cfg(unix)]
fn attachment_changes_after_holder_observation() {
    for attach_base in [false, true] {
        let (fixture, host) = Fixture::hosted();
        let owner = head(&fixture.repo);
        git(&fixture.repo, &["switch", "-c", "other"]);
        commit_file(&fixture.repo, "other.txt", "other bytes\n");
        if !attach_base {
            git(&fixture.repo, &["switch", BASE]);
        }
        let origin = push_to_origin_from_elsewhere(&fixture, "remote.txt");
        let bin = fixture.root.path().join("git-wrapper");
        fs::create_dir_all(&bin).unwrap();
        let wrapper = bin.join("git");
        let marker = fixture.root.path().join("listing-seen");
        let target = if attach_base { BASE } else { "other" };
        let script = format!(
            "#!/bin/sh\ncase \" $* \" in\n  *' worktree list --porcelain -z '*)\n    if [ ! -e '{}' ]; then\n      /usr/bin/git \"$@\"\n      touch '{}'\n      /usr/bin/git switch '{}' >&2 || exit $?\n      exit 0\n    fi;;\nesac\nexec /usr/bin/git \"$@\"\n",
            marker.display(),
            marker.display(),
            target
        );
        fs::write(&wrapper, script).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let old_path = std::env::var_os("PATH").unwrap();
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(&old_path));
        // This family runs in an isolated process.
        unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
        host.add_task("T-RACE", TaskStatus::Backlog);
        let output = action(
            &host,
            "worktree_setup",
            &setup_input(&["T-RACE"], "jrun-race"),
        )
        .unwrap();
        unsafe { std::env::set_var("PATH", old_path) };
        assert!(marker.exists(), "holder listing was intercepted");
        assert_eq!(output["base_sha"], origin);
        assert_eq!(git(&fixture.repo, &["rev-parse", BASE]), owner);
        assert_eq!(git(&fixture.repo, &["branch", "--show-current"]), target);
        assert_eq!(git(&fixture.repo, &["status", "--porcelain"]), "");
        assert_eq!(fixture.repo.join("other.txt").exists(), !attach_base);
        assert!(!fixture.repo.join("remote.txt").exists());
        assert_eq!(head(&Checkout::from_setup(&output).path), origin);
    }
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn local_base_freshness_contracts() {
    isolated_in(module_path!(), "local_base_freshness_contracts", || {
        #[cfg(unix)]
        attachment_changes_after_holder_observation();
        #[cfg(unix)]
        linked_owner_with_newline_is_used_for_admission();
        equal_and_ahead_owner_bases_are_used();
        crate::git_fixture::run_scenarios(&[
            (
                "a_clean_owner_behind_origin_starts_at_origin_without_moving_owner",
                a_clean_owner_behind_origin_starts_at_origin_without_moving_owner,
            ),
            (
                "a_literal_origin_prefixed_base_fetches_its_exact_remote_branch",
                a_literal_origin_prefixed_base_fetches_its_exact_remote_branch,
            ),
            (
                "a_large_clean_advance_is_admitted",
                a_large_clean_advance_is_admitted,
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
                "a_diverged_base_checked_out_nowhere_names_the_branch_not_a_checkout",
                a_diverged_base_checked_out_nowhere_names_the_branch_not_a_checkout,
            ),
            (
                "a_diverged_base_attached_elsewhere_names_its_real_worktree",
                a_diverged_base_attached_elsewhere_names_its_real_worktree,
            ),
            (
                "an_ignored_owner_file_at_a_new_origin_path_is_preserved",
                an_ignored_owner_file_at_a_new_origin_path_is_preserved,
            ),
            (
                "an_ignored_owner_file_at_an_incoming_ancestor_path_is_preserved",
                an_ignored_owner_file_at_an_incoming_ancestor_path_is_preserved,
            ),
            (
                "an_ignored_owner_file_at_a_new_gitlink_path_is_preserved",
                an_ignored_owner_file_at_a_new_gitlink_path_is_preserved,
            ),
            (
                "an_unrelated_ignored_owner_file_does_not_block_admission",
                an_unrelated_ignored_owner_file_does_not_block_admission,
            ),
            (
                "landing_preserves_ignored_owner_files",
                landing_preserves_ignored_owner_files,
            ),
            (
                "local_setup_keeps_the_owner_base_when_origin_is_absent_or_unreachable",
                local_setup_keeps_the_owner_base_when_origin_is_absent_or_unreachable,
            ),
        ]);
    });
}
