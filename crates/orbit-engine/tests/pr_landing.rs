#![allow(missing_docs)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! PR landing and merge gating through the engine's deterministic actions.
//!
//! Each test drives the shipped `pr_open` and `pr_complete` actions through
//! [`execute_deterministic_action`]. The host keeps tasks in memory but does
//! not replace the provider boundary: the engine's own adapter runs `gh` and
//! `git` as it does in production. A substitute `gh` first on `PATH` stands in
//! for the forge. It serves one pull request whose head and base branches live
//! in a real bare remote. Each status read takes the next scripted check state.
//! A conditional merge request lands a real squash commit on the remote base.
//! The substitute enforces only the head-SHA condition, never branch
//! protection, so the engine is the only gate between a red check and a merge.
//!
//! `PATH`, `$HOME` and Git configuration are process-global, so each test body
//! re-runs in an isolated copy of this binary. That child has disposable
//! `$HOME`/`$TMPDIR`, no inherited Orbit, Git or GitHub authority, and a bounded
//! wait. The shared supervisor kills and reaps it on timeout.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError, process::run_bounded_capped, test_env};
use orbit_engine::{
    ReviewLandingRequest, RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate,
    execute_deterministic_action, review_gate,
};
use orbit_types::task::{Task, TaskComment, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Names the test whose body this process runs as the isolated child.
const CHILD_ENV: &str = "ORBIT_PR_LANDING_CHILD";
/// The parent-owned directory holding the substitute `gh` and its pointer to
/// the forge state of the case being run.
const SANDBOX_ENV: &str = "ORBIT_PR_LANDING_SANDBOX";
/// Upper bound on one isolated test body, including the engine's poll sleeps.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

const BASE: &str = "agent-main";
const BRANCH: &str = "orbit/landing-candidate";
const RUN_ID: &str = "jrun-landing";
const TASK_ID: &str = "T-LANDING";
const PR_NUMBER: &str = "42";

// ---------------------------------------------------------------------------
// Merge gating
// ---------------------------------------------------------------------------

/// Pending checks are waited out, never merged early. Once they pass, the
/// reviewed candidate is merged with a request conditional on its head SHA.
/// The merge is read back from the forge, the landing is recorded for review
/// coverage, and only then does the task reach `done`.
#[test]
fn passing_checks_merge_the_reviewed_candidate_and_record_its_delivery() {
    isolated(
        "passing_checks_merge_the_reviewed_candidate_and_record_its_delivery",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            fx.script_checks(&["pending", "success"]);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);

            let opened = action(
                &host,
                "pr_open",
                &fx.open_input(&fx.candidate, &fx.base_sha),
            )
            .expect("open the reviewed candidate");
            assert_eq!(opened["pr_created"], true);
            assert_eq!(opened["pr_number"], PR_NUMBER);
            assert_eq!(fx.forge_state("pr-head").as_deref(), Some(BRANCH));
            assert_eq!(fx.forge_state("pr-base").as_deref(), Some(BASE));
            // `pr_promote` hands the bundle to review before completion runs.
            host.set_status(TASK_ID, TaskStatus::Review);

            let completed = action(&host, "pr_complete", &fx.complete_input())
                .expect("complete once checks pass");

            let merge = &completed["merge"];
            assert_eq!(merge["merged"], true);
            assert!(
                merge["waited_seconds"].as_u64().unwrap() > 0,
                "the pending read must be waited out before merging: {merge}"
            );
            assert_eq!(
                fx.merge_requests(),
                vec![format!("sha={} merge_method=squash", fx.candidate)],
                "exactly one merge, conditional on the reviewed head"
            );
            assert!(
                fx.status_reads() >= 3,
                "pending, passing and merged states are each read from the forge"
            );

            let landed = fx.remote_tip(BASE);
            assert_ne!(landed, fx.base_sha, "the remote base advanced");
            assert_eq!(merge["landed_commit"], landed.as_str());
            assert_eq!(merge["managed_merge"], true);
            assert_eq!(merge["delivery_evidence"]["merge_commit"], landed.as_str());
            assert_eq!(
                merge["delivery_evidence"]["head_sha"],
                fx.candidate.as_str()
            );

            let landings = host.landings();
            assert_eq!(landings.len(), 1, "one review landing is recorded");
            let landing = &landings[0];
            assert_eq!(landing.run_id, RUN_ID);
            assert_eq!(landing.task_ids, vec![TASK_ID.to_string()]);
            assert_eq!(landing.pr_number, PR_NUMBER);
            assert_eq!(landing.base, BASE);
            assert_eq!(landing.reviewed_head_sha, fx.candidate);
            assert!(landing.managed_merge);
            assert_eq!(landing.landed_commit.as_deref(), Some(landed.as_str()));

            // The recorded landing is what Core checks coverage against: the
            // reviewed tree, squashed onto the reviewed base.
            review_gate::fetch_landed_commit(&fx.repo, &landed).expect("fetch landing");
            let landed_revision = review_gate::revision(&fx.repo, &landed).unwrap();
            let candidate_revision = review_gate::revision(&fx.repo, &fx.candidate).unwrap();
            let base_revision = review_gate::revision(&fx.repo, &fx.base_sha).unwrap();
            assert_eq!(landed_revision.tree, candidate_revision.tree);
            let facts = review_gate::landed_candidate_facts(
                &fx.repo,
                &landed_revision,
                &fx.candidate,
                &base_revision.tree,
                1,
            )
            .unwrap();
            assert_eq!(facts.base_at_landing, base_revision);
            assert!(!facts.is_candidate_commit);
            assert_eq!((facts.parents, facts.span_commits), (1, 1));

            assert_eq!(completed["completed_task_ids"], json!([TASK_ID]));
            assert_eq!(host.status(TASK_ID), TaskStatus::Done);
            let notes = host.completion_notes();
            assert_eq!(notes.len(), 1, "one guarded review -> done transition");
            assert!(
                notes[0].contains(&landed),
                "the done transition names the merge commit that permitted it: {}",
                notes[0]
            );
        },
    );
}

