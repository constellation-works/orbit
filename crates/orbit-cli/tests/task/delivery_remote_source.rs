//! Delivery observation follows `origin/<branch>` [ORB-14118].
//!
//! A pull request merged on the remote never moves the checkout's local
//! branch. The consumer has to fetch that one ref and read the remote-tracking
//! head. These fixtures keep the objects on a local bare repo so the pass is
//! the real fetch, not a stubbed revision.

use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::Utc;
use orbit_core::application::automation::evaluate_auto_task;
use serde_json::{Value, json};

use crate::auto_task_lifecycle_cli::{git, publish_origin_if_configured};
use crate::isolated_cli_fixture::Fixture;

const BRANCH: &str = "fixture-delivery";
const REPOSITORY: &str = "owner/repository";
const CONSUMER: &str = "remote-deliveries";
const REVIEW_CONSUMER: &str = "delivery-code-review";
const REVIEW_CREW: &str = "sonnet";

fn in_isolated_child(test: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_DELIVERY_REMOTE_CHILD";
    if std::env::var(CHILD).ok().as_deref() == Some(test) {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    false
}

fn open_runtime(fixture: &Fixture) -> orbit_core::OrbitRuntime {
    use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
    use orbit_core::ActorIdentity;

    let roots = RegisteredRuntimeFactory::resolve_roots_for_cwd(&fixture.repo, Some(&fixture.root))
        .unwrap();
    assert!(roots.global_root.starts_with(fixture._temp.path()));
    RegisteredRuntimeFactory::open_resolved_roots(roots)
        .unwrap()
        .with_actor(ActorIdentity::human("fixture"))
}

fn git_at(dir: &Path, home: &Path, args: &[&str]) -> String {
    git_at_env(dir, home, args, &[])
}

fn git_at_env(dir: &Path, home: &Path, args: &[&str], extra: &[(&str, &str)]) -> String {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM");
    for (key, value) in extra {
        command.env(key, value);
    }
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).trim().to_string()
}

fn trigger(max_wait_minutes: u32) -> Value {
    json!({
        "branch": BRANCH,
        "threshold": 1,
        "max_wait_minutes": max_wait_minutes,
        "coverage": "landed_code_review_v1",
        "max_items": 20,
        "retries": 0,
    })
}

fn add_github_origin(fixture: &Fixture) {
    git(
        fixture,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://github.com/{REPOSITORY}.git"),
        ],
    );
}

/// Baselined delivery consumer whose origin is a bare repo reached through
/// the GitHub URL. The local branch is the baseline commit.
fn baselined_remote_consumer(fixture: &Fixture) -> orbit_core::OrbitRuntime {
    git(fixture, &["checkout", "-b", BRANCH]);
    fs::write(fixture.repo.join("fixture.txt"), "baseline\n").unwrap();
    git(fixture, &["add", "fixture.txt"]);
    git(fixture, &["commit", "-m", "Disposable baseline"]);
    add_github_origin(fixture);
    publish_origin_if_configured(fixture);
    fixture.json(&[
        "auto-task",
        "add",
        "--name",
        CONSUMER,
        "--deliveries-landed",
        &trigger(60).to_string(),
        "--title",
        "Review remote deliveries",
        "--json",
    ]);
    fixture.json(&["auto-task", "toggle", CONSUMER, "on", "--json"]);
    let runtime = open_runtime(fixture);
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    evaluate_auto_task(&runtime, &definition, false, Utc::now()).expect("baseline");
    runtime
}

fn observed_commit(runtime: &orbit_core::OrbitRuntime) -> String {
    let consumer =
        orbit_core::application::automation::consumer_key(runtime, "auto-task", CONSUMER).unwrap();
    runtime
        .automation_store()
        .unwrap()
        .automation_state(&consumer)
        .unwrap()
        .expect("baselined state")
        .observed
        .commit
}

fn local_branch(fixture: &Fixture) -> String {
    git(fixture, &["rev-parse", &format!("refs/heads/{BRANCH}")])
}

