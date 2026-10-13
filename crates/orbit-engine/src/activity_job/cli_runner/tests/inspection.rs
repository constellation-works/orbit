#![allow(missing_docs)]

//! Retry classification for source-inspection failures [ORB-14091].
//!
//! Deterministic validation, slot ownership, a missing commit, and a
//! read-only checkout violation are permanent. Slot exhaustion and git or
//! filesystem IO stay retryable. The short-revision case is driven through
//! the job retry wrapper, which is the caller that decides whether a
//! task-pilot dispatch runs again.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orbit_common::process::shell::quote_posix_arg;
use orbit_common::test_env::{assert_child_test_passed, run_child_test};
use serde_json::json;
use tempfile::tempdir;

use super::super::super::dispatcher::DispatchError;
use super::super::inspection::SourceInspection;
use super::test_support::write_executable;

#[test]
fn missing_commit_and_slot_ownership_are_permanent() {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let revision = init_repo(&repo);
    let missing = "0123456789abcdef0123456789abcdef01234567";
    assert_permanent(
        SourceInspection::from_input(
            &json!({"inspection_revision": missing, "source_revision": missing}),
            Some(&repo),
            Some("reviewer"),
        ),
        "does not name a commit",
    );

    let blob = git(&repo, &["rev-parse", "HEAD:README"]);
    assert_permanent(
        SourceInspection::from_input(
            &json!({"inspection_revision": blob}),
            Some(&repo),
            Some("reviewer"),
        ),
        "does not name a commit",
    );

    let held = SourceInspection::from_input(
        &json!({"inspection_revision": revision, "source_revision": revision}),
        Some(&repo),
        Some("reviewer"),
    )
    .expect("lease a slot")
    .expect("inspection checkout");
    let slot = held.root().parent().expect("slot dir").to_path_buf();
    drop(held);

    fs::write(slot.join("owner"), "not-orbit\n").expect("poison owner");
    assert_permanent(
        SourceInspection::from_input(
            &json!({"inspection_revision": revision, "source_revision": revision}),
            Some(&repo),
            Some("reviewer"),
        ),
        "unrecognized owner",
    );

    fs::remove_file(slot.join("owner")).expect("remove owner");
    fs::create_dir_all(slot.join("checkout")).expect("unowned checkout");
    assert_permanent(
        SourceInspection::from_input(
            &json!({"inspection_revision": revision, "source_revision": revision}),
            Some(&repo),
            Some("reviewer"),
        ),
        "unowned inspection checkout",
    );
}

#[test]
fn slot_exhaustion_and_git_io_stay_retryable() {
    let temp = tempdir().expect("tempdir");
    let bare = temp.path().join("bare");
    fs::create_dir(&bare).expect("bare dir");
    // A `.git` file naming a missing gitdir stops Git's upward discovery
    // here. Without it, a TMPDIR nested in another checkout resolves that
    // repository and the missing revision reads as a permanent missing SHA.
    fs::write(bare.join(".git"), "gitdir: missing-gitdir\n").expect("bare gitfile");
    let missing = "0123456789abcdef0123456789abcdef01234567";
    assert_retryable(
        SourceInspection::from_input(
            &json!({"inspection_revision": missing}),
            Some(&bare),
            Some("reviewer"),
        ),
        "not a git repository",
    );

    let repo = temp.path().join("repo");
    let revision = init_repo(&repo);
    fs::write(repo.join(".orbit"), "not a directory").expect("block the pool");
    assert_retryable(
        SourceInspection::from_input(
            &json!({"inspection_revision": revision, "source_revision": revision}),
            Some(&repo),
            Some("reviewer"),
        ),
        "File exists",
    );

    let pooled = temp.path().join("pooled");
    let pooled_revision = init_repo(&pooled);
    let input = json!({
        "inspection_revision": pooled_revision,
        "source_revision": pooled_revision,
    });
    let mut held = Vec::new();
    let mut exhausted = None;
    for _ in 0..64 {
        match SourceInspection::from_input(&input, Some(&pooled), Some("reviewer")) {
            Ok(Some(inspection)) => held.push(inspection),
            Ok(None) => panic!("a pinned revision must open an inspection"),
            Err(error) => {
                exhausted = Some(error);
                break;
            }
        }
    }
    assert!(!held.is_empty(), "the pool leased at least one slot");
    let error = exhausted.expect("the inspection pool is bounded");
    assert_retryable_error(&error, "all source inspection slots are leased");
}

