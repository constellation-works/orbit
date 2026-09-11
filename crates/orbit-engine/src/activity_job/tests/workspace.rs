#![allow(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

use super::super::dispatcher::DispatchError;
use super::super::workspace::fingerprint::{git_fingerprint, untracked_file_identity};
use super::super::workspace::*;

/// Names the isolated child that owns the Git shim, so a future second user of
/// the fixture cannot mistake another test's child process for its own.
const GIT_SHIM_CHILD_ENV: &str = "ORBIT_TEST_GIT_SHIM_CHILD";

/// Carries the shim's invocation log to that child.
const GIT_SHIM_LOG_ENV: &str = "ORBIT_TEST_GIT_SHIM_LOG";

#[test]
fn resolve_subprocess_cwd_prefers_input_over_task_over_tool_ctx() {
    let input_dir = tempdir().expect("input tempdir");
    let task_dir = tempdir().expect("task tempdir");
    let tool_dir = tempdir().expect("tool tempdir");

    let input = serde_json::json!({
        "workspace_path": input_dir.path().display().to_string()
    });
    let task_ctx = serde_json::json!({
        "workspace_path": task_dir.path().display().to_string()
    });
    let resolved = resolve_subprocess_cwd(&input, Some(&task_ctx), Some(tool_dir.path()))
        .expect("input cwd resolves");
    assert_eq!(
        resolved,
        Some(
            input_dir
                .path()
                .canonicalize()
                .expect("canonical input dir")
        )
    );

    let input = serde_json::json!({});
    let resolved = resolve_subprocess_cwd(&input, Some(&task_ctx), Some(tool_dir.path()))
        .expect("task cwd resolves");
    assert_eq!(
        resolved,
        Some(task_dir.path().canonicalize().expect("canonical task dir"))
    );

    // Absent key (direct, non-worktree run): fall back to the tool context's
    // workspace_root — the repo root is the correct cwd there.
    let resolved =
        resolve_subprocess_cwd(&input, None, Some(tool_dir.path())).expect("tool cwd resolves");
    assert_eq!(
        resolved,
        Some(tool_dir.path().canonicalize().expect("canonical tool dir"))
    );
}

#[test]
fn resolve_subprocess_cwd_fails_closed_on_declared_non_string_workspace_path() {
    // Regression (ORB-10134): a worktree pipeline step whose workspace_path
    // template rendered to a non-string, non-null value must fail closed, not
    // silently fall back to the tool context's workspace_root (the primary
    // checkout).
    let tool_dir = tempdir().expect("tool tempdir");

    for value in [
        serde_json::json!(42),
        serde_json::json!(true),
        serde_json::json!({ "nested": "object" }),
    ] {
        let input = serde_json::json!({ "workspace_path": value });
        let err = resolve_subprocess_cwd(&input, None, Some(tool_dir.path()))
            .expect_err("non-string workspace_path must fail closed");
        match err {
            DispatchError::CliInvocationFailed(message) => {
                assert!(
                    message.contains("non-string workspace_path"),
                    "message should flag the non-string value: {message}"
                );
            }
            other => panic!("expected CliInvocationFailed, got {other:?}"),
        }
    }

    // An empty-string render is likewise refused (fail closed, not fall back).
    let input = serde_json::json!({ "workspace_path": "   " });
    let err = resolve_subprocess_cwd(&input, None, Some(tool_dir.path()))
        .expect_err("empty workspace_path must fail closed");
    assert!(matches!(err, DispatchError::CliInvocationFailed(_)));
}

#[test]
fn resolve_subprocess_cwd_treats_null_workspace_path_as_absent() {
    // The agent envelope / task context serialize an undeclared workspace_path
    // as JSON null; that must be treated as "not declared" so direct
    // (non-worktree) runs fall back to the tool context's workspace_root.
    let tool_dir = tempdir().expect("tool tempdir");

    let input = serde_json::json!({ "workspace_path": null });
    let resolved = resolve_subprocess_cwd(&input, None, Some(tool_dir.path()))
        .expect("null input workspace_path falls back to tool cwd");
    assert_eq!(
        resolved,
        Some(tool_dir.path().canonicalize().expect("canonical tool dir"))
    );

    // Same for a null workspace_path on the task context (its always-present
    // key), which is how a task with no declared workspace serializes.
    let task_ctx = serde_json::json!({ "workspace_path": null });
    let resolved = resolve_subprocess_cwd(
        &serde_json::json!({}),
        Some(&task_ctx),
        Some(tool_dir.path()),
    )
    .expect("null task-context workspace_path falls back to tool cwd");
    assert_eq!(
        resolved,
        Some(tool_dir.path().canonicalize().expect("canonical tool dir"))
    );
}

