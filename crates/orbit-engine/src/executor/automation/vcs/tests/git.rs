#![allow(missing_docs)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

use orbit_exec::EnvironmentMode;
use serde_json::json;

use super::super::git::{
    BaseSyncMode, GitTimeoutBudget, fetch_remote_base, git_command_success, git_request, git_run,
    resolve_worktree_start_point,
};
#[cfg(unix)]
use super::with_fake_git;

#[test]
fn remote_mode_fetches_origin_base_when_local_base_is_stale() {
    let temp = tempdir().unwrap();
    let remote = temp.path().join("remote.git");
    let seed = temp.path().join("seed");
    let local = temp.path().join("local");

    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    init_repo(&seed, "agent-main");
    let local_v1 = commit_file(&seed, "base.txt", "v1");
    git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&seed, &["push", "-u", "origin", "agent-main"]);

    git(
        temp.path(),
        &[
            "clone",
            "--branch",
            "agent-main",
            remote.to_str().unwrap(),
            local.to_str().unwrap(),
        ],
    );

    let remote_v2 = commit_file(&seed, "base.txt", "v2");
    git(&seed, &["push", "origin", "agent-main"]);

    assert_eq!(git(&local, &["rev-parse", "agent-main"]), local_v1);

    let start_point =
        resolve_worktree_start_point(&local, "agent-main", BaseSyncMode::Remote).unwrap();

    assert_eq!(start_point, "origin/agent-main");
    assert_eq!(git(&local, &["rev-parse", "agent-main"]), local_v1);
    assert_eq!(git(&local, &["rev-parse", "origin/agent-main"]), remote_v2);
}

#[test]
fn concurrent_linked_worktree_fetches_pin_the_same_origin_tip() {
    let temp = tempdir().unwrap();
    let remote = temp.path().join("remote.git");
    let seed = temp.path().join("seed");
    let local = temp.path().join("local");
    let linked = temp.path().join("linked");

    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    init_repo(&seed, "agent-main");
    let local_v1 = commit_file(&seed, "base.txt", "v1");
    git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&seed, &["push", "-u", "origin", "agent-main"]);
    git(
        temp.path(),
        &[
            "clone",
            "--branch",
            "agent-main",
            remote.to_str().unwrap(),
            local.to_str().unwrap(),
        ],
    );
    git(
        &local,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );

    let remote_v2 = commit_file(&seed, "base.txt", "v2");
    git(&seed, &["push", "origin", "agent-main"]);
    fs::write(local.join("user.txt"), "keep local bytes\n").unwrap();

    let results: Vec<Result<String, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = [&local, &linked]
            .into_iter()
            .flat_map(|path| {
                (0..3).map(|_| {
                    let path = path.clone();
                    scope.spawn(move || {
                        resolve_worktree_start_point(&path, "agent-main", BaseSyncMode::Remote)
                            .map_err(|error| error.to_string())
                    })
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("fetch thread joined"))
            .collect()
    });

    for (index, result) in results.iter().enumerate() {
        let start_point = result
            .as_ref()
            .unwrap_or_else(|error| panic!("concurrent fetch {index} failed: {error}"));
        assert_eq!(start_point, "origin/agent-main");
    }
    assert_eq!(git(&local, &["rev-parse", "origin/agent-main"]), remote_v2);
    assert_eq!(git(&linked, &["rev-parse", "origin/agent-main"]), remote_v2);
    assert_eq!(git(&local, &["rev-parse", "agent-main"]), local_v1);
    assert_eq!(git(&local, &["rev-parse", "HEAD"]), local_v1);
    assert_eq!(
        fs::read_to_string(local.join("user.txt")).unwrap(),
        "keep local bytes\n"
    );
}

#[test]
fn remote_fetch_failure_is_observable_and_leaves_local_refs() {
    let temp = tempdir().unwrap();
    let remote = temp.path().join("remote.git");
    let seed = temp.path().join("seed");
    let local = temp.path().join("local");

    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    init_repo(&seed, "agent-main");
    let local_v1 = commit_file(&seed, "base.txt", "v1");
    git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&seed, &["push", "-u", "origin", "agent-main"]);
    git(
        temp.path(),
        &[
            "clone",
            "--branch",
            "agent-main",
            remote.to_str().unwrap(),
            local.to_str().unwrap(),
        ],
    );
    git(
        &local,
        &[
            "remote",
            "set-url",
            "origin",
            "/no/such/orbit-delivery-remote.git",
        ],
    );

    let error = fetch_remote_base(&local, "agent-main").expect_err("broken origin must fail");
    let message = error.to_string();
    assert!(
        message.contains("failed to fetch") || message.contains("could not fetch"),
        "{message}"
    );
    assert_eq!(git(&local, &["rev-parse", "agent-main"]), local_v1);
    assert_eq!(git(&local, &["rev-parse", "HEAD"]), local_v1);
}

#[test]
fn local_mode_resolves_local_base_without_origin_remote() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "local-only");

    let start_point =
        resolve_worktree_start_point(&repo, "agent-main", BaseSyncMode::Local).unwrap();

    assert_eq!(start_point, "agent-main");
}

