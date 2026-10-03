use std::fs;
use std::path::Path;
use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_types::task::Task;
use orbit_types::workflow::handoff::TaskHandoff;
use serde_json::{Value, json};

use super::super::git_commit;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};
use crate::context::{ClaimExecutionContext, RuntimeHost};
use crate::executor::automation::vcs::claim::{claim_handoff, claim_validate};
use crate::executor::automation::vcs::git::{git_output, git_success};

#[test]
fn singleton_refuses_unknown_untracked_paths_without_mutating_files_or_index() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    fs::write(workspace.join("README.md"), "intended tracked edit\n").unwrap();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/new.rs"), "pub fn intended() {}\n").unwrap();
    git_success(workspace, &["add", "--", "src/new.rs"]).unwrap();
    fs::write(workspace.join("screenshot.png"), b"scratch screenshot").unwrap();
    fs::write(workspace.join("browser-path.txt"), b"/tmp/browser").unwrap();

    let mut task = task_with_file("T1", "Deliver source only", "README.md", "codex");
    task.context_files.clear();
    task.execution_summary = "Outcome: success\nChanges:\n- Source updated.".to_string();
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());
    let index_before = git_output(workspace, &["diff", "--cached", "--binary"]).unwrap();
    let readme_before = fs::read(workspace.join("README.md")).unwrap();
    let new_source_before = fs::read(workspace.join("src/new.rs")).unwrap();
    let screenshot_before = fs::read(workspace.join("screenshot.png")).unwrap();
    let marker_before = fs::read(workspace.join("browser-path.txt")).unwrap();
    let head_before = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();

    let error = git_commit(
        &host,
        &json!({
            "scope": "all",
            "job_run_id": "batch-1",
            "workspace_path": workspace,
        }),
    )
    .expect_err("unknown untracked evidence must refuse delivery");
    let message = error.to_string();
    assert!(message.contains("browser-path.txt"), "{message}");
    assert!(message.contains("screenshot.png"), "{message}");
    assert!(message.contains("exact `file:` task selector"), "{message}");

    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).unwrap(),
        head_before
    );
    assert_eq!(
        git_output(workspace, &["diff", "--cached", "--binary"]).unwrap(),
        index_before
    );
    assert_eq!(
        fs::read(workspace.join("README.md")).unwrap(),
        readme_before
    );
    assert_eq!(
        fs::read(workspace.join("src/new.rs")).unwrap(),
        new_source_before
    );
    assert_eq!(
        fs::read(workspace.join("screenshot.png")).unwrap(),
        screenshot_before
    );
    assert_eq!(
        fs::read(workspace.join("browser-path.txt")).unwrap(),
        marker_before
    );
}

#[test]
fn singleton_commits_tracked_changes_and_exactly_declared_new_source() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    for (path, contents) in [
        ("delete.txt", "delete me\n"),
        ("rename-from.txt", "rename me\n"),
    ] {
        fs::write(workspace.join(path), contents).unwrap();
    }
    git_success(workspace, &["add", "--", "delete.txt", "rename-from.txt"]).unwrap();
    git_success(workspace, &["commit", "-m", "tracked fixtures"]).unwrap();

    fs::write(workspace.join("README.md"), "tracked edit\n").unwrap();
    fs::remove_file(workspace.join("delete.txt")).unwrap();
    git_success(workspace, &["mv", "--", "rename-from.txt", "rename-to.txt"]).unwrap();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/new.rs"), "pub fn added() {}\n").unwrap();

    let mut task = task_with_file("T1", "Candidate paths", "README.md", "codex");
    task.context_files.push("file:src/new.rs".to_string());
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf());
    git_commit(
        &host,
        &json!({
            "scope": "all",
            "job_run_id": "batch-1",
            "workspace_path": workspace,
        }),
    )
    .expect("the explicit candidate is committed");

    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-status", "HEAD"]).unwrap(),
        "M\tREADME.md\nD\tdelete.txt\nR100\trename-from.txt\trename-to.txt\nA\tsrc/new.rs"
    );
    assert!(
        git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn per_task_commits_only_each_tasks_explicit_candidate_paths() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    fs::create_dir_all(workspace.join("one")).unwrap();
    fs::create_dir_all(workspace.join("two")).unwrap();
    fs::write(workspace.join("one/new.rs"), "pub fn one() {}\n").unwrap();
    fs::write(workspace.join("two/new.rs"), "pub fn two() {}\n").unwrap();

    let mut one = task_with_file("T1", "First candidate", "one/new.rs", "codex");
    one.context_files = vec!["file:one/new.rs".to_string()];
    let mut two = task_with_file("T2", "Second candidate", "two/new.rs", "claude");
    two.context_files = vec!["file:two/new.rs".to_string()];
    let host = CommitTestHost::new(vec![one, two], workspace.to_path_buf());

    let result = git_commit(
        &host,
        &json!({
            "scope": "per_task",
            "job_run_id": "batch-1",
            "workspace_path": workspace,
            "completed_task_ids": ["T1", "T2"],
        }),
    )
    .expect("both explicitly scoped task candidates are committed");

    assert_eq!(result["committed_task_ids"], json!(["T1", "T2"]));
    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-only", "HEAD^"]).unwrap(),
        "one/new.rs"
    );
    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
        "two/new.rs"
    );
}

