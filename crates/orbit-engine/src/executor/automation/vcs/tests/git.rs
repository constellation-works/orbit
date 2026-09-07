#![allow(missing_docs)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

use super::super::git::{BaseSyncMode, fetch_remote_base, resolve_worktree_start_point};

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
