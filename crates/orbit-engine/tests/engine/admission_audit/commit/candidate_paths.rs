use std::fs;
use std::path::Path;

use orbit_types::task::{ContextWideningStep, Task};
use serde_json::{Value, json};

use super::git_commit;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};
use super::{git_output, git_success};

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
fn scratch_under_orbit_tmp_is_never_delivered() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::scratch_under_orbit_tmp_is_never_delivered"
    )) {
        return;
    }

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

/// ORB-13990: the 2026-10-04 claim refusals. An implementer may create a path
/// outside the frozen footprint (a new top-level `tests/` file, another
/// crate): the claimed leaf commits it, and owner validation turns every
/// added path the footprint does not cover into an exact widening request.
/// Only paths no owner can accept are refused — Git or `.orbit` metadata and
/// environment files — before any index change.
fn claimed_new_paths_outside_the_footprint_deliver_and_request_owner_widening() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::claimed_new_paths_outside_the_footprint_deliver_and_request_owner_widening"
    )) {
        return;
    }

    for (selector, new_path, widened) in [
        (
            "file:crates/one/src/lib.rs",
            "tests/claim_refusal.rs",
            Some(true),
        ),
        (
            "file:crates/one/src/lib.rs",
            "crates/two/src/lib.rs",
            Some(true),
        ),
        (
            "file:crates/one/src/lib.rs",
            "crates/one/tests/new.rs",
            Some(true),
        ),
        ("file:a/b/x.rs", "other/y.rs", Some(true)),
        ("dir:a/b", "a/b/child/z.rs", Some(false)),
        ("dir:.", "a/b/y.rs", Some(false)),
        ("file:crates/one/src/lib.rs", "crates/one/.env", None),
        ("dir:.", ".orbit/private.json", None),
        ("dir:.", ".Orbit/private.json", None),
        ("dir:.", ".ENV", None),
        ("dir:.", ".Env.local", None),
        ("dir:.", ".ENVRC", None),
    ] {
        let temp = claimed_worktree();
        let workspace = temp.path();
        let base = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
        let task = claimed_task(&[selector]);
        let host = CommitTestHost::new(vec![task.clone()], workspace.to_path_buf())
            .with_claim_binding(CLAIMED_TASK_ID);
        write(workspace, new_path, "pub fn added() {}\n");
        let before = untracked_status(workspace);
        let result = git_commit(&host, &claimed_commit_input(workspace));
        let Some(widened) = widened else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("task delivery refused protected untracked paths"),
                "{new_path} is a path no owner can accept"
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
            assert!(
                orbit_engine::validate_claim_new_paths(
                    workspace,
                    &task.context_files,
                    &base,
                    "HEAD"
                )
                .is_err(),
                "owner validation must refuse {new_path}"
            );
            continue;
        };
        result.unwrap_or_else(|error| panic!("{selector} -> {new_path} delivers: {error}"));
        assert_eq!(
            git_output(workspace, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
            new_path
        );
        assert!(
            host.widenings(CLAIMED_TASK_ID).is_empty(),
            "a claimed leaf never writes the owner's selectors; the owner widens at handoff"
        );
        let (_, widening) =
            orbit_engine::validate_claim_new_paths(workspace, &task.context_files, &base, "HEAD")
                .unwrap();
        assert_eq!(
            widening,
            if widened {
                vec![new_path.to_string()]
            } else {
                Vec::new()
            },
            "owner widening request for {selector} -> {new_path}"
        );
    }
}