/// A required check that fails, before or after a wait, or an outstanding
/// required review, refuses completion before any merge request. The PR stays
/// open, the remote base does not move, and the task stays in `review`.
#[test]
fn a_failed_or_blocked_check_never_merges_and_the_task_stays_in_review() {
    isolated(
        "a_failed_or_blocked_check_never_merges_and_the_task_stays_in_review",
        |sandbox| {
            for (case, checks, refusal) in [
                (
                    "failed required check",
                    &["failure"][..],
                    "status check 'test' failed",
                ),
                (
                    "pending checks that then fail",
                    &["pending", "failure"][..],
                    "status check 'test' failed",
                ),
                (
                    "required review outstanding",
                    &["review_required"][..],
                    "REVIEW_REQUIRED",
                ),
            ] {
                let fx = Fixture::new(sandbox);
                fx.script_checks(checks);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .unwrap_or_else(|error| panic!("{case}: open: {error}"));
                host.set_status(TASK_ID, TaskStatus::Review);

                let error = action(&host, "pr_complete", &fx.complete_input())
                    .expect_err("a red or blocked PR must not complete");

                assert!(error.to_string().contains(refusal), "{case}: {error}");
                assert_eq!(
                    fx.status_reads(),
                    checks.len(),
                    "{case}: refused at the first red read"
                );
                assert!(
                    fx.merge_requests().is_empty(),
                    "{case}: no merge is requested"
                );
                assert_eq!(fx.forge_state("merged"), None, "{case}: PR stays open");
                assert_eq!(
                    fx.remote_tip(BASE),
                    fx.base_sha,
                    "{case}: the remote base is untouched"
                );
                assert!(host.landings().is_empty(), "{case}: no landing recorded");
                assert!(
                    host.completion_notes().is_empty(),
                    "{case}: no done transition is attempted"
                );
                assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{case}");
            }
        },
    );
}

// ---------------------------------------------------------------------------
// Reviewed-candidate binding at publication
// ---------------------------------------------------------------------------