fn install_pull(fixture: &Fixture, sha: &str, pr: u64) {
    let pulls = fixture._temp.path().join("pulls");
    fs::create_dir_all(&pulls).unwrap();
    let response = json!([{
        "number": pr,
        "html_url": format!("https://github.com/{REPOSITORY}/pull/{pr}"),
        "merge_commit_sha": sha,
        "merged_at": "2026-10-04T00:00:00Z",
        "base": {"ref": BRANCH, "repo": {"full_name": REPOSITORY}},
    }]);
    fs::write(pulls.join(format!("{sha}.json")), response.to_string()).unwrap();
    let bin = fixture._temp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\nsha=${{2#*/commits/}}\nexec cat \"{}/pulls/${{sha%%/*}}.json\"\n",
        fixture._temp.path().display()
    );
    let gh = bin.join("gh");
    fs::write(&gh, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn gh_path(fixture: &Fixture) -> String {
    let bin = fixture._temp.path().join("bin");
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap()
    .to_string_lossy()
    .into_owned()
}

fn publisher(fixture: &Fixture) -> std::path::PathBuf {
    let bare = fixture.repo.with_file_name("origin.git");
    let publisher = fixture._temp.path().join("publisher");
    git_at(
        &fixture.repo,
        &fixture.home,
        &[
            "clone",
            "-q",
            &bare.display().to_string(),
            &publisher.display().to_string(),
        ],
    );
    publisher
}

/// A commit that lands only on the bare remote mints a batch, and the
/// checkout's branch, index and worktree stay where they were.
#[test]
fn remote_only_landing_mints_a_batch_without_moving_the_checkout() {
    const TEST: &str =
        "delivery_remote_source::remote_only_landing_mints_a_batch_without_moving_the_checkout";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let runtime = baselined_remote_consumer(&fixture);
    let local = local_branch(&fixture);
    fs::write(fixture.repo.join("fixture.txt"), "dirty local edit\n").unwrap();
    // `git` trims, so porcelain's leading space is not part of this snapshot.
    // Equality before and after is what shows observation left the index and
    // worktree alone. An empty cached diff is the unstaged edit.
    let status = git(&fixture, &["status", "--porcelain"]);
    let cached = git(&fixture, &["diff", "--cached", "--name-only"]);
    let unstaged = git(&fixture, &["diff", "--name-only"]);
    assert!(cached.is_empty(), "the fixture stages nothing: {cached}");
    assert_eq!(unstaged, "fixture.txt", "{unstaged}");

    let publisher = publisher(&fixture);
    fs::write(publisher.join("fixture.txt"), "landed on the remote only\n").unwrap();
    git_at(
        &publisher,
        &fixture.home,
        &["commit", "-am", "Squash-merge #42"],
    );
    let remote_sha = git_at(&publisher, &fixture.home, &["rev-parse", "HEAD"]);
    git_at(
        &publisher,
        &fixture.home,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")],
    );
    install_pull(&fixture, &remote_sha, 42);
    let path = gh_path(&fixture);
    let _gh = orbit_common::test_env::scoped([("PATH", Some(path.as_str()))]);

    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    let diagnostic =
        evaluate_auto_task(&runtime, &definition, false, Utc::now()).expect("remote observation");
    let state = diagnostic
        .state
        .clone()
        .unwrap_or_else(|| panic!("no state: {diagnostic:#?}"));
    let batch = state
        .active
        .as_ref()
        .unwrap_or_else(|| panic!("no batch minted: {state:#?}"))
        .batch
        .clone();
    assert!(
        batch.commits.contains(&remote_sha),
        "the remote commit is the delivery: {batch:#?}"
    );
    assert_eq!(batch.through_inclusive.commit, remote_sha);
    assert_eq!(
        local_branch(&fixture),
        local,
        "observation moved the local branch"
    );
    assert_eq!(git(&fixture, &["status", "--porcelain"]), status);
    assert_eq!(git(&fixture, &["diff", "--cached", "--name-only"]), cached);
    assert_eq!(git(&fixture, &["diff", "--name-only"]), unstaged);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("fixture.txt")).unwrap(),
        "dirty local edit\n"
    );
    assert_eq!(
        git(
            &fixture,
            &["rev-parse", &format!("refs/remotes/origin/{BRANCH}")]
        ),
        remote_sha
    );
    assert_ne!(remote_sha, local);
}

/// A broken origin defers the pass and leaves the cursor on the last observed
/// remote head, including when the local branch has since moved.
#[test]
fn fetch_failure_defers_without_falling_back_to_the_local_branch() {
    const TEST: &str =
        "delivery_remote_source::fetch_failure_defers_without_falling_back_to_the_local_branch";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let runtime = baselined_remote_consumer(&fixture);
    let observed = observed_commit(&runtime);
    let bare = fixture.repo.with_file_name("origin.git");
    let key = format!("url.{}.insteadOf", bare.display());
    git(&fixture, &["config", "--unset", &key]);
    git(
        &fixture,
        &["remote", "set-url", "origin", "/no/such/remote.git"],
    );
    fs::write(fixture.repo.join("fixture.txt"), "local only\n").unwrap();
    git(&fixture, &["commit", "-am", "Not on the remote"]);
    let local = local_branch(&fixture);
    assert_ne!(local, observed);

    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    let error = evaluate_auto_task(&runtime, &definition, false, Utc::now())
        .expect_err("a failed fetch defers");
    assert!(error.to_string().contains("source_fetch_failed"), "{error}");
    assert_eq!(
        observed_commit(&runtime),
        observed,
        "a failed fetch must not advance observation onto the local branch"
    );
}

/// A force-push still fails the ancestry check against the fetched head.
#[test]
fn force_pushed_remote_history_defers_as_history_diverged() {
    const TEST: &str =
        "delivery_remote_source::force_pushed_remote_history_defers_as_history_diverged";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let runtime = baselined_remote_consumer(&fixture);
    let observed = observed_commit(&runtime);
    let local = local_branch(&fixture);
    let publisher = publisher(&fixture);
    git_at(
        &publisher,
        &fixture.home,
        &["checkout", "--orphan", "diverged"],
    );
    git_at(
        &publisher,
        &fixture.home,
        &["commit", "--allow-empty", "-m", "unrelated history"],
    );
    git_at(
        &publisher,
        &fixture.home,
        &[
            "push",
            "--force",
            "-q",
            "origin",
            &format!("HEAD:refs/heads/{BRANCH}"),
        ],
    );

    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    match evaluate_auto_task(&runtime, &definition, false, Utc::now()) {
        Ok(diagnostic) => {
            assert!(
                diagnostic.reason.contains("history_diverged"),
                "{diagnostic:#?}"
            );
            let state = diagnostic.state.expect("state");
            assert_eq!(state.observed.commit, observed);
        }
        Err(error) => {
            assert!(error.to_string().contains("history_diverged"), "{error}");
            assert_eq!(observed_commit(&runtime), observed);
        }
    }
    assert_eq!(local_branch(&fixture), local);
}

fn enable_review_crew(fixture: &Fixture) {
    for (key, value) in [
        ("crews.sonnet.enabled", "true"),
        ("workflow.default_crew", REVIEW_CREW),
        ("workflow.system_crew", REVIEW_CREW),
    ] {
        fixture
            .command(&["config", "set", "--global", key, value])
            .assert()
            .success();
    }
}

fn retarget_review(fixture: &Fixture, max_wait_minutes: u32) {
    fixture.json(&[
        "auto-task",
        "update",
        REVIEW_CONSUMER,
        "--deliveries-landed",
        &trigger(max_wait_minutes).to_string(),
        "--json",
    ]);
}

fn doctor_review(fixture: &Fixture) -> Value {
    let output = fixture.command(&["doctor", "--json"]).output().unwrap();
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row["check"] == "review")
        .cloned()
        .unwrap_or_else(|| panic!("no review row: {rows}"))
}