/// ORB-13990: an owner run's implementer may create and modify paths outside
/// the task's selectors. Delivery commits them and widens the task's
/// selectors with exact `file:` entries, recording the implement step.
fn owner_delivery_commits_out_of_selector_paths_and_widens_with_provenance() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::owner_delivery_commits_out_of_selector_paths_and_widens_with_provenance"
    )) {
        return;
    }

    let temp = claimed_worktree();
    let workspace = temp.path();
    let mut task = claimed_task(&["file:src/lib.rs"]);
    task.execution_summary = "Outcome: success\nChanges:\n- added a test".to_string();
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());
    write(workspace, "src/lib.rs", "pub fn changed() {}\n");
    write(workspace, "tests/claim_refusal.rs", "#[test]\nfn t() {}\n");
    write(workspace, "README.md", "edited outside the selectors\n");

    git_commit(&host, &claimed_commit_input(workspace)).expect("owner delivery");

    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
        "README.md\nsrc/lib.rs\ntests/claim_refusal.rs"
    );
    let widenings = host.widenings(CLAIMED_TASK_ID);
    assert_eq!(widenings.len(), 1, "{widenings:?}");
    assert_eq!(widenings[0].step, ContextWideningStep::Implement);
    assert_eq!(
        widenings[0].selectors,
        vec!["file:README.md", "file:tests/claim_refusal.rs"]
    );
    assert_eq!(
        host.task(CLAIMED_TASK_ID).context_files,
        vec![
            "file:src/lib.rs",
            "file:README.md",
            "file:tests/claim_refusal.rs"
        ]
    );
}

/// ORB-13990: a multi-task bundle commits every path with exactly one task
/// and refuses nothing. Ambiguous ownership goes to the task whose agent
/// changed the path (its widening history), else the exact `file:` owner; a
/// path no selector covers goes to the first task, whose selectors widen.
fn per_task_bundles_attribute_each_path_to_the_task_whose_agent_changed_it() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::per_task_bundles_attribute_each_path_to_the_task_whose_agent_changed_it"
    )) {
        return;
    }

    let temp = claimed_worktree();
    let workspace = temp.path();
    let first = {
        let mut task = task_with_file("T-A", "First", "unused", "claude");
        task.context_files = vec!["dir:src".to_string()];
        task
    };
    let second = task_with_file("T-B", "Second", "src/b.rs", "claude");
    let host = CommitTestHost::new(vec![first, second], workspace.to_path_buf())
        .with_agent_widening("T-B", &["src/b_new.rs"]);
    write(workspace, "src/a.rs", "a\n");
    write(workspace, "src/b.rs", "b\n");
    write(workspace, "src/b_new.rs", "b new\n");
    write(workspace, "docs/unowned.md", "unowned\n");

    let output = git_commit(
        &host,
        &json!({
            "scope": "per_task",
            "job_run_id": "batch-1",
            "workspace_path": workspace,
            "completed_task_ids": ["T-A", "T-B"],
        }),
    )
    .expect("ambiguous and unowned paths are attributed, not refused");

    assert_eq!(output["committed_task_ids"], json!(["T-A", "T-B"]));
    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
        "src/b.rs\nsrc/b_new.rs",
        "T-B commits its exact-selector file and the file its agent created"
    );
    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-only", "HEAD~1"]).unwrap(),
        "docs/unowned.md\nsrc/a.rs"
    );
    assert_eq!(
        host.widenings("T-A")
            .iter()
            .flat_map(|widening| widening.selectors.clone())
            .collect::<Vec<_>>(),
        vec!["file:docs/unowned.md"]
    );
    assert!(untracked_status(workspace).is_empty());
}

/// Candidate Git modes, rather than the owner's current worktree, determine
/// symlink safety; a renamed addition outside the footprint is an ordinary
/// widening request.
#[cfg(unix)]
fn owner_new_path_validation_refuses_symlinks_and_widens_rename_destinations() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::owner_new_path_validation_refuses_symlinks_and_widens_rename_destinations"
    )) {
        return;
    }

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
        let validated = orbit_engine::validate_claim_new_paths(
            workspace,
            &["file:src/lib.rs".into()],
            &base,
            "HEAD",
        );
        if symlink {
            let error = validated.unwrap_err().to_string();
            assert!(
                error.contains(path),
                "owner refusal must name the candidate path: {error}"
            );
        } else {
            assert_eq!(validated.unwrap().1, vec![path.to_string()]);
        }
    }
}