/// `pr_open` publishes only the candidate the review gate settled. A
/// checkout that gained an unreviewed commit, or a candidate rebased onto a
/// base other than the reviewed one, is refused before the forge is asked
/// anything. The refusal is recorded on the task. Opening the same fixture with
/// the current head and base as the reviewed pair then succeeds, so each
/// refusal is caused by the mismatch alone.
#[test]
fn pr_open_refuses_a_head_or_base_other_than_the_reviewed_candidate() {
    isolated(
        "pr_open_refuses_a_head_or_base_other_than_the_reviewed_candidate",
        |sandbox| {
            for case in ["unreviewed commit on the head", "rebased onto another base"] {
                let fx = Fixture::new(sandbox);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                let (reviewed_head, reviewed_base) = match case {
                    "unreviewed commit on the head" => {
                        fx.commit("src/unreviewed.txt", "later edit\n");
                        (fx.candidate.clone(), fx.base_sha.clone())
                    }
                    _ => {
                        let reviewed_base = fx.base_sha.clone();
                        fx.advance_base_and_rebase();
                        (fx.head(), reviewed_base)
                    }
                };
                let base_sha = fx.local_tip(BASE);

                let mut input = fx.open_input(&reviewed_head, &reviewed_base);
                input["base_sha"] = json!(base_sha);
                let error = action(&host, "pr_open", &input)
                    .expect_err("a candidate other than the reviewed one must not publish");

                assert!(
                    error.to_string().contains("review_gate_stale"),
                    "{case}: {error}"
                );
                assert!(fx.forge_calls().is_empty(), "{case}: forge never asked");
                assert_eq!(fx.forge_state("pr-head"), None, "{case}: no PR exists");
                let comments = host.comments(TASK_ID);
                assert_eq!(comments.len(), 1, "{case}: one failed-handoff record");
                assert!(
                    comments[0].message.contains("[phase=stale-review-gate]"),
                    "{case}: {}",
                    comments[0].message
                );
                assert_eq!(host.status(TASK_ID), TaskStatus::InProgress, "{case}");

                let mut current = fx.open_input(&fx.head(), &base_sha);
                current["base_sha"] = json!(base_sha);
                let opened = action(&host, "pr_open", &current)
                    .unwrap_or_else(|error| panic!("{case}: current candidate: {error}"));
                assert_eq!(opened["pr_created"], true, "{case}");
                assert_eq!(fx.forge_state("pr-head").as_deref(), Some(BRANCH));
                assert_eq!(fx.forge_state("pr-base").as_deref(), Some(BASE));
            }
        },
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A checkout on [`BRANCH`] one commit ahead of [`BASE`], both pushed to a
/// bare remote that the substitute forge reads and merges into.
struct Fixture {
    _root: TempDir,
    repo: PathBuf,
    forge: PathBuf,
    base_sha: String,
    candidate: String,
}

impl Fixture {
    /// A fresh repository and forge, made the active forge for `gh`.
    fn new(sandbox: &Path) -> Self {
        let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
        let forge = root.path().join("forge");
        let remote = forge.join("remote.git");
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(root.path(), &["init", "--bare", path_str(&remote)]);
        git(&repo, &["init"]);
        git(&repo, &["checkout", "-b", BASE]);
        git(&repo, &["config", "user.name", "Orbit Test"]);
        git(
            &repo,
            &["config", "user.email", "orbit-test@example.invalid"],
        );
        fs::write(repo.join("README.md"), "base\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "base"]);
        git(&repo, &["remote", "add", "origin", path_str(&remote)]);
        git(&repo, &["push", "-u", "origin", BASE]);
        git(&repo, &["checkout", "-b", BRANCH]);
        let repo = repo.canonicalize().unwrap();
        let fixture = Self {
            _root: root,
            base_sha: git(&repo, &["rev-parse", BASE]),
            candidate: String::new(),
            repo,
            forge,
        };
        let candidate = fixture.commit("src/feature.txt", "reviewed change\n");
        fs::write(sandbox.join("active-forge"), path_str(&fixture.forge)).unwrap();
        fixture.script_checks(&["success"]);
        Self {
            candidate,
            ..fixture
        }
    }

    /// Check states the forge reports, one per status read; the last repeats.
    fn script_checks(&self, states: &[&str]) {
        fs::write(
            self.forge.join("checks"),
            format!("{}\n", states.join("\n")),
        )
        .unwrap();
    }

    /// Commit `file` on the checkout and push it, returning the new head.
    fn commit(&self, file: &str, contents: &str) -> String {
        let path = self.repo.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        git(&self.repo, &["add", file]);
        git(&self.repo, &["commit", "-m", &format!("write {file}")]);
        git(&self.repo, &["push", "-u", "origin", BRANCH]);
        self.head()
    }

    /// Land unrelated work on the base, then rebase and republish the
    /// candidate onto it.
    fn advance_base_and_rebase(&self) {
        git(&self.repo, &["checkout", BASE]);
        fs::write(self.repo.join("base.txt"), "advanced\n").unwrap();
        git(&self.repo, &["add", "base.txt"]);
        git(&self.repo, &["commit", "-m", "advance base"]);
        git(&self.repo, &["push", "origin", BASE]);
        git(&self.repo, &["checkout", BRANCH]);
        git(&self.repo, &["rebase", BASE]);
        git(&self.repo, &["push", "--force", "origin", BRANCH]);
    }

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn local_tip(&self, branch: &str) -> String {
        git(&self.repo, &["rev-parse", branch])
    }

    fn remote_tip(&self, branch: &str) -> String {
        git(
            &self.forge,
            &[
                "--git-dir",
                "remote.git",
                "rev-parse",
                &format!("refs/heads/{branch}"),
            ],
        )
    }

    fn open_input(&self, reviewed_head: &str, reviewed_base: &str) -> Value {
        json!({
            "workspace_path": self.repo,
            "job_run_id": RUN_ID,
            "completed_task_ids": [TASK_ID],
            "head": BRANCH,
            "base": BASE,
            "base_ref": BASE,
            "base_sha": self.base_sha,
            "base_sync": "local",
            "reviewed_head_sha": reviewed_head,
            "reviewed_base_sha": reviewed_base,
        })
    }

    /// The completion step as a `--complete` pipeline hands it the run's
    /// published and reviewed candidate.
    fn complete_input(&self) -> Value {
        json!({
            "workspace_path": self.repo,
            "job_run_id": RUN_ID,
            "completed_task_ids": [TASK_ID],
            "completion": "done",
            "pr_number": PR_NUMBER,
            "head": BRANCH,
            "base": BASE,
            "base_sync": "local",
            "published_head_sha": self.candidate,
            "reviewed_head_sha": self.candidate,
            "max_wait_seconds": 30,
            "poll_interval_seconds": 5,
        })
    }

    fn forge_state(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.forge.join(name))
            .ok()
            .map(|value| value.trim().to_string())
    }

    fn forge_lines(&self, name: &str) -> Vec<String> {
        self.forge_state(name)
            .map(|value| value.lines().map(ToOwned::to_owned).collect())
            .unwrap_or_default()
    }

    /// Every `gh` call, as its first two arguments.
    fn forge_calls(&self) -> Vec<String> {
        self.forge_lines("calls")
    }

    /// Every merge mutation the forge received, as its `sha=` and
    /// `merge_method=` fields.
    fn merge_requests(&self) -> Vec<String> {
        self.forge_lines("merge-requests")
    }

    fn status_reads(&self) -> usize {
        self.forge_state("reads")
            .map(|value| value.parse().unwrap())
            .unwrap_or(0)
            + usize::from(self.forge_state("merged-reads").is_some())
    }
}

fn action(host: &DeliveryHost, name: &str, input: &Value) -> Result<Value, OrbitError> {
    execute_deterministic_action(host, name, &json!({}), input, false, &HashMap::new(), None)
}

fn task(status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
        job_run_machine: None,
        id: TASK_ID.to_string(),
        title: "Land the reviewed candidate".to_string(),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: "Outcome: success\n\nChanges:\n- Reviewed change.".to_string(),
        context_files: Vec::new(),
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: Some(RUN_ID.to_string()),
        crew: None,
        orchestrator: None,
        created_at: now,
        updated_at: now,
    }
}

/// The task store and review-landing record the delivery actions write
/// through, kept in memory. The provider boundary is left to the engine.
struct DeliveryHost {
    repo: PathBuf,
    tasks: Mutex<BTreeMap<String, Task>>,
    comments: Mutex<BTreeMap<String, Vec<TaskComment>>>,
    completion_notes: Mutex<Vec<String>>,
    landings: Mutex<Vec<ReviewLandingRequest>>,
}

impl DeliveryHost {
    fn new(repo: &Path, status: TaskStatus) -> Self {
        Self {
            repo: repo.to_path_buf(),
            tasks: Mutex::new(BTreeMap::from([(TASK_ID.to_string(), task(status))])),
            comments: Mutex::default(),
            completion_notes: Mutex::default(),
            landings: Mutex::default(),
        }
    }

    fn set_status(&self, id: &str, status: TaskStatus) {
        self.tasks.lock().unwrap().get_mut(id).unwrap().status = status;
    }

    fn status(&self, id: &str) -> TaskStatus {
        self.tasks.lock().unwrap()[id].status
    }

    fn comments(&self, id: &str) -> Vec<TaskComment> {
        self.comments
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    fn completion_notes(&self) -> Vec<String> {
        self.completion_notes.lock().unwrap().clone()
    }

    fn landings(&self) -> Vec<ReviewLandingRequest> {
        self.landings.lock().unwrap().clone()
    }
}

impl RuntimeHost for DeliveryHost {
    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.tasks
            .lock()
            .unwrap()
            .get(task_id)
            .cloned()
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        Ok(self.comments(task_id))
    }

    /// The guarded transition: like the task store, refuse a write whose
    /// expected status is stale.
    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        if task.status != update.expected_status {
            return Err(OrbitError::Execution(format!(
                "task {task_id} is {} but the write expected {}",
                task.status, update.expected_status
            )));
        }
        task.status = update.status;
        if update.status == TaskStatus::Done {
            self.completion_notes
                .lock()
                .unwrap()
                .push(update.note.unwrap_or_default());
        }
        Ok(task.clone())
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        if let Some(status) = update.status {
            task.status = status;
        }
        self.comments
            .lock()
            .unwrap()
            .entry(task_id.to_string())
            .or_default()
            .extend(update.append_comments);
        Ok(())
    }

    fn record_review_landing(&self, request: &ReviewLandingRequest) -> Result<(), OrbitError> {
        self.landings.lock().unwrap().push(request.clone());
        Ok(())
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo.to_string_lossy().into_owned())
    }
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed in {}:\n{}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("utf8 fixture path")
}