#[test]
fn per_task_refuses_ambiguous_or_unowned_candidates_before_index_mutation() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    fs::create_dir_all(workspace.join("shared")).unwrap();
    fs::create_dir_all(workspace.join("orphan")).unwrap();
    fs::write(workspace.join("shared/new.rs"), "pub fn shared() {}\n").unwrap();
    fs::write(workspace.join("orphan/new.rs"), "pub fn orphan() {}\n").unwrap();
    git_success(workspace, &["add", "--", "shared/new.rs", "orphan/new.rs"]).unwrap();

    let mut one = task_with_file("T1", "First candidate", "shared", "codex");
    one.context_files = vec!["dir:shared".to_string()];
    let mut two = task_with_file("T2", "Second candidate", "shared", "claude");
    two.context_files = vec!["dir:shared".to_string()];
    let host = CommitTestHost::new(vec![one, two], workspace.to_path_buf());
    let index_before = git_output(workspace, &["diff", "--cached", "--binary"]).unwrap();

    let error = git_commit(
        &host,
        &json!({
            "scope": "per_task",
            "job_run_id": "batch-1",
            "workspace_path": workspace,
            "completed_task_ids": ["T1", "T2"],
        }),
    )
    .expect_err("candidate ownership must be deterministic");
    let message = error.to_string();
    assert!(message.contains("orphan/new.rs"), "{message}");
    assert!(message.contains("shared/new.rs"), "{message}");
    assert!(
        message.contains("T1") && message.contains("T2"),
        "{message}"
    );
    assert_eq!(
        git_output(workspace, &["diff", "--cached", "--binary"]).unwrap(),
        index_before
    );
    assert_eq!(
        git_output(workspace, &["rev-list", "--count", "HEAD"]).unwrap(),
        "1"
    );
}

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

/// ORB-13756: a claimed worker cannot append selectors (the footprint is
/// frozen) or stage on a writable index, so a module split whose new files sit
/// under an admitted `dir:` selector was refused at `commit` however it went.
/// The claim's footprint is its new-path intent: the file is committed and the
/// candidate validates and hands off.
#[test]
fn claimed_run_commits_and_hands_off_a_new_file_under_an_admitted_dir_selector() {
    let temp = claimed_worktree();
    let workspace = temp.path();
    write(workspace, "src/split/new.rs", "pub fn split() {}\n");
    write(
        workspace,
        "src/split/nested/deeper.rs",
        "pub fn deeper() {}\n",
    );
    write(workspace, "README.md", "tracked edit\n");

    let host = CommitTestHost::new(
        vec![claimed_task(&["dir:src/split", "file:README.md"])],
        workspace.to_path_buf(),
    )
    .with_claim_binding(CLAIMED_TASK_ID);
    let result = git_commit(&host, &claimed_commit_input(workspace))
        .expect("new files inside the admitted footprint are delivered");
    assert_eq!(result["committed"], json!(true));
    assert_eq!(
        git_output(workspace, &["show", "--format=", "--name-status", "HEAD"]).unwrap(),
        "M\tREADME.md\nA\tsrc/split/nested/deeper.rs\nA\tsrc/split/new.rs"
    );
    assert!(untracked_status(workspace).is_empty());

    let claim = ClaimLeafHost::default();
    let mut input = json!({"workspace_path": workspace});
    let validated = claim_validate(&claim, &input).expect("the committed candidate validates");
    input["candidate"] = validated["candidate"].clone();
    input["validation"] = validated["validation"].clone();
    let handed = claim_handoff(&claim, &input).expect("the committed candidate hands off");
    assert_eq!(handed["handed_off"], json!(true));
    let handoff = claim
        .handoff
        .lock()
        .unwrap()
        .clone()
        .expect("typed handoff recorded");
    assert_eq!(
        handoff.candidate.candidate.commit,
        git_output(workspace, &["rev-parse", "HEAD"]).unwrap(),
        "the handoff names the commit that carries the new files"
    );
}