/// Set in the re-executed child to the directory holding the stub `git`.
const STUB_GIT_DIR_ENV: &str = "ORBIT_TEST_INSPECTION_STUB_GIT_DIR";
const STALLED_COPY_CHILD: &str = "activity_job::cli_runner::tests::inspection::stalled_full_ancestry_copy_leaves_a_reusable_slot";

/// [ORB-14806] A full-ancestry copy that stalls past its bulk deadline fails
/// the lease and is killed, and the next lease reuses the slot. Admitted under
/// test_strategy criterion 2: `create` takes the copy deadline as a seam, so a
/// stall can be injected without waiting out the production bound.
#[test]
fn stalled_full_ancestry_copy_leaves_a_reusable_slot() {
    if let Some(stub_dir) = std::env::var_os(STUB_GIT_DIR_ENV) {
        let stub_dir = Path::new(&stub_dir);
        let temp = tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let revision = init_repo(&repo);
        let input = json!({"inspection_revision": revision, "source_revision": revision});

        set_fetch_delay(stub_dir, 600);
        let error = SourceInspection::create(&repo, &revision, Duration::from_millis(500))
            .err()
            .expect("a copy that outlasts its deadline fails the lease");
        assert_retryable_error(&error, "process timed out");

        set_fetch_delay(stub_dir, 0);
        let inspection = SourceInspection::from_input(&input, Some(&repo), Some("reviewer"))
            .expect("the next lease reuses the slot")
            .expect("inspection checkout");
        inspection.verify().expect("the reused slot verifies");
        return;
    }
    run_child_with_stub_git(STALLED_COPY_CHILD);
}

/// Re-executes `test` in a child whose `git` is a stub: it sleeps for the
/// seconds in `fetch-delay` before each fetch, then runs the real Git. PATH is process-global, so the stub is installed only in the child.
fn run_child_with_stub_git(test: &str) {
    let stub_dir = tempdir().expect("stub dir");
    set_fetch_delay(stub_dir.path(), 0);
    let delay = quote_posix_arg(&stub_dir.path().join("fetch-delay").display().to_string());
    let git_path = quote_posix_arg(&real_git_path().display().to_string());
    write_executable(
        &stub_dir.path().join("git"),
        &format!(
            "#!/bin/sh\ncase \" $* \" in\n  *\" fetch \"*) sleep \"$(cat {delay})\" ;;\nesac\nexec {git_path} \"$@\"\n"
        ),
    );
    let mut paths = vec![stub_dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));

    let output_dir = tempdir().expect("child output dir");
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", test, "--nocapture"])
        .env(STUB_GIT_DIR_ENV, stub_dir.path())
        .env("PATH", std::env::join_paths(paths).expect("stub PATH"));
    let output = run_child_test(&mut command, test, output_dir.path());
    assert_child_test_passed(test, output.status, &output.stdout, &output.stderr);
}

fn set_fetch_delay(stub_dir: &Path, seconds: u64) {
    fs::write(stub_dir.join("fetch-delay"), seconds.to_string()).expect("fetch delay");
}

/// The `git` this process resolves, found before the stub shadows it.
fn real_git_path() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git on PATH")
}

fn assert_permanent(result: Result<Option<SourceInspection>, DispatchError>, needle: &str) {
    match result {
        Err(error) => assert_permanent_error(&error, needle),
        Ok(_) => panic!("deterministic inspection failure"),
    }
}

fn assert_permanent_error(error: &DispatchError, needle: &str) {
    let DispatchError::CliInvocationPermanent(message) = error else {
        panic!("ORB-14091: expected a permanent inspection failure, got {error}");
    };
    assert!(
        error.is_non_retryable(),
        "permanent inspection failures skip the retry wrapper: {error}"
    );
    assert!(message.contains(needle), "{message}");
}

fn assert_retryable(result: Result<Option<SourceInspection>, DispatchError>, needle: &str) {
    match result {
        Err(error) => assert_retryable_error(&error, needle),
        Ok(_) => panic!("retryable inspection failure"),
    }
}