fn owner_host(workspace: &Path) -> CommitTestHost {
    let mut task = task_with_file("T-OWNER", "Add a crate guide", "unused", "grok");
    task.execution_summary = "Outcome: success\nChanges:\n- added a crate guide".to_string();
    task.context_files = vec!["file:crates/new-crate/CLAUDE.md".to_string()];
    CommitTestHost::new(vec![task], workspace.to_path_buf())
}

fn owner_commit_input(workspace: &Path) -> Value {
    json!({
        "scope": "all",
        "job_run_id": "batch-1",
        "workspace_path": workspace,
    })
}

#[cfg(unix)]
fn symlink_at(workspace: &Path, path: &str, target: &str) {
    let path = workspace.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, &path).unwrap();
}

#[cfg(unix)]
fn assert_symlink_mode_120000(workspace: &Path, path: &str) {
    let listing = git_output(workspace, &["ls-tree", "HEAD", path]).unwrap();
    assert!(
        listing.starts_with("120000 blob "),
        "{path} must be committed as a symlink: {listing}"
    );
    let blob = listing
        .split_whitespace()
        .nth(2)
        .unwrap_or_else(|| panic!("{path} listing has no blob: {listing}"));
    assert_eq!(
        git_output(workspace, &["cat-file", "-p", blob]).unwrap(),
        "CLAUDE.md",
        "{path} must store the literal sibling target"
    );
    assert!(
        fs::symlink_metadata(workspace.join(path))
            .unwrap()
            .file_type()
            .is_symlink(),
        "{path} stays a worktree symlink after the commit"
    );
}

/// Owner delivery commits a new crate guide and its `AGENTS.md -> CLAUDE.md`
/// sibling, including when the guide is already tracked. The link is mode
/// 120000 and its blob is the literal target `CLAUDE.md`.
#[cfg(unix)]
fn owner_delivery_commits_crate_guide_symlinks_as_mode_120000() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::owner_delivery_commits_crate_guide_symlinks_as_mode_120000"
    )) {
        return;
    }

    let temp = claimed_worktree();
    let workspace = temp.path();
    write(workspace, "crates/new-crate/CLAUDE.md", "# guide\n");
    symlink_at(workspace, "crates/new-crate/AGENTS.md", "CLAUDE.md");
    git_commit(&owner_host(workspace), &owner_commit_input(workspace))
        .expect("a new crate guide symlink is delivered");
    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
        "crates/new-crate/AGENTS.md\ncrates/new-crate/CLAUDE.md"
    );
    assert_symlink_mode_120000(workspace, "crates/new-crate/AGENTS.md");
    let guide = git_output(
        workspace,
        &["ls-tree", "HEAD", "crates/new-crate/CLAUDE.md"],
    )
    .unwrap();
    assert!(
        guide.starts_with("100644 blob "),
        "the guide itself stays a regular file: {guide}"
    );

    let tracked = claimed_worktree();
    let workspace = tracked.path();
    write(workspace, "crates/new-crate/CLAUDE.md", "# guide\n");
    git_success(workspace, &["add", "crates/new-crate/CLAUDE.md"]).unwrap();
    git_success(workspace, &["commit", "-m", "guide"]).unwrap();
    symlink_at(workspace, "crates/new-crate/AGENTS.md", "CLAUDE.md");
    git_commit(&owner_host(workspace), &owner_commit_input(workspace))
        .expect("a symlink to an already tracked guide is delivered");
    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
        "crates/new-crate/AGENTS.md"
    );
    assert_symlink_mode_120000(workspace, "crates/new-crate/AGENTS.md");
}