fn baseline_review_consumer(fixture: &Fixture, max_wait_minutes: u32) -> String {
    enable_review_crew(fixture);
    git(fixture, &["checkout", "-b", BRANCH]);
    fs::write(fixture.repo.join("fixture.txt"), "baseline\n").unwrap();
    git(fixture, &["add", "fixture.txt"]);
    git(fixture, &["commit", "-m", "Disposable baseline"]);
    retarget_review(fixture, max_wait_minutes);
    fixture
        .command(&["auto-task", "toggle", REVIEW_CONSUMER, "on"])
        .assert()
        .success();
    let runtime = open_runtime(fixture);
    let definition = runtime.auto_task_show(REVIEW_CONSUMER).unwrap().unwrap();
    evaluate_auto_task(&runtime, &definition, false, Utc::now()).expect("baseline");
    local_branch(fixture)
}

fn bare_origin(fixture: &Fixture) -> std::path::PathBuf {
    let bare = fixture.repo.with_file_name("origin.git");
    git_at(
        &fixture.repo,
        &fixture.home,
        &["init", "--bare", "-q", &bare.display().to_string()],
    );
    git_at(
        &bare,
        &fixture.home,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{BRANCH}")],
    );
    git(
        fixture,
        &["remote", "add", "origin", &bare.display().to_string()],
    );
    bare
}