#[test]
fn git_timeout_budget_defaults_are_finite_and_operation_specific() {
    let budget = GitTimeoutBudget::DEFAULT;
    assert_eq!(budget.timeout_for(&["status"]), 30_000);
    assert_eq!(budget.timeout_for(&["rev-parse", "HEAD"]), 30_000);
    assert_eq!(budget.timeout_for(&["fetch", "origin", "main"]), 60_000);
    assert_eq!(
        budget.timeout_for(&["worktree", "add", "/tmp/wt", "main"]),
        120_000
    );
    assert_eq!(budget.timeout_for(&["rebase", "abc"]), 120_000);
    assert!(budget.default_ms >= GitTimeoutBudget::MIN_MS);
    assert!(budget.worktree_add_ms <= GitTimeoutBudget::MAX_MS);
    assert!(budget.rebase_ms <= GitTimeoutBudget::MAX_MS);
}

#[test]
fn git_timeout_budget_accepts_valid_overrides() {
    let blanket = GitTimeoutBudget::from_input(&json!({ "git_timeout_ms": 2_000 })).unwrap();
    assert_eq!(blanket.timeout_for(&["status"]), 2_000);
    assert_eq!(blanket.timeout_for(&["fetch", "origin"]), 2_000);
    assert_eq!(blanket.timeout_for(&["worktree", "add", "p"]), 2_000);
    assert_eq!(blanket.timeout_for(&["rebase", "abc"]), 2_000);

    let overlay = GitTimeoutBudget::from_input(&json!({
        "git_timeout_ms": 5_000,
        "git_timeouts": { "rebase": 8_000, "worktree_add": 7_000 }
    }))
    .unwrap();
    assert_eq!(overlay.timeout_for(&["status"]), 5_000);
    assert_eq!(overlay.timeout_for(&["rebase", "abc"]), 8_000);
    assert_eq!(overlay.timeout_for(&["worktree", "add", "p"]), 7_000);
    assert_eq!(overlay.timeout_for(&["fetch", "origin"]), 5_000);
}

#[test]
fn git_timeout_budget_rejects_invalid_and_extreme_values() {
    for (input, needle) in [
        (json!({ "git_timeout_ms": 0 }), "between"),
        (json!({ "git_timeout_ms": 600_001u64 }), "between"),
        (json!({ "git_timeout_ms": -1 }), "integer"),
        (json!({ "git_timeout_ms": "unbounded" }), "integer"),
        (json!({ "git_timeouts": { "rebase": 0 } }), "between"),
        (
            json!({ "git_timeouts": { "clone": 1_000 } }),
            "unknown git_timeouts key",
        ),
        (json!({ "git_timeouts": [] }), "must be an object"),
    ] {
        let error = GitTimeoutBudget::from_input(&input).expect_err("invalid budget");
        assert!(
            error.to_string().contains(needle),
            "expected '{needle}' in {error} for {input}"
        );
    }
}

#[cfg(unix)]
#[test]
fn git_command_success_treats_stderr_timeout_phrase_as_ordinary_failure() {
    if !with_fake_git(
        module_path!(),
        "git_command_success_treats_stderr_timeout_phrase_as_ordinary_failure",
        &[("ORBIT_TEST_GIT_PHRASE_FAIL", "show-ref".to_string())],
    ) {
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");

    let outcome = git_run(
        &repo,
        &["show-ref", "--verify", "--quiet", "refs/heads/agent-main"],
    )
    .unwrap();
    assert!(
        !outcome.timed_out,
        "stderr phrase must not classify as a supervisor timeout: {outcome:?}"
    );
    assert!(!outcome.success, "injected probe must fail: {outcome:?}");
    assert!(
        outcome.stderr.contains("process timed out"),
        "fixture must print the misleading phrase: {outcome:?}"
    );

    let success = git_command_success(
        &repo,
        &["show-ref", "--verify", "--quiet", "refs/heads/agent-main"],
    )
    .expect("ordinary Git failure is Ok(false), not a timeout error");
    assert!(
        !success,
        "failed probe with timeout prose must stay a boolean no"
    );
}

#[test]
fn git_request_always_sets_a_bounded_timeout_and_keeps_hook_policy() {
    let temp = tempdir().unwrap();
    let request = git_request(temp.path(), &["status"], 15_000);
    assert_eq!(request.timeout_ms, Some(15_000));
    assert_ne!(request.timeout_ms, None);
    assert!(
        request
            .args
            .windows(2)
            .any(|pair| pair == ["-c", "core.hooksPath=/dev/null"]),
        "hooks remain disabled: {:?}",
        request.args
    );
    assert!(
        request
            .args
            .windows(2)
            .any(|pair| pair == ["-c", "gc.auto=0"]),
        "gc.auto remains disabled: {:?}",
        request.args
    );
    let EnvironmentMode::ClearAndSet(env) = request.environment_mode else {
        panic!("git request must clear the environment");
    };
    assert!(
        env.iter()
            .any(|(key, value)| key == "GIT_OPTIONAL_LOCKS" && value == "0")
    );
    assert!(
        !env.iter()
            .any(|(key, _)| key.starts_with("ORBIT_") && key.contains("TOKEN"))
    );
}

fn init_repo(path: &Path, branch: &str) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init"]);
    git(path, &["checkout", "-b", branch]);
    git(path, &["config", "user.name", "Orbit Test"]);
    git(path, &["config", "user.email", "orbit-test@example.com"]);
}

fn commit_file(repo: &Path, file_name: &str, contents: &str) -> String {
    fs::write(repo.join(file_name), contents).unwrap();
    git(repo, &["add", file_name]);
    git(repo, &["commit", "-m", &format!("write {file_name}")]);
    git(repo, &["rev-parse", "HEAD"])
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