// ---------------------------------------------------------------------------
// Substitute forge
// ---------------------------------------------------------------------------

/// The `gh` surface the engine's provider adapter calls, over one pull request
/// (#42). `__SANDBOX__/active-forge` names the forge directory of the running
/// case; its `remote.git` holds the real branches.
const FAKE_GH: &str = r#"#!/bin/sh
set -eu
forge=$(cat '__SANDBOX__/active-forge')
remote="$forge/remote.git"
url="https://github.com/orbit/test/pull/42"
export GIT_AUTHOR_NAME=Forge GIT_AUTHOR_EMAIL=forge@example.invalid
export GIT_COMMITTER_NAME=Forge GIT_COMMITTER_EMAIL=forge@example.invalid
printf '%s %s\n' "$1" "${2:-}" >> "$forge/calls"

head_sha() {
    git --git-dir="$remote" rev-parse "refs/heads/$(cat "$forge/pr-head")"
}

check() {
    case "$1" in
        pending) printf '{"__typename":"CheckRun","name":"test","status":"IN_PROGRESS","conclusion":""}' ;;
        success) printf '{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}' ;;
        failure) printf '{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE"}' ;;
    esac
}

status() {
    pr_head=$(cat "$forge/pr-head")
    pr_base=$(cat "$forge/pr-base")
    if [ -f "$forge/merged" ]; then
        touch "$forge/merged-reads"
        printf '{"number":42,"state":"MERGED","mergedAt":"2026-10-03T00:00:00Z","mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","reviewDecision":"","statusCheckRollup":[%s],"headRefName":"%s","headRefOid":"%s","baseRefName":"%s","mergeCommit":{"oid":"%s"},"url":"%s"}\n' \
            "$(check success)" "$pr_head" "$(head_sha)" "$pr_base" "$(cat "$forge/merged")" "$url"
        return
    fi
    reads=$(( $(cat "$forge/reads" 2>/dev/null || echo 0) + 1 ))
    printf '%s\n' "$reads" > "$forge/reads"
    observed=$(sed -n "${reads}p" "$forge/checks")
    [ -n "$observed" ] || observed=$(tail -n 1 "$forge/checks")
    review=""
    case "$observed" in
        pending) merge_state=BLOCKED; rollup=$(check pending) ;;
        success) merge_state=CLEAN; rollup=$(check success) ;;
        failure) merge_state=BLOCKED; rollup=$(check failure) ;;
        review_required) merge_state=BLOCKED; review=REVIEW_REQUIRED; rollup=$(check success) ;;
        *) echo "fake gh: unknown check state '$observed'" >&2; exit 2 ;;
    esac
    printf '{"number":42,"state":"OPEN","mergedAt":null,"mergeable":"MERGEABLE","mergeStateStatus":"%s","reviewDecision":"%s","statusCheckRollup":[%s],"headRefName":"%s","headRefOid":"%s","baseRefName":"%s","mergeCommit":null,"url":"%s"}\n' \
        "$merge_state" "$review" "$rollup" "$pr_head" "$(head_sha)" "$pr_base" "$url"
}