#[test]
fn resolve_subprocess_cwd_rejects_non_directory_path() {
    let temp = tempdir().expect("tempdir");
    let file = temp.path().join("not-a-dir");
    std::fs::write(&file, b"not a directory").expect("write file");
    let task_ctx = serde_json::json!({
        "workspace_path": file.display().to_string()
    });

    let err = resolve_subprocess_cwd(&serde_json::json!({}), Some(&task_ctx), None)
        .expect_err("file path rejected");
    match err {
        DispatchError::CliInvocationFailed(message) => {
            assert!(
                message.contains(&file.display().to_string()),
                "message should name file path: {message}"
            );
        }
        other => panic!("expected CliInvocationFailed, got {other:?}"),
    }
}

#[test]
fn resolve_subprocess_cwd_rejects_declared_missing_path() {
    let temp = tempdir().expect("tempdir");
    let missing = temp.path().join("missing-worktree");
    let input = serde_json::json!({
        "workspace_path": missing.display().to_string()
    });

    let err = resolve_subprocess_cwd(&input, None, None).expect_err("missing path rejected");
    match err {
        DispatchError::CliInvocationFailed(message) => {
            assert!(
                message.contains(&missing.display().to_string()),
                "message should name missing path: {message}"
            );
        }
        other => panic!("expected CliInvocationFailed, got {other:?}"),
    }
}

#[test]
fn vanished_untracked_staging_file_is_not_a_snapshot_failure() {
    let root = tempdir().expect("tempdir");
    let path = root.path().join(".tracked.yaml.refresh.tmp");
    std::fs::write(&path, "staged").expect("stage file");
    std::fs::remove_file(&path).expect("atomic rename consumed staging file");

    assert_eq!(
        untracked_file_identity(root.path(), ".tracked.yaml.refresh.tmp")
            .expect("vanished staging file is benign"),
        None
    );
}

#[test]
fn fingerprint_identities_match_per_path_git_and_stay_stable() {
    let fixture = linked_worktree_fixture();
    seed_identity_dirt(&fixture.assigned);

    let first = git_fingerprint(&fixture.assigned).expect("first fingerprint");
    let second = git_fingerprint(&fixture.assigned).expect("second fingerprint");
    assert_eq!(
        first, second,
        "consecutive snapshots of an unchanged dirty tree must be byte-identical"
    );

    let value = serde_json::to_value(&first).expect("serialize fingerprint");
    let spaced = "dir with spaces/a file.txt";
    let quoted = "weird/foo\"bar.txt";
    assert_eq!(
        value["untracked_content"][spaced],
        untracked_file_identity(&fixture.assigned, spaced)
            .expect("spaced untracked identity")
            .expect("spaced file exists")
    );
    assert_eq!(
        value["untracked_content"][quoted],
        untracked_file_identity(&fixture.assigned, quoted)
            .expect("quoted untracked identity")
            .expect("quoted file exists")
    );

    assert_eq!(
        value["path_states"]["README.md"]["index_entry_sha256"],
        expected_optional_identity(
            "git-index-entry-v1",
            &git_bytes(
                &fixture.assigned,
                &["ls-files", "--stage", "-z", "--", "README.md"]
            )
        )
    );
    assert_eq!(
        value["path_states"]["README.md"]["worktree_patch_sha256"],
        expected_optional_identity(
            "git-worktree-path-patch-v1",
            &git_bytes(
                &fixture.assigned,
                &[
                    "diff",
                    "--binary",
                    "--full-index",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-renames",
                    "--",
                    "README.md",
                ]
            )
        )
    );
    assert_eq!(
        value["path_states"]["staged.txt"]["staged_patch_sha256"],
        expected_optional_identity(
            "git-staged-path-patch-v1",
            &git_bytes(
                &fixture.assigned,
                &[
                    "diff",
                    "--cached",
                    "--binary",
                    "--full-index",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-renames",
                    "HEAD",
                    "--",
                    "staged.txt",
                ]
            )
        )
    );
    assert_eq!(
        value["tracked_patch_sha256"],
        expected_identity(
            "git-tracked-patch-v1",
            &git_bytes(
                &fixture.assigned,
                &[
                    "diff",
                    "--binary",
                    "--full-index",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-renames",
                    "HEAD",
                    "--",
                ]
            )
        )
    );
}