/// ORB-13756: the footprint admits new paths without widening it. A path
/// outside every admitted selector still refuses delivery before any index
/// change, and the refusal names only that path.
#[test]
fn claimed_run_refuses_an_untracked_path_outside_its_footprint_before_index_change() {
    let temp = claimed_worktree();
    let workspace = temp.path();
    write(workspace, "src/split/new.rs", "pub fn split() {}\n");
    write(
        workspace,
        "src/splitter.rs",
        "sibling prefix, not inside the dir\n",
    );
    write(workspace, "notes/screenshot.png", "scratch");
    write(workspace, "README.md", "tracked edit\n");

    let host = CommitTestHost::new(
        vec![claimed_task(&["dir:src/split", "file:README.md"])],
        workspace.to_path_buf(),
    )
    .with_claim_binding(CLAIMED_TASK_ID);
    let head_before = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
    let index_before = git_output(workspace, &["diff", "--cached", "--binary"]).unwrap();
    let status_before = untracked_status(workspace);

    let message = git_commit(&host, &claimed_commit_input(workspace))
        .expect_err("a path outside the footprint refuses delivery")
        .to_string();
    assert!(message.contains("notes/screenshot.png"), "{message}");
    assert!(message.contains("src/splitter.rs"), "{message}");
    assert!(!message.contains("src/split/new.rs"), "{message}");
    assert!(message.contains("frozen footprint"), "{message}");
    assert!(
        message.contains("Orbit did not change the index"),
        "{message}"
    );
    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).unwrap(),
        head_before
    );
    assert_eq!(
        git_output(workspace, &["diff", "--cached", "--binary"]).unwrap(),
        index_before
    );
    assert_eq!(untracked_status(workspace), status_before);
}

/// ORB-13756: footprint admission is the claim's, carried by its trusted
/// worker binding. A local run, or a binding for another task, keeps the
/// exact-`file:` contract and is refused before any index change.
#[test]
fn dir_selectors_admit_no_new_path_without_the_claims_binding() {
    for binding in [None, Some("T-OTHER")] {
        let temp = claimed_worktree();
        let workspace = temp.path();
        write(workspace, "src/split/new.rs", "pub fn split() {}\n");
        let mut task = claimed_task(&["dir:src/split"]);
        task.execution_summary = "Outcome: success\nChanges:\n- split".to_string();
        let mut host = CommitTestHost::new(vec![task], workspace.to_path_buf());
        if let Some(task_id) = binding {
            host = host.with_claim_binding(task_id);
        }

        let message = git_commit(&host, &claimed_commit_input(workspace))
            .expect_err("a directory selector is not local new-file intent")
            .to_string();
        assert!(
            message.contains("src/split/new.rs"),
            "{binding:?}: {message}"
        );
        assert!(
            message.contains("exact `file:` task selector"),
            "{binding:?}: {message}"
        );
        assert_eq!(
            git_output(workspace, &["rev-list", "--count", "HEAD"]).unwrap(),
            "1"
        );
        assert!(
            git_output(workspace, &["diff", "--cached", "--name-only"])
                .unwrap()
                .is_empty()
        );
    }
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

/// Records claim validation logs and the typed handoff, so the claimed leaf's
/// `claim_validate` and `claim_handoff` steps run over a committed fixture.
#[derive(Default)]
struct ClaimLeafHost {
    handoff: Mutex<Option<TaskHandoff>>,
}

impl RuntimeHost for ClaimLeafHost {
    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError> {
        Ok(ClaimExecutionContext {
            workspace_id: "owner-workspace".into(),
            task_id: CLAIMED_TASK_ID.into(),
            claim_id: "claim-1".into(),
            machine_id: "follower-machine".into(),
            run_id: "batch-1".into(),
            ship_mode: "local".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            required_commands: vec!["true".into()],
        })
    }

    fn attach_claim_validation_log(
        &self,
        _path: &str,
        _content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    fn record_claim_handoff(&self, handoff: &TaskHandoff) -> Result<(), OrbitError> {
        *self.handoff.lock().unwrap() = Some(handoff.clone());
        Ok(())
    }
}