fn assert_retryable_error(error: &DispatchError, needle: &str) {
    let DispatchError::CliInvocationFailed(message) = error else {
        panic!("ORB-14091: expected a retryable inspection failure, got {error}");
    };
    assert!(
        !error.is_non_retryable(),
        "slot exhaustion and git/IO stay retryable: {error}"
    );
    assert!(message.contains(needle), "{message}");
}

fn init_repo(path: &Path) -> String {
    fs::create_dir_all(path).expect("repo dir");
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "inspection@example.test"]);
    git(path, &["config", "user.name", "inspection"]);
    fs::write(path.join("README"), "hello\n").expect("readme");
    git(path, &["add", "README"]);
    git(path, &["commit", "-m", "init"]);
    git(path, &["rev-parse", "HEAD"])
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "gc.auto=0",
        ])
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout")
        .trim()
        .to_string()
}

// Security invariant at the private inspection boundary: every Git invocation,
// including verification, disables fsmonitor and discards parent Git authority.
// The shim observes the real child's effective config, then executes real Git.
#[test]
fn inspection_git_ignores_parent_locale_and_config() {
    const TEST: &str = "activity_job::cli_runner::tests::inspection::inspection_git_ignores_parent_locale_and_config";
    const REPO: &str = "ORBIT_TEST_INSPECTION_POLICY_REPO";
    if let Some(repo) = std::env::var_os(REPO) {
        let repo = Path::new(&repo);
        let revision = std::env::var("ORBIT_TEST_INSPECTION_POLICY_REVISION").unwrap();
        let blob = std::env::var("ORBIT_TEST_INSPECTION_POLICY_BLOB").unwrap();
        assert_permanent(
            SourceInspection::from_input(
                &json!({"inspection_revision": blob}),
                Some(repo),
                Some("reviewer"),
            ),
            "does not name a commit",
        );
        let inspection = SourceInspection::from_input(
            &json!({"inspection_revision": revision}),
            Some(repo),
            Some("reviewer"),
        )
        .unwrap()
        .unwrap();
        inspection.verify().unwrap();
        return;
    }
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let revision = init_repo(&repo);
    let blob = git(&repo, &["rev-parse", "HEAD:README"]);
    let stub_dir = temp.path().join("bin");
    fs::create_dir(&stub_dir).unwrap();
    let real_git = serde_json::to_string(&real_git_path()).unwrap();
    write_executable(
        &stub_dir.join("git"),
        &format!(
            r#"#!/usr/bin/env python3
import os, subprocess, sys
assert os.environ.get('LC_ALL') == 'C', os.environ.get('LC_ALL')
assert 'LANGUAGE' not in os.environ
assert 'GIT_DIR' not in os.environ
assert not any(k == 'GIT_CONFIG' or k.startswith('GIT_CONFIG_') for k in os.environ)
args = sys.argv[1:]
i = 0
while i < len(args) and args[i] == '-c':
    i += 2
config = subprocess.check_output([{real_git}, *args[:i], 'config', '--get', 'core.fsmonitor'], text=True)
assert config.strip() == 'false', config
os.execv({real_git}, [{real_git}, *args])
"#
        ),
    );
    let mut paths = vec![stub_dir];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    for locale in ["C", "de_DE.UTF-8"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", TEST, "--nocapture"])
            .env(REPO, &repo)
            .env("ORBIT_TEST_INSPECTION_POLICY_REVISION", &revision)
            .env("ORBIT_TEST_INSPECTION_POLICY_BLOB", &blob)
            .env("PATH", std::env::join_paths(&paths).unwrap())
            .env("LANG", locale)
            .env("LC_ALL", locale)
            .env("LANGUAGE", "de")
            .env("GIT_DIR", temp.path().join("absent-gitdir"))
            .env("GIT_CONFIG", "absent-config")
            .env("GIT_CONFIG_GLOBAL", "absent-global-config")
            .env("GIT_CONFIG_SYSTEM", "absent-system-config")
            .env("GIT_CONFIG_PARAMETERS", "malformed")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.fsmonitor")
            .env("GIT_CONFIG_VALUE_0", "true")
            .env("GIT_CONFIG_FUTURE_OVERRIDE", "must-be-scrubbed");
        let output = run_child_test(&mut child, TEST, temp.path());
        assert_child_test_passed(TEST, output.status, &output.stdout, &output.stderr);
    }
}