create() {
    shift 2
    while [ $# -gt 0 ]; do
        case "$1" in
            --base) printf '%s\n' "$2" > "$forge/pr-base"; shift 2 ;;
            --head) printf '%s\n' "$2" > "$forge/pr-head"; shift 2 ;;
            *) shift ;;
        esac
    done
    printf '%s\n' "$url"
}

merge() {
    sha=""
    method=""
    for arg in "$@"; do
        case "$arg" in
            sha=*) sha=${arg#sha=} ;;
            merge_method=*) method=${arg#merge_method=} ;;
        esac
    done
    printf 'sha=%s merge_method=%s\n' "$sha" "$method" >> "$forge/merge-requests"
    if [ "$sha" != "$(head_sha)" ]; then
        echo "gh: Head branch was modified. Review and try the merge again. (HTTP 409)" >&2
        exit 1
    fi
    base_ref="refs/heads/$(cat "$forge/pr-base")"
    parent=$(git --git-dir="$remote" rev-parse "$base_ref")
    tree=$(git --git-dir="$remote" rev-parse "$sha^{tree}")
    landed=$(git --git-dir="$remote" commit-tree "$tree" -p "$parent" -m "Squash pull request #42")
    git --git-dir="$remote" update-ref "$base_ref" "$landed" "$parent"
    printf '%s\n' "$landed" > "$forge/merged"
    printf '{"sha":"%s","merged":true,"message":"Pull Request successfully merged"}\n' "$landed"
}