/// Neighbouring irregular candidates stay on the existing refusal, and a
/// claimed leaf still refuses protected untracked paths. Nothing is staged.
#[cfg(unix)]
fn owner_delivery_still_refuses_neighbouring_irregular_paths() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::owner_delivery_still_refuses_neighbouring_irregular_paths"
    )) {
        return;
    }

    type OwnerRefusal = (&'static str, &'static str, fn(&Path));
    let cases: &[OwnerRefusal] = &[
        (
            "other symlink name",
            "crates/new-crate/README.md",
            |workspace| {
                write(workspace, "crates/new-crate/CLAUDE.md", "# guide\n");
                symlink_at(workspace, "crates/new-crate/README.md", "CLAUDE.md");
            },
        ),
        ("other directory", "docs/AGENTS.md", |workspace| {
            write(workspace, "docs/CLAUDE.md", "# guide\n");
            symlink_at(workspace, "docs/AGENTS.md", "CLAUDE.md");
        }),
        (
            "crate root is not a crate guide",
            "crates/AGENTS.md",
            |workspace| {
                write(workspace, "crates/CLAUDE.md", "# guide\n");
                symlink_at(workspace, "crates/AGENTS.md", "CLAUDE.md");
            },
        ),
        (
            "dot-slash link text",
            "crates/new-crate/AGENTS.md",
            |workspace| {
                write(workspace, "crates/new-crate/CLAUDE.md", "# guide\n");
                symlink_at(workspace, "crates/new-crate/AGENTS.md", "./CLAUDE.md");
            },
        ),
        (
            "other link text",
            "crates/new-crate/AGENTS.md",
            |workspace| {
                write(workspace, "crates/new-crate/README.md", "# readme\n");
                symlink_at(workspace, "crates/new-crate/AGENTS.md", "README.md");
            },
        ),
        (
            "absolute target",
            "crates/new-crate/AGENTS.md",
            |workspace| {
                write(workspace, "crates/new-crate/CLAUDE.md", "# guide\n");
                let target = workspace.join("crates/new-crate/CLAUDE.md");
                symlink_at(
                    workspace,
                    "crates/new-crate/AGENTS.md",
                    target.to_str().expect("utf-8 fixture path"),
                );
            },
        ),
        (
            "escaping sibling target",
            "crates/new-crate/AGENTS.md",
            |workspace| {
                write(workspace, "crates/CLAUDE.md", "# guide\n");
                symlink_at(workspace, "crates/new-crate/AGENTS.md", "../CLAUDE.md");
            },
        ),
        (
            "escaping absolute-looking relative target",
            "crates/new-crate/AGENTS.md",
            |workspace| {
                symlink_at(workspace, "crates/new-crate/AGENTS.md", "../../etc/passwd");
            },
        ),
        (
            "dangling target",
            "crates/new-crate/AGENTS.md",
            |workspace| {
                symlink_at(workspace, "crates/new-crate/AGENTS.md", "CLAUDE.md");
            },
        ),
        (
            "directory target",
            "crates/new-crate/AGENTS.md",
            |workspace| {
                fs::create_dir_all(workspace.join("crates/new-crate/CLAUDE.md")).unwrap();
                symlink_at(workspace, "crates/new-crate/AGENTS.md", "CLAUDE.md");
            },
        ),
        (
            "symlink target",
            "crates/new-crate/AGENTS.md",
            |workspace| {
                write(workspace, "crates/new-crate/guide.md", "# guide\n");
                symlink_at(workspace, "crates/new-crate/CLAUDE.md", "guide.md");
                symlink_at(workspace, "crates/new-crate/AGENTS.md", "CLAUDE.md");
            },
        ),
        (
            "gitignored guide is not part of the delivery",
            "crates/new-crate/AGENTS.md",
            |workspace| {
                write(workspace, ".gitignore", "crates/new-crate/CLAUDE.md\n");
                write(workspace, "crates/new-crate/CLAUDE.md", "# guide\n");
                symlink_at(workspace, "crates/new-crate/AGENTS.md", "CLAUDE.md");
            },
        ),
        ("nested repository", "nested/", |workspace| {
            let nested = workspace.join("nested");
            fs::create_dir_all(&nested).unwrap();
            git_success(&nested, &["init"]).unwrap();
            git_success(&nested, &["config", "user.email", "local@example.test"]).unwrap();
            git_success(&nested, &["config", "user.name", "Local User"]).unwrap();
            fs::write(nested.join("inside.txt"), "x\n").unwrap();
            git_success(&nested, &["add", "inside.txt"]).unwrap();
            git_success(&nested, &["commit", "-m", "nested"]).unwrap();
        }),
        (
            "tracked path replaced by a directory",
            "README.md",
            |workspace| {
                fs::remove_file(workspace.join("README.md")).unwrap();
                fs::create_dir(workspace.join("README.md")).unwrap();
                write(workspace, "README.md/inside.txt", "inside\n");
            },
        ),
        (".git component", "notes/.GIT", |workspace| {
            write(workspace, "notes/.GIT", "not a repository\n");
        }),
    ];

    for (label, needle, setup) in cases {
        let temp = claimed_worktree();
        let workspace = temp.path();
        let head = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
        setup(workspace);
        let before = untracked_status(workspace);
        let error = git_commit(&owner_host(workspace), &owner_commit_input(workspace))
            .expect_err(label)
            .to_string();
        assert!(
            error.contains("task delivery refused non-regular candidate paths"),
            "{label}: {error}"
        );
        assert!(
            error.contains(needle),
            "{label} must name {needle}: {error}"
        );
        assert_eq!(
            git_output(workspace, &["rev-parse", "HEAD"]).unwrap(),
            head,
            "{label} does not commit"
        );
        assert_eq!(
            untracked_status(workspace),
            before,
            "{label} leaves the worktree"
        );
    }

    for path in [".env", ".orbit/private.json", ".envrc", "nested/.GIT/HEAD"] {
        let temp = claimed_worktree();
        let workspace = temp.path();
        let head = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
        write(workspace, path, "secret\n");
        let before = untracked_status(workspace);
        let host = CommitTestHost::new(vec![claimed_task(&["dir:."])], workspace.to_path_buf())
            .with_claim_binding(CLAIMED_TASK_ID);
        let error = git_commit(&host, &claimed_commit_input(workspace))
            .expect_err(path)
            .to_string();
        assert!(
            error.contains("task delivery refused protected untracked paths"),
            "{path}: {error}"
        );
        assert!(error.contains(path), "{path} must be named: {error}");
        assert_eq!(
            git_output(workspace, &["rev-parse", "HEAD"]).unwrap(),
            head,
            "{path} does not commit"
        );
        assert_eq!(
            untracked_status(workspace),
            before,
            "{path} leaves the worktree"
        );
    }
}

