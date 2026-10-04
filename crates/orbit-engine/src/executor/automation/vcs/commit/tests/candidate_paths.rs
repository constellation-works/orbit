use std::fs;
use std::path::Path;

use orbit_types::task::Task;
use serde_json::{Value, json};

use super::super::git_commit;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};
use crate::executor::automation::vcs::git::{git_output, git_success};

const CLAIMED_TASK_ID: &str = "T-CLAIMED";

/// A claimed task whose frozen footprint is `selectors`, with no exact
/// selector for any file the implementer creates.
fn claimed_task(selectors: &[&str]) -> Task {
    let mut task = task_with_file(CLAIMED_TASK_ID, "Split a module", "unused", "claude");
    task.context_files = selectors
        .iter()
        .map(|selector| (*selector).to_string())
        .collect();
    task
}

/// The `commit` step input both claimed pipelines pass, carrying this
/// attempt's implementer output.
fn claimed_commit_input(workspace: &Path) -> Value {
    json!({
        "scope": "all",
        "job_run_id": "batch-1",
        "workspace_path": workspace,
        "implementation": {
            "execution_summary": "Outcome: success\nChanges:\n- split the module",
            "context_files_added": ["file:src/split/new.rs"],
        },
    })
}

/// A fixture repo checked out on `candidate`, branched from `agent-main`, as
/// a claimed leaf's worktree is.
fn claimed_worktree() -> tempfile::TempDir {
    let temp = initialized_git_repo();
    git_success(temp.path(), &["branch", "agent-main"]).unwrap();
    git_success(temp.path(), &["checkout", "-b", "candidate"]).unwrap();
    temp
}

