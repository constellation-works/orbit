//! `run_git` argv admission at its public boundary: an argv that would have
//! Git run a caller-named program is refused before any process starts, and
//! the plumbing Orbit drives still runs.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::git::run_git;
use tempfile::TempDir;

const IDENTITY: [&str; 4] = [
    "-c",
    "user.name=fixture",
    "-c",
    "user.email=fixture@orbit.invalid",
];

fn git_ok(repo: &Path, args: &[&str]) -> String {
    let output = run_git(repo, args).expect("admitted argv");
    assert!(output.success, "git {args:?}: {}", output.stderr);
    output.stdout.trim().to_string()
}

fn repository() -> TempDir {
    let repo = TempDir::new().expect("tempdir");
    git_ok(repo.path(), &["init", "-q", "--template=", "-b", "main"]);
    let commit = [
        &IDENTITY[..],
        &["commit", "--allow-empty", "-q", "-m", "base"],
    ]
    .concat();
    git_ok(repo.path(), &commit);
    repo
}

#[test]
fn program_running_argv_is_refused_before_git_runs() {
    let repo = repository();
    let marker = repo.path().join("ran");
    let touch = format!("touch {}", marker.display());
    let hooks = repo.path().join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("pre-commit");
    std::fs::write(&hook, format!("#!/bin/sh\n{touch}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let template = TempDir::new().unwrap();
    std::fs::create_dir(template.path().join("hooks")).unwrap();
    std::fs::copy(&hook, template.path().join("hooks/post-checkout")).unwrap();

    let upload_pack = format!("--upload-pack={touch};");
    let abbreviated = format!("--upload-p={touch};");
    let hooks_path = format!("core.hooksPath={}", hooks.display());
    let alias = format!("alias.ran=!{touch}");
    let ssh = format!("core.sshCommand={touch};");
    let template_arg = format!("--template={}", template.path().display());
    let refused: Vec<Vec<&str>> = vec![
        vec!["fetch", &upload_pack, "."],
        vec!["fetch", &abbreviated, "."],
        vec!["fetch", "--upload-pack", &touch, "."],
        vec!["ls-remote", &upload_pack, "."],
        vec![
            "-c",
            &hooks_path,
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "x",
        ],
        vec![
            "-c",
            "core.hooksPath=",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "x",
        ],
        vec!["-c", &alias, "ran"],
        vec!["-C", ".", "-c", &alias, "ran"],
        vec!["-c", &ssh, "fetch", "ssh://example.invalid/repo"],
        vec!["--exec-path=/nonexistent", "status"],
        vec!["rebase", "-x", &touch, "HEAD"],
        vec!["init", "-q", &template_arg, "nested"],
        vec!["rev-parse", "HEAD\0"],
        vec![],
        vec!["-c"],
    ];
    for args in refused {
        match run_git(repo.path(), &args) {
            Err(OrbitError::InvalidInput(message)) => {
                assert!(message.starts_with("refusing to run git"), "{message}");
            }
            Err(other) => panic!("git {args:?}: expected invalid input, got {other:?}"),
            Ok(output) => panic!("git {args:?} ran (success={})", output.success),
        }
        assert!(!marker.exists(), "git {args:?} ran a caller-named program");
    }
    assert!(!repo.path().join("nested").exists(), "refused init ran");
}

#[test]
fn orbit_plumbing_argv_still_runs() {
    let repo = repository();
    let path = repo.path();
    let head = git_ok(path, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]);

    let pinned = [
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "gc.auto=0",
        "-c",
        "protocol.file.allow=always",
    ];
    let status = [
        &pinned[..],
        &["status", "--porcelain=v1", "--untracked-files=all"],
    ]
    .concat();
    assert_eq!(git_ok(path, &status), "");

    let common = git_ok(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let git_dir = format!("--git-dir={common}");
    git_ok(
        path,
        &[
            git_dir.as_str(),
            "cat-file",
            "-e",
            &format!("{head}^{{commit}}"),
        ],
    );
    git_ok(path, &["--git-dir", &common, "rev-parse", "HEAD"]);
    // Git keys are case-insensitive; the admission compares them the same way.
    git_ok(
        path,
        &["-c", "CORE.HOOKSPATH=/dev/null", "rev-parse", "HEAD"],
    );

    git_ok(path, &["branch", "side"]);
    git_ok(path, &["merge-base", "--is-ancestor", &head, "side"]);
    assert_eq!(
        git_ok(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]),
        "main"
    );

    let checkout = path.join("checkout");
    let target = checkout.to_string_lossy().to_string();
    git_ok(
        path,
        &["worktree", "add", "--detach", "--quiet", &target, &head],
    );
    git_ok(path, &["worktree", "remove", "--force", &target]);
    git_ok(path, &["worktree", "prune"]);
}