#[test]
fn capture_of_thousands_of_untracked_files_uses_a_constant_git_budget() {
    let Some(shim) = GitShim::install(
        module_path!(),
        "capture_of_thousands_of_untracked_files_uses_a_constant_git_budget",
    ) else {
        return;
    };

    let fixture = linked_worktree_fixture();
    write_untracked_tree(&fixture.assigned, 25);
    let pair = declared_pair(&fixture, "ORB-FINGERPRINT-BUDGET", "run-fingerprint-budget");
    let input = worktree_input(&fixture, "ORB-FINGERPRINT-BUDGET");

    let small_before = shim.invocation_count(&[&fixture.assigned, &fixture.primary]);
    WorktreeBoundaryGuard::capture(
        &input,
        None,
        "run-fingerprint-budget-small",
        "codex",
        Some(&fixture.assigned),
        Some(&fixture.primary),
        Some(&pair),
    )
    .expect("small capture")
    .expect("small guard enabled");
    let small_invocations =
        shim.invocation_count(&[&fixture.assigned, &fixture.primary]) - small_before;
    assert!(
        small_invocations > 0,
        "capture must spawn git through the PATH shim"
    );

    write_untracked_tree(&fixture.assigned, 2_000);
    let large_before = shim.invocation_count(&[&fixture.assigned, &fixture.primary]);
    let started = Instant::now();
    WorktreeBoundaryGuard::capture(
        &input,
        None,
        "run-fingerprint-budget-large",
        "codex",
        Some(&fixture.assigned),
        Some(&fixture.primary),
        Some(&pair),
    )
    .expect("large capture")
    .expect("large guard enabled");
    let elapsed = started.elapsed();
    let large_invocations =
        shim.invocation_count(&[&fixture.assigned, &fixture.primary]) - large_before;

    assert_eq!(
        small_invocations, large_invocations,
        "git spawns must stay constant as untracked files grow ({small_invocations} vs {large_invocations})"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "2,000 untracked files must fingerprint well under a second, took {elapsed:?}"
    );
}

struct LinkedWorktreeFixture {
    primary: PathBuf,
    assigned: PathBuf,
    _temp: TempDir,
}

fn linked_worktree_fixture() -> LinkedWorktreeFixture {
    let temp = tempdir().expect("fixture tempdir");
    let primary = temp.path().join("primary");
    let assigned = temp.path().join("assigned");
    fs::create_dir_all(&primary).expect("create primary");
    git_ok(&primary, &["init"]);
    git_ok(&primary, &["config", "user.name", "Orbit Test"]);
    git_ok(
        &primary,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    fs::write(primary.join("README.md"), "base\n").expect("write initial file");
    git_ok(&primary, &["add", "README.md"]);
    git_ok(&primary, &["commit", "-m", "initial"]);
    git_ok(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            "orbit-fingerprint-test",
            assigned.to_str().expect("utf8 assigned path"),
        ],
    );

    LinkedWorktreeFixture {
        primary: primary.canonicalize().expect("canonical primary"),
        assigned: assigned.canonicalize().expect("canonical assigned"),
        _temp: temp,
    }
}

fn seed_identity_dirt(root: &Path) {
    fs::write(root.join("README.md"), b"binary\0dirt\n").expect("dirty tracked file");
    fs::write(root.join("staged.txt"), "staged\n").expect("write staged file");
    git_ok(root, &["add", "--", "staged.txt"]);
    fs::create_dir_all(root.join("dir with spaces")).expect("spaced dir");
    fs::write(root.join("dir with spaces/a file.txt"), "x").expect("spaced untracked");
    fs::create_dir_all(root.join("weird")).expect("weird dir");
    fs::write(root.join("weird/foo\"bar.txt"), "q\n").expect("quoted untracked");
}

fn write_untracked_tree(root: &Path, count: usize) {
    let dir = root.join("untracked-batch");
    fs::create_dir_all(&dir).expect("untracked dir");
    for index in 0..count {
        fs::write(dir.join(format!("{index}.txt")), index.to_string()).expect("untracked file");
    }
}

fn worktree_input(fixture: &LinkedWorktreeFixture, task_id: &str) -> serde_json::Value {
    serde_json::json!({
        "prompt": "implement",
        "task_id": task_id,
        "workspace_path": fixture.assigned,
        "repo_root": fixture.assigned,
    })
}