#[test]
fn candidate_path_authority_contracts() {
    super::super::run_family(
        concat!(module_path!(), "::candidate_path_authority_contracts"),
        module_path!(),
        &[
            (
                "scratch_under_orbit_tmp_is_never_delivered",
                scratch_under_orbit_tmp_is_never_delivered,
            ),
            (
                "claimed_new_paths_outside_the_footprint_deliver_and_request_owner_widening",
                claimed_new_paths_outside_the_footprint_deliver_and_request_owner_widening,
            ),
            (
                "owner_delivery_commits_out_of_selector_paths_and_widens_with_provenance",
                owner_delivery_commits_out_of_selector_paths_and_widens_with_provenance,
            ),
            (
                "per_task_bundles_attribute_each_path_to_the_task_whose_agent_changed_it",
                per_task_bundles_attribute_each_path_to_the_task_whose_agent_changed_it,
            ),
            #[cfg(unix)]
            (
                "owner_new_path_validation_refuses_symlinks_and_widens_rename_destinations",
                owner_new_path_validation_refuses_symlinks_and_widens_rename_destinations,
            ),
            #[cfg(unix)]
            (
                "owner_delivery_commits_crate_guide_symlinks_as_mode_120000",
                owner_delivery_commits_crate_guide_symlinks_as_mode_120000,
            ),
            #[cfg(unix)]
            (
                "owner_delivery_still_refuses_neighbouring_irregular_paths",
                owner_delivery_still_refuses_neighbouring_irregular_paths,
            ),
        ],
    );
}