fn write(workspace: &Path, path: &str, contents: &str) {
    let path = workspace.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn untracked_status(workspace: &Path) -> String {
    git_output(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .unwrap()
}

/// ORB-13756: scratch under `.orbit/tmp/` is never a delivery candidate, even
/// where the repository does not ignore it and a selector covers it: not
/// under a claim footprint admitting `dir:.orbit`, and not under a local
/// run's exact selector. It is left untouched rather than refused, since it
/// is where every worker is told to write scratch.
#[test]
fn scratch_under_orbit_tmp_is_never_delivered() {
    let claimed = |workspace: &Path| {
        CommitTestHost::new(
            vec![claimed_task(&["dir:src", "dir:.orbit"])],
            workspace.to_path_buf(),
        )
        .with_claim_binding(CLAIMED_TASK_ID)
    };
    let local = |workspace: &Path| {
        let mut task = claimed_task(&["file:src/new.rs", "file:.orbit/tmp/evidence.json"]);
        task.execution_summary = "Outcome: success\nChanges:\n- added".to_string();
        CommitTestHost::new(vec![task], workspace.to_path_buf())
    };
    let hosts: [&dyn Fn(&Path) -> CommitTestHost; 2] = [&claimed, &local];
    for host in hosts {
        let temp = claimed_worktree();
        let workspace = temp.path();
        write(workspace, "src/new.rs", "pub fn added() {}\n");
        write(workspace, ".orbit/tmp/evidence.json", "{}");
        write(workspace, ".orbit/tmp/logs/run.log", "log");

        git_commit(&host(workspace), &claimed_commit_input(workspace))
            .expect("source is delivered past the scratch");
        assert_eq!(
            git_output(workspace, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
            "src/new.rs"
        );
        assert_eq!(
            untracked_status(workspace),
            "?? .orbit/tmp/evidence.json\n?? .orbit/tmp/logs/run.log"
        );
        assert_eq!(
            fs::read_to_string(workspace.join(".orbit/tmp/evidence.json")).unwrap(),
            "{}"
        );
    }
}

/// Commit admission and the owner's observed candidate must agree on original
/// module intent and eligible widening, without treating `crates/` as one unit.
#[test]
fn claimed_module_new_paths_match_owner_handoff_validation() {
    for (selector, new_path, allowed) in [
        ("file:a/b/x.rs", "a/b/y.rs", true),
        ("file:a/b/x.rs", "a/b/tests/x.rs", true),
        ("file:a/b/x.rs", "a/b/tests/nested/x.rs", true),
        ("file:a/b/x.rs", "a/c/z.rs", true),
        ("file:a/b/x.rs", "a/b/other/z.rs", true),
        ("file:a/b/x.rs", "a/b/tests-other/z.rs", true),
        ("dir:a/b", "a/b/child/z.rs", true),
        ("dir:a/b", "a/b-other/z.rs", true),
        ("file:x.rs", "y.rs", true),
        ("file:x.rs", "other/y.rs", false),
        ("dir:.", "a/b/y.rs", true),
        (
            "file:crates/one/src/lib.rs",
            "crates/one/new/module.rs",
            true,
        ),
        ("file:crates/one/src/lib.rs", "crates/two/src/lib.rs", false),
        (
            "file:crates/one/src/lib.rs",
            "crates/two/tests/env.rs",
            true,
        ),
        (
            "file:crates/one/src/lib.rs",
            "crates/one-other/src/lib.rs",
            false,
        ),
        ("file:crates/one/src/lib.rs", "crates/one/.env", false),
        ("dir:.", ".orbit/private.json", false),
    ] {
        let temp = claimed_worktree();
        let workspace = temp.path();
        let base = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
        let task = claimed_task(&[selector]);
        let host = CommitTestHost::new(vec![task.clone()], workspace.to_path_buf())
            .with_claim_binding(CLAIMED_TASK_ID);
        write(workspace, new_path, "pub fn added() {}\n");
        let before = untracked_status(workspace);
        assert!(
            super::super::scope::task_candidate_paths(
                workspace,
                std::slice::from_ref(&task),
                super::super::scope::NewPathIntent::ExactFile,
            )
            .is_err(),
            "local delivery still requires an exact selector for {new_path}"
        );
        let result = git_commit(&host, &claimed_commit_input(workspace));
        if allowed {
            result.expect("admitted module new path is committed");
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("task delivery refused unknown untracked paths"),
                "Untouched units and protected paths must retain the delivery refusal"
            );
            assert_eq!(
                untracked_status(workspace),
                before,
                "refusal preserves the index and bytes"
            );
            // Build the candidate independently as a malicious worker could;
            // owner validation must refuse it even though Git accepts it.
            git_success(workspace, &["add", "--", new_path]).unwrap();
            git_success(workspace, &["commit", "-m", "candidate"]).unwrap();
        }
        assert_eq!(
            crate::validate_claim_new_paths(workspace, &task.context_files, &base, "HEAD").is_ok(),
            allowed,
            "Owner validation must agree for {selector} -> {new_path}"
        );
        if matches!(
            new_path,
            "crates/one/new/module.rs" | "crates/two/tests/env.rs" | "a/c/z.rs"
        ) {
            let (_, widening) =
                crate::validate_claim_new_paths(workspace, &task.context_files, &base, "HEAD")
                    .unwrap();
            assert_eq!(
                widening,
                vec![new_path.to_string()],
                "eligible extra files must produce an exact owner widening request"
            );
        }
    }
}

/// Candidate Git modes, rather than the owner's current worktree, determine
/// symlink safety and renamed additions.
#[cfg(unix)]
#[test]
fn owner_new_path_validation_refuses_symlink_and_untouched_rename_destination() {
    for symlink in [true, false] {
        let temp = claimed_worktree();
        let workspace = temp.path();
        let base = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
        let path = if symlink {
            "src/new/link.rs"
        } else {
            "other/renamed.rs"
        };
        fs::create_dir_all(workspace.join(path).parent().unwrap()).unwrap();
        if symlink {
            std::os::unix::fs::symlink("../../outside", workspace.join(path)).unwrap();
        } else {
            // A rename still has an addition destination when rename detection
            // is disabled, even when Git considers the bytes identical.
            git_success(workspace, &["mv", "README.md", path]).unwrap();
        }
        git_success(workspace, &["add", "--", path]).unwrap();
        git_success(workspace, &["commit", "-m", "untrusted candidate"]).unwrap();
        let error =
            crate::validate_claim_new_paths(workspace, &["file:src/lib.rs".into()], &base, "HEAD")
                .unwrap_err()
                .to_string();
        assert!(
            error.contains(path),
            "owner refusal must name the candidate path: {error}"
        );
    }
}