fn declared_pair(
    fixture: &LinkedWorktreeFixture,
    task_id: &str,
    run_id: &str,
) -> super::super::workspace::DeclaredWorktreePair {
    let input = worktree_input(fixture, task_id);
    validate_declared_worktree_pair(&input, None, run_id, "codex", Some(&fixture.primary))
        .expect("validate declared pair")
        .expect("linked worktree pair")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let output = git_command(repo, args);
    assert!(
        output.status.success(),
        "git {} in {} failed: {}",
        args.join(" "),
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_bytes(repo: &Path, args: &[&str]) -> Vec<u8> {
    let output = git_command(repo, args);
    assert!(
        output.status.success(),
        "git {} in {} failed: {}",
        args.join(" "),
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_command(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git")
}

fn expected_identity(domain: &str, bytes: &[u8]) -> serde_json::Value {
    serde_json::Value::String(domain_sha256(domain, bytes))
}

fn expected_optional_identity(domain: &str, bytes: &[u8]) -> serde_json::Value {
    if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        expected_identity(domain, bytes)
    }
}

fn domain_sha256(domain: &str, bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

/// Child-side handle on the `git` shim installed for the isolated test
/// process: the shim logs each invocation's cwd and arguments, then delegates
/// to the real Git binary.
struct GitShim {
    log_path: PathBuf,
}

impl GitShim {
    /// Isolate the shim's PATH in a child test process. Prepending the shim
    /// directory to this process's PATH would redirect `git` for every test
    /// running concurrently in the same binary, and those tests then fail once
    /// the shim's TempDir is removed.
    ///
    /// Returns the shim handle when this process is the isolated child; in the
    /// parent it runs the child to completion and returns `None`.
    fn install(module: &str, test: &str) -> Option<Self> {
        let module = module
            .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
            .unwrap_or(module);
        let exact_test = format!("{module}::{test}");

        if std::env::var(GIT_SHIM_CHILD_ENV).ok().as_deref() == Some(&exact_test) {
            let log_path = std::env::var_os(GIT_SHIM_LOG_ENV).expect("shim log path in child");
            return Some(Self {
                log_path: PathBuf::from(log_path),
            });
        }

        let dir = tempdir().expect("shim dir");
        let log_path = dir.path().join("git-invocations.log");
        fs::write(&log_path, "").expect("create shim log");
        let real_git = which_git();
        let script = dir.path().join("git");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\t%s\\n' \"$(pwd)\" \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
                log_path.display(),
                real_git.display()
            ),
        )
        .expect("write git shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script).expect("shim metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).expect("shim permissions");
        }

        let mut paths = vec![dir.path().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", &exact_test, "--nocapture"])
            .env(GIT_SHIM_CHILD_ENV, &exact_test)
            .env(GIT_SHIM_LOG_ENV, &log_path)
            .env("PATH", std::env::join_paths(paths).expect("shim PATH"))
            .output()
            .expect("isolated git shim test");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "git shim test failed:\n{}\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        );
        // A libtest `--exact` filter that matches nothing still exits 0, which
        // would let a stale `test` literal (e.g. after the enclosing test was
        // renamed) make this fixture pass without ever running the child's
        // assertions. Require the harness summary to confirm the one test we
        // asked for actually ran.
        assert_exactly_one_test_ran(&exact_test, &stdout);

        None
    }

    fn invocation_count(&self, roots: &[&Path]) -> usize {
        let log = fs::read_to_string(&self.log_path).expect("read shim log");
        log.lines()
            .filter(|line| {
                roots.iter().any(|root| {
                    line.split_once('\t')
                        .is_some_and(|(cwd, _)| Path::new(cwd) == *root)
                })
            })
            .count()
    }
}

fn which_git() -> PathBuf {
    let output = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate git");
    assert!(output.status.success(), "git must be on PATH");
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
}

/// Fails unless the child's libtest summary reports exactly one test run.
/// `--exact <filter>` exits 0 whether it matched one test or filtered out
/// every test, so `output.status.success()` alone cannot tell a real run
/// apart from a stale `exact_test` that no longer names any function.
fn assert_exactly_one_test_ran(exact_test: &str, child_stdout: &str) {
    let passed = child_stdout.lines().find_map(|line| {
        let rest = line.strip_prefix("test result: ok. ")?;
        let (count, _) = rest.split_once(" passed;")?;
        count.parse::<usize>().ok()
    });
    assert_eq!(
        passed,
        Some(1),
        "git shim child must run exactly one test ({exact_test}); got:\n{child_stdout}"
    );
}

#[test]
fn zero_matched_tests_are_rejected_instead_of_passing_silently() {
    let stdout = "running 0 tests\n\n\
        test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s\n";
    let result =
        std::panic::catch_unwind(|| assert_exactly_one_test_ran("module::renamed_away", stdout));
    assert!(
        result.is_err(),
        "a filter that matched no tests must fail the git shim fixture, not pass silently"
    );
}

#[test]
fn one_matched_test_is_accepted() {
    let stdout = "running 1 test\ntest module::real_test ... ok\n\n\
        test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
    assert_exactly_one_test_ran("module::real_test", stdout);
}