case "$1 ${2:-}" in
    "pr list")
        if [ -f "$forge/pr-head" ]; then
            printf '[{"number":42,"title":"Landing","headRefName":"%s","author":{"login":"orbit"}}]\n' "$(cat "$forge/pr-head")"
        else
            echo '[]'
        fi ;;
    "pr create") create "$@" ;;
    "pr view")
        case "$*" in
            *mergeStateStatus*) status ;;
            *) printf '{"number":42,"title":"Landing","body":"","headRefName":"%s","files":[],"commits":[],"url":"%s"}\n' "$(cat "$forge/pr-head")" "$url" ;;
        esac ;;
    "repo view") echo '{"nameWithOwner":"orbit/test"}' ;;
    "api graphql")
        printf '{"data":{"repository":{"autoMergeAllowed":true,"mergeCommitAllowed":false,"rebaseMergeAllowed":true,"squashMergeAllowed":true,"pullRequest":{"baseRefName":"%s","baseRef":{"branchProtectionRule":{"requiresLinearHistory":true}}}}}}\n' "$(cat "$forge/pr-base")" ;;
    "api repos/{owner}/{repo}/pulls/42/merge") merge "$@" ;;
    *) echo "fake gh: unsupported call: $*" >&2; exit 2 ;;
esac
"#;

// ---------------------------------------------------------------------------
// Isolated child process
// ---------------------------------------------------------------------------

/// Run `body` in a copy of this test binary whose `PATH` starts with the
/// substitute `gh`, and fail if it fails or outlives [`CHILD_DEADLINE`].
fn isolated(test: &str, body: impl FnOnce(&Path)) {
    if std::env::var(CHILD_ENV).as_deref() == Ok(test) {
        let sandbox = PathBuf::from(std::env::var_os(SANDBOX_ENV).expect("sandbox path"));
        body(&sandbox);
        return;
    }
    let sandbox = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let home = sandbox.path().join("home");
    let tmp = sandbox.path().join("tmp");
    let bin = sandbox.path().join("bin");
    for dir in [&home, &tmp, &bin] {
        fs::create_dir_all(dir).unwrap();
    }
    let gh = bin.join("gh");
    fs::write(
        &gh,
        FAKE_GH.replace("__SANDBOX__", path_str(sandbox.path())),
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = vec![bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));

    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("ORBIT_") || name.starts_with("GIT_") || name.starts_with("GH_") {
            command.env_remove(name.as_ref());
        }
    }
    command
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .current_dir(sandbox.path())
        .env_remove("GITHUB_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN")
        .env(CHILD_ENV, test)
        .env(SANDBOX_ENV, sandbox.path())
        .env("PATH", std::env::join_paths(path).unwrap())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("TMPDIR", &tmp)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = run_bounded_capped(&mut command, CHILD_DEADLINE, 256 * 1024)
        .unwrap_or_else(|error| panic!("{test} did not finish in its isolated child: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "{test} failed in its isolated child ({}):\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