fn fetch_remote_tracking(fixture: &Fixture) {
    git(
        fixture,
        &[
            "fetch",
            "origin",
            &format!("+refs/heads/{BRANCH}:refs/remotes/origin/{BRANCH}"),
        ],
    );
}

/// Doctor names the remote-tracking head. A missing ref is not ok once the
/// consumer has a cursor. A trail younger than `max_wait_minutes` is reported
/// and still ok. The local branch does not move.
#[test]
fn doctor_reports_the_remote_tracking_head_without_fetching() {
    const TEST: &str =
        "delivery_remote_source::doctor_reports_the_remote_tracking_head_without_fetching";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let local = baseline_review_consumer(&fixture, 60);
    let row = doctor_review(&fixture);
    assert_eq!(row["status"], "ok", "{row}");
    let message = row["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("branch `{BRANCH}` resolves")),
        "{message}"
    );
    assert!(!message.contains("origin/"), "{message}");

    let bare = bare_origin(&fixture);
    let row = doctor_review(&fixture);
    assert_eq!(row["status"], "error", "{row}");
    assert!(
        row["message"]
            .as_str()
            .unwrap()
            .contains("has no remote-tracking ref"),
        "{row}"
    );

    git(
        &fixture,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")],
    );
    fetch_remote_tracking(&fixture);
    let row = doctor_review(&fixture);
    assert_eq!(row["status"], "ok", "{row}");
    assert!(
        row["message"]
            .as_str()
            .unwrap()
            .contains(&format!("matches origin/{BRANCH}")),
        "{row}"
    );

    let publisher = fixture._temp.path().join("publisher");
    git_at(
        &fixture.repo,
        &fixture.home,
        &[
            "clone",
            "-q",
            &bare.display().to_string(),
            &publisher.display().to_string(),
        ],
    );
    fs::write(publisher.join("fixture.txt"), "landed remotely\n").unwrap();
    git_at(
        &publisher,
        &fixture.home,
        &["commit", "-am", "Remote landing"],
    );
    git_at(
        &publisher,
        &fixture.home,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")],
    );
    fetch_remote_tracking(&fixture);
    assert_eq!(local_branch(&fixture), local);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("fixture.txt")).unwrap(),
        "baseline\n"
    );

    let row = doctor_review(&fixture);
    assert_eq!(row["status"], "ok", "{row}");
    let message = row["message"].as_str().unwrap();
    assert!(message.contains("trails"), "{message}");
    assert!(message.contains("by 1"), "{message}");
    assert_eq!(
        row["status"], "ok",
        "a fresh trail is inside max_wait_minutes: {row}"
    );
}

/// A remote landing older than `max_wait_minutes` is not a healthy consumer.
#[test]
fn doctor_is_not_ok_when_the_remote_trail_exceeds_max_wait() {
    const TEST: &str =
        "delivery_remote_source::doctor_is_not_ok_when_the_remote_trail_exceeds_max_wait";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let local = baseline_review_consumer(&fixture, 60);
    let bare = bare_origin(&fixture);
    git(
        &fixture,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")],
    );
    let publisher = fixture._temp.path().join("publisher");
    git_at(
        &fixture.repo,
        &fixture.home,
        &[
            "clone",
            "-q",
            &bare.display().to_string(),
            &publisher.display().to_string(),
        ],
    );
    fs::write(publisher.join("fixture.txt"), "landed long ago\n").unwrap();
    git_at_env(
        &publisher,
        &fixture.home,
        &["commit", "-am", "Old remote landing"],
        &[
            ("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z"),
            ("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z"),
        ],
    );
    git_at(
        &publisher,
        &fixture.home,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")],
    );
    fetch_remote_tracking(&fixture);
    assert_eq!(local_branch(&fixture), local);

    let row = doctor_review(&fixture);
    assert_eq!(row["status"], "error", "{row}");
    let message = row["message"].as_str().unwrap();
    assert!(message.contains("max_wait_minutes"), "{message}");
    assert!(message.contains("trails"), "{message}");
}
