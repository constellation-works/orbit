use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use orbit_common::fs::file_lock::FileLockOptions;
use orbit_common::fs::git::{git_common_dir, git_fetch_lock_target};
use orbit_common::fs::io::with_exclusive_file_lock_options;
use orbit_common::test_env::canonical_temp_dir;
use orbit_types::workflow::TRANSIENT_FAILURE_MARKER;

use super::super::git::{GitTimeoutBudget, GitTimeoutBudgetGuard, fetch_remote_base_within};

fn git_ok(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git setup");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// Fault injection: the stalled remote is an ssh shim that never answers, so the
// fetch only ends when the holder's own deadline kills it. The scaled limits
// keep the production relation (hold limit below the waiter's wait); the
// relation of the real constants is pinned by const assertions beside them.
#[test]
fn stalled_delivery_fetch_releases_the_lock_before_a_waiter_times_out() {
    let root = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
    let repo = root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let started = root.path().join("fetch-started");
    let ssh = root.path().join("ssh");
    fs::write(
        &ssh,
        format!(
            "#!/bin/sh\necho started > '{}'\nexec sleep 600\n",
            started.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    git_ok(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "ssh://fixture.invalid/owner/repository.git",
        ],
    );
    git_ok(&repo, &["config", "core.sshCommand", ssh.to_str().unwrap()]);
    git_ok(&repo, &["config", "ssh.variant", "ssh"]);

    let hold_limit = Duration::from_millis(1500);
    let waiter_wait = Duration::from_secs(4);
    let lock_target = git_fetch_lock_target(&git_common_dir(&repo).unwrap());

    let (done, holder_done) = mpsc::channel();
    let holder_repo = repo.clone();
    let holder_started = Instant::now();
    let holder = thread::spawn(move || {
        // A raised activity budget must not lengthen the hold.
        let _budget = GitTimeoutBudgetGuard::install(GitTimeoutBudget {
            fetch_ms: GitTimeoutBudget::MAX_MS,
            ..GitTimeoutBudget::DEFAULT
        });
        let result = fetch_remote_base_within(&holder_repo, "main", hold_limit);
        done.send(holder_started.elapsed()).unwrap();
        result
    });

    let give_up = Instant::now() + Duration::from_secs(20);
    while !started.exists() {
        assert!(Instant::now() < give_up, "the stalled fetch never started");
        thread::sleep(Duration::from_millis(20));
    }

    // A pilot- or final-recovery-style waiter, mid-hold, outwaits the holder.
    let waited = Instant::now();
    let acquired = with_exclusive_file_lock_options(
        &lock_target,
        "git fetch",
        FileLockOptions {
            timeout: waiter_wait,
            warn_after: waiter_wait,
            ..FileLockOptions::default()
        },
        || Ok::<_, std::io::Error>(waited.elapsed()),
    )
    .expect("a waiter must acquire the lock once the holder's deadline passes, not time out");
    assert!(acquired < waiter_wait, "waited {acquired:?}");

    let held = holder_done.recv().unwrap();
    assert!(
        held < hold_limit + Duration::from_secs(2),
        "the holder kept the lock {held:?} against a {hold_limit:?} limit"
    );
    let error = holder.join().unwrap().expect_err("a stalled fetch fails");
    let error = error.to_string();
    assert!(
        error.contains(TRANSIENT_FAILURE_MARKER),
        "a stalled remote stays a transient failure: {error}"
    );
}
