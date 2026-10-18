//! [ORB-15308] A re-claimed task whose earlier claim already opened a pull
//! request republishes through that pull request, or closes it for the new
//! one, and never leaves two open. A fresh attempt that resumes nothing
//! closes the task's earlier open pull requests the same way.
//!
//! Each test drives the shipped `candidate_resume`, `git_push` and `pr_open`
//! actions in order, as the claimed PR leaf runs them, through
//! [`execute_deterministic_action`]. Git runs for real against a bare
//! remote, and a substitute `gh` first on `PATH` serves any number of pull
//! requests from files beside it, so the engine's own provider adapter is
//! what lists, creates, views and closes them. The earlier claim's retained
//! worktree sits in the same repository, as it does on the host that made
//! the candidate. `PATH` is process-global, so each body runs in an isolated
//! copy of this binary.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError, test_env};
use orbit_engine::{PrConfig, TaskAutomationUpdate, execute_deterministic_action};
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use super::git_fixture;

const CHILD_ENV: &str = "ORBIT_CLAIMED_PR_REUSE_CHILD";
const SANDBOX_ENV: &str = "ORBIT_CLAIMED_PR_REUSE_SANDBOX";
const BASE: &str = "main";
const TASK_ID: &str = "T-REUSE";
/// The re-claim's leaf run.
const RUN_ID: &str = "jrun-reuse-next";
/// The earlier claim's published branch, and the one its pull request heads.
const EARLIER: &str = "orbit/T-REUSE-aaaaaaaa";
/// The branch the re-claim's worktree starts on, named for its own run.
const OURS: &str = "orbit/T-REUSE-bbbbbbbb";
const PRIOR_PR: &str = "41";

/// A fresh attempt of a re-admitted task: the earlier attempt's `[BLOCKED]`
/// failure-handoff pull request #41 is still open, and the new run resumes
/// no candidate, so nothing names #41 to it. Its `pr_open` opens the new pull
/// request and closes #41 with a comment naming it, so the task has exactly
/// one open pull request. A human's pull request on the task's branch prefix,
/// another task's pull request and a merged one are left alone, and a retried
/// `pr_open` closes nothing more.
fn a_fresh_attempt_closes_the_tasks_earlier_open_pull_request() {
    isolated(
        "a_fresh_attempt_closes_the_tasks_earlier_open_pull_request",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            fx.describe_pr(PRIOR_PR, None, &format!("Blocked delivery of {TASK_ID}."));
            fx.add_pr(
                "50",
                "orbit/T-REUSE-cccccccc",
                "OPEN",
                Some("someone"),
                TASK_ID,
            );
            fx.add_pr("51", "orbit/T-OTHER-dddddddd", "OPEN", None, "T-OTHER");
            fx.add_pr("52", "orbit/T-REUSE-eeeeeeee", "MERGED", None, TASK_ID);
            let host = Host::closing_superseded(&fx.repo);

            fs::write(fx.claim.join("fresh.txt"), "fresh attempt\n").unwrap();
            git(&fx.claim, &["add", "fresh.txt"]);
            git(&fx.claim, &["commit", "-q", "-m", "fresh attempt"]);
            let pushed = action(&host, "git_push", &fx.push_input(OURS, &Value::Null)).unwrap();
            assert_eq!(pushed["decision"], "performed_create", "{pushed}");

            let opened = action(&host, "pr_open", &fx.open_input(OURS, &Value::Null)).unwrap();
            assert_eq!(opened["decision"], "performed", "{opened}");
            let replacement = opened["pr_number"].as_str().unwrap().to_string();
            assert!(opened["superseded_pull_request"].is_null(), "{opened}");
            let closed = opened["closed_stale_pull_requests"].as_array().unwrap();
            assert_eq!(closed.len(), 1, "{opened}");
            assert_eq!(closed[0]["number"], PRIOR_PR, "{opened}");
            assert_eq!(closed[0]["decision"], "closed", "{opened}");
            assert!(
                !fx.pr_file(PRIOR_PR, "comment").is_empty(),
                "the earlier pull request is closed with a comment"
            );
            let mut expected = vec!["50".to_string(), "51".to_string(), replacement];
            expected.sort();
            assert_eq!(
                fx.open_prs(),
                expected,
                "only the task's earlier Orbit pull request is closed"
            );
            assert_eq!(fx.pr_file("52", "state"), "MERGED");
            assert_eq!(
                fx.remote_tip(EARLIER),
                fx.earlier_head,
                "the closed pull request's branch is kept"
            );

            let again = action(&host, "pr_open", &fx.open_input(OURS, &Value::Null)).unwrap();
            assert_eq!(again["decision"], "reused", "{again}");
            assert_eq!(again["closed_stale_pull_requests"], json!([]), "{again}");
            let closes = fx.calls().iter().filter(|call| *call == "pr close").count();
            assert_eq!(closes, 1, "{:?}", fx.calls());
        },
    );
}

/// The earlier claim pushed its candidate and opened #41, then failed in its
/// before-landing review. The re-claim resumes it on a moved base: its
/// branch becomes the candidate's, the push replaces the published head
/// under a lease on it, and `pr_open` reuses #41 rather than opening another.
/// The earlier claim's retained worktree keeps its checkout and commits on a
/// branch renamed aside.
fn a_resumed_published_candidate_republishes_through_its_open_pull_request() {
    isolated(
        "a_resumed_published_candidate_republishes_through_its_open_pull_request",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = Host::new(&fx.repo);

            let resumed = action(&host, "candidate_resume", &fx.resume_input()).unwrap();
            assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
            assert_eq!(resumed["repair"]["trigger"], "continuation", "{resumed}");
            assert_eq!(resumed["prior_pull_request"], PRIOR_PR, "{resumed}");
            assert_eq!(resumed["reused_branch"], EARLIER, "{resumed}");
            assert_eq!(resumed["reused_head_sha"], fx.earlier_head, "{resumed}");
            assert!(resumed["branch_reuse_refused"].is_null(), "{resumed}");
            assert_eq!(
                git(&fx.claim, &["symbolic-ref", "--short", "HEAD"]),
                EARLIER
            );
            let aside = format!("{EARLIER}-superseded-bbbbbbbb");
            assert_eq!(
                git(&fx.earlier, &["symbolic-ref", "--short", "HEAD"]),
                aside,
                "the retained worktree keeps its checkout on a branch renamed aside"
            );
            assert_eq!(git(&fx.earlier, &["rev-parse", "HEAD"]), fx.earlier_head);

            let head = fx.continue_and_commit();
            let pushed = action(&host, "git_push", &fx.push_input(EARLIER, &resumed)).unwrap();
            assert_eq!(pushed["decision"], "performed_force_with_lease", "{pushed}");
            assert_eq!(
                fx.remote_tip(EARLIER),
                head,
                "the published head is replaced"
            );

            let opened = action(&host, "pr_open", &fx.open_input(EARLIER, &resumed)).unwrap();
            assert_eq!(opened["decision"], "reused", "{opened}");
            assert_eq!(opened["pr_number"], PRIOR_PR, "{opened}");
            assert!(opened["superseded_pull_request"].is_null(), "{opened}");
            assert!(
                !fx.calls().iter().any(|call| call == "pr create"),
                "no second pull request is created: {:?}",
                fx.calls()
            );
            assert_eq!(fx.open_prs(), vec![PRIOR_PR.to_string()]);
        },
    );
}

/// The re-claim cannot take over the earlier branch: this host's copy of it
/// holds a commit that was never published, which is never moved. The leaf
/// publishes on its own branch, and `pr_open` closes #41 with a comment
/// naming the pull request that replaced it.
fn an_unreusable_branch_closes_the_earlier_pull_request_for_the_new_one() {
    isolated(
        "an_unreusable_branch_closes_the_earlier_pull_request_for_the_new_one",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            fs::write(fx.earlier.join("notes.txt"), "never pushed\n").unwrap();
            git(&fx.earlier, &["add", "notes.txt"]);
            git(&fx.earlier, &["commit", "-q", "-m", "local only"]);
            let unpublished = git(&fx.earlier, &["rev-parse", "HEAD"]);
            let host = Host::new(&fx.repo);

            let resumed = action(&host, "candidate_resume", &fx.resume_input()).unwrap();
            assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
            assert!(resumed["reused_branch"].is_null(), "{resumed}");
            assert!(resumed["reused_head_sha"].is_null(), "{resumed}");
            let refused = resumed["branch_reuse_refused"].as_str().unwrap_or_default();
            assert!(refused.contains(&unpublished), "{resumed}");
            assert_eq!(git(&fx.claim, &["symbolic-ref", "--short", "HEAD"]), OURS);
            assert_eq!(
                git(&fx.earlier, &["rev-parse", EARLIER]),
                unpublished,
                "a branch holding unpublished work is left where it is"
            );

            fx.continue_and_commit();
            let pushed = action(&host, "git_push", &fx.push_input(OURS, &resumed)).unwrap();
            assert_eq!(pushed["decision"], "performed_create", "{pushed}");
            assert_eq!(fx.remote_tip(EARLIER), fx.earlier_head, "untouched");

            let opened = action(&host, "pr_open", &fx.open_input(OURS, &resumed)).unwrap();
            assert_eq!(opened["decision"], "performed", "{opened}");
            let replacement = opened["pr_number"].as_str().unwrap().to_string();
            assert_ne!(replacement, PRIOR_PR);
            let superseded = &opened["superseded_pull_request"];
            assert_eq!(superseded["number"], PRIOR_PR, "{opened}");
            assert_eq!(superseded["decision"], "closed", "{opened}");
            assert_eq!(fx.open_prs(), vec![replacement.clone()]);
            let comment = fx.pr_file(PRIOR_PR, "comment");
            assert!(
                comment.contains(&format!("Superseded by #{replacement}")),
                "the closing comment names the superseding pull request: {comment}"
            );

            // A retried `pr_open` reuses its own pull request and leaves the
            // closed one alone.
            let again = action(&host, "pr_open", &fx.open_input(OURS, &resumed)).unwrap();
            assert_eq!(again["decision"], "reused", "{again}");
            assert_eq!(again["superseded_pull_request"]["decision"], "not_open");
            let closes = fx.calls().iter().filter(|call| *call == "pr close").count();
            assert_eq!(closes, 1, "{:?}", fx.calls());
        },
    );
}

/// The earlier branch gained a commit after the published head, and the base
/// moved again while the re-claim implemented. `pr_prepare` observes the
/// advanced remote and `git_rebase` reports a fresh rewrite checkpoint that
/// names it, which the ordinary push would accept. The adopted lease is on
/// the published head, so the push refuses and the newer commit stays.
fn an_advanced_adopted_branch_is_not_overwritten_after_a_base_rebase() {
    isolated(
        "an_advanced_adopted_branch_is_not_overwritten_after_a_base_rebase",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = Host::new(&fx.repo);
            let resumed = action(&host, "candidate_resume", &fx.resume_input()).unwrap();
            fx.continue_and_commit();
            let concurrent = fx.push_remote_only_commit();
            fx.advance_base();

            let synced = fx.sync(&host);
            assert_eq!(synced["rewritten"], true, "{synced}");
            assert_eq!(synced["remote_sha_before"], concurrent, "{synced}");
            let refused = action(&host, "git_push", &fx.synced_push_input(&resumed, &synced))
                .expect_err("the advanced branch is not replaced")
                .to_string();
            assert!(refused.contains("push_lease_lost"), "{refused}");
            assert!(refused.contains(&concurrent), "{refused}");
            assert!(refused.contains(&fx.earlier_head), "{refused}");
            assert_eq!(
                fx.remote_tip(EARLIER),
                concurrent,
                "the newer head survives"
            );
        },
    );
}

/// A branch deleted on the remote since the published head is not recreated
/// by the resumed candidate.
fn a_deleted_adopted_branch_is_not_recreated() {
    isolated("a_deleted_adopted_branch_is_not_recreated", |sandbox| {
        let fx = Fixture::new(sandbox);
        let host = Host::new(&fx.repo);
        let resumed = action(&host, "candidate_resume", &fx.resume_input()).unwrap();
        fx.continue_and_commit();
        git(&fx.repo, &["push", "-q", "origin", "--delete", EARLIER]);
        fx.advance_base();

        let synced = fx.sync(&host);
        let refused = action(&host, "git_push", &fx.synced_push_input(&resumed, &synced))
            .expect_err("the deleted branch is not recreated")
            .to_string();
        assert!(refused.contains("push_lease_lost"), "{refused}");
        assert!(refused.contains("missing"), "{refused}");
        assert!(refused.contains(&fx.earlier_head), "{refused}");
        let refs = git(
            &fx.forge.join("remote.git"),
            &["for-each-ref", &format!("refs/heads/{EARLIER}")],
        );
        assert!(refs.is_empty(), "the branch stays deleted: {refs}");
    });
}

/// A remote still at the published head is replaced under the lease on it
/// even when the base moved, and a retried push finds its own head published.
fn an_unmoved_adopted_branch_is_replaced_after_a_base_rebase() {
    isolated(
        "an_unmoved_adopted_branch_is_replaced_after_a_base_rebase",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = Host::new(&fx.repo);
            let resumed = action(&host, "candidate_resume", &fx.resume_input()).unwrap();
            fx.continue_and_commit();
            fx.advance_base();

            let synced = fx.sync(&host);
            assert_eq!(synced["rewritten"], true, "{synced}");
            assert_eq!(synced["remote_sha_before"], fx.earlier_head, "{synced}");
            let input = fx.synced_push_input(&resumed, &synced);
            let pushed = action(&host, "git_push", &input).unwrap();
            assert_eq!(pushed["decision"], "performed_force_with_lease", "{pushed}");
            let rebased = synced["head_sha"].as_str().unwrap();
            assert_eq!(fx.remote_tip(EARLIER), rebased);

            let retried = action(&host, "git_push", &input).unwrap();
            assert_eq!(retried["decision"], "reused_current", "{retried}");
            assert_eq!(fx.remote_tip(EARLIER), rebased);
        },
    );
}

/// A remote still at the published head fast-forwards under the lease on it,
/// and a retried push finds the candidate published.
fn an_unmoved_adopted_branch_fast_forwards_under_its_published_lease() {
    isolated(
        "an_unmoved_adopted_branch_fast_forwards_under_its_published_lease",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = Host::new(&fx.repo);
            let resumed = action(&host, "candidate_resume", &fx.resume_input()).unwrap();
            let head = fx.continue_from_published_head();

            let pushed = action(&host, "git_push", &fx.push_input(EARLIER, &resumed)).unwrap();
            assert_eq!(pushed["decision"], "performed_fast_forward", "{pushed}");
            assert_eq!(pushed["force_with_lease"], true, "{pushed}");
            assert_eq!(fx.remote_tip(EARLIER), head);

            let retried = action(&host, "git_push", &fx.push_input(EARLIER, &resumed)).unwrap();
            assert_eq!(retried["decision"], "reused_current", "{retried}");
            assert_eq!(fx.remote_tip(EARLIER), head);
        },
    );
}

/// The adopted branch is deleted after the engine observed it, before its
/// fast-forward push starts. The lease refuses the push, and the branch stays
/// deleted.
fn a_fast_forward_does_not_recreate_an_adopted_branch_deleted_after_observation() {
    isolated(
        "a_fast_forward_does_not_recreate_an_adopted_branch_deleted_after_observation",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = Host::new(&fx.repo);
            let resumed = action(&host, "candidate_resume", &fx.resume_input()).unwrap();
            fx.continue_from_published_head();
            fx.change_before_push(Change::Delete);

            action(&host, "git_push", &fx.push_input(EARLIER, &resumed))
                .expect_err("the push is refused under the published-head lease");
            let refs = git(
                &fx.forge.join("remote.git"),
                &["for-each-ref", &format!("refs/heads/{EARLIER}")],
            );
            assert!(refs.is_empty(), "the branch stays deleted: {refs}");
        },
    );
}

/// The adopted branch is rewound to an ancestor of the local candidate after
/// the engine observed it, before its fast-forward push starts. The lease
/// refuses the push, and the rewound head survives.
fn a_fast_forward_does_not_move_an_adopted_branch_rewound_after_observation() {
    isolated(
        "a_fast_forward_does_not_move_an_adopted_branch_rewound_after_observation",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = Host::new(&fx.repo);
            let resumed = action(&host, "candidate_resume", &fx.resume_input()).unwrap();
            fx.continue_from_published_head();
            let rewound = git(&fx.repo, &["rev-parse", &format!("{}^", fx.earlier_head)]);
            fx.change_before_push(Change::Rewind(rewound.clone()));

            action(&host, "git_push", &fx.push_input(EARLIER, &resumed))
                .expect_err("the push is refused under the published-head lease");
            assert_eq!(fx.remote_tip(EARLIER), rewound, "the rewound head survives");
        },
    );
}

/// A change to the adopted branch on the remote, timed to land after the
/// engine observed the branch and before its push starts.
enum Change {
    Delete,
    Rewind(String),
}

struct Fixture {
    _root: tempfile::TempDir,
    forge: PathBuf,
    repo: PathBuf,
    /// The earlier claim's retained worktree.
    earlier: PathBuf,
    earlier_head: String,
    /// The re-claim's worktree, on [`OURS`] at the moved base.
    claim: PathBuf,
    base_sha: String,
    /// Where a [`Change`] waits for the engine's next push.
    change_file: PathBuf,
}

impl Fixture {
    fn new(sandbox: &Path) -> Self {
        let root = tempfile::tempdir_in(sandbox).unwrap();
        let forge = root.path().join("forge");
        let remote = forge.join("remote.git");
        let repo = root.path().join("repo");
        fs::create_dir_all(forge.join("prs")).unwrap();
        fs::create_dir_all(&repo).unwrap();
        git(root.path(), &["init", "-q", "--bare", path_str(&remote)]);
        git_fixture::init(&repo);
        let repo = repo.canonicalize().unwrap();
        git(&repo, &["remote", "add", "origin", path_str(&remote)]);
        git(&repo, &["checkout", "-q", "-b", BASE]);
        git(&repo, &["config", "user.name", "Orbit Test"]);
        git(
            &repo,
            &["config", "user.email", "orbit-test@example.invalid"],
        );
        fs::write(repo.join("README.md"), "base\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        git(&repo, &["push", "-q", "origin", BASE]);

        // The earlier claim: its candidate, pushed, with #41 open on it.
        let earlier = root.path().join("earlier");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                EARLIER,
                path_str(&earlier),
                BASE,
            ],
        );
        let earlier = earlier.canonicalize().unwrap();
        fs::create_dir_all(earlier.join("src")).unwrap();
        fs::write(earlier.join("src/feature.txt"), "earlier candidate\n").unwrap();
        git(&earlier, &["add", "src/feature.txt"]);
        git(&earlier, &["commit", "-q", "-m", "earlier candidate"]);
        git(&earlier, &["push", "-q", "origin", EARLIER]);
        let earlier_head = git(&earlier, &["rev-parse", "HEAD"]);
        let pr = forge.join("prs").join(PRIOR_PR);
        fs::create_dir_all(&pr).unwrap();
        fs::write(pr.join("head"), EARLIER).unwrap();
        fs::write(pr.join("base"), BASE).unwrap();
        fs::write(pr.join("state"), "OPEN").unwrap();

        // The base moves before the task is claimed again.
        fs::write(repo.join("other.txt"), "landed meanwhile\n").unwrap();
        git(&repo, &["add", "other.txt"]);
        git(&repo, &["commit", "-q", "-m", "landed meanwhile"]);
        git(&repo, &["push", "-q", "origin", BASE]);
        let base_sha = git(&repo, &["rev-parse", BASE]);

        let claim = root.path().join("claim");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", OURS, path_str(&claim), BASE],
        );
        let claim = claim.canonicalize().unwrap();

        let gh = sandbox.join("bin").join("gh");
        fs::create_dir_all(gh.parent().unwrap()).unwrap();
        fs::write(&gh, FAKE_GH.replace("__FORGE__", path_str(&forge))).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();

        // The real Git is found before the substitute shadows it on `PATH`.
        let real_git = PathBuf::from(std::env::var_os("ORBIT_CLAIMED_PR_REUSE_REAL_GIT").unwrap());
        let change_file = root.path().join("push-change");
        let git_shim = sandbox.join("bin").join("git");
        fs::write(
            &git_shim,
            PUSH_BOUNDARY_GIT
                .replace("__GIT__", path_str(&real_git))
                .replace("__REMOTE__", path_str(&remote))
                .replace("__CHANGE__", path_str(&change_file))
                .replace("__REF__", &format!("refs/heads/{EARLIER}")),
        )
        .unwrap();
        fs::set_permissions(&git_shim, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _root: root,
            forge,
            repo,
            earlier,
            earlier_head,
            claim,
            base_sha,
            change_file,
        }
    }

    /// The candidate reference the owner kept from the earlier claim, as the
    /// re-claim's leaf input carries it.
    fn resume_input(&self) -> Value {
        json!({
            "job_run_id": RUN_ID,
            "task_ids": [TASK_ID],
            "workspace_path": self.claim,
            "base_sha": self.base_sha,
            "claimed": true,
            "candidate": {
                "branch": EARLIER,
                "head_sha": self.earlier_head,
                "pull_request": PRIOR_PR,
                "source_run_id": "jrun-reuse-earlier",
                "failed_step_id": "landing_review_gate_settle",
                "published": true,
            },
        })
    }

    /// The implementer continues the applied candidate and the commit step
    /// commits it; returns the new head.
    fn continue_and_commit(&self) -> String {
        assert_eq!(
            fs::read_to_string(self.claim.join("src/feature.txt")).unwrap(),
            "earlier candidate\n",
            "the candidate is applied for the implementer"
        );
        fs::write(self.claim.join("src/feature.txt"), "continued candidate\n").unwrap();
        git(&self.claim, &["add", "-A"]);
        git(&self.claim, &["commit", "-q", "-m", "continued candidate"]);
        git(&self.claim, &["rev-parse", "HEAD"])
    }

    /// The continued candidate committed on the published head. Adoption
    /// leaves HEAD at the moved base, so the adopted branch is first reset
    /// onto the published head, the state a fast-forward needs.
    fn continue_from_published_head(&self) -> String {
        git(&self.claim, &["reset", "-q", "--hard", &self.earlier_head]);
        self.continue_and_commit()
    }

    /// Lands `change` on the adopted branch when the engine's next push
    /// starts, after the engine has observed the branch.
    fn change_before_push(&self, change: Change) {
        let to = match change {
            Change::Delete => "delete".to_string(),
            Change::Rewind(sha) => sha,
        };
        fs::write(&self.change_file, to).unwrap();
    }

    fn push_input(&self, branch: &str, resumed: &Value) -> Value {
        json!({
            "job_run_id": RUN_ID,
            "completed_task_ids": [TASK_ID],
            "workspace_path": self.claim,
            "branch": branch,
            "base": BASE,
            "base_ref": BASE,
            "rewrite_performed": false,
            "candidate_resume": resumed,
        })
    }

    /// What the earlier claim's host, or anyone else, pushes onto the
    /// published branch after the candidate was kept: a commit the re-claim's
    /// candidate never contained. Returns the branch's new head.
    fn push_remote_only_commit(&self) -> String {
        fs::write(self.earlier.join("remote-only.txt"), "remote only\n").unwrap();
        git(&self.earlier, &["add", "remote-only.txt"]);
        git(&self.earlier, &["commit", "-q", "-m", "remote only"]);
        let head = git(&self.earlier, &["rev-parse", "HEAD"]);
        git(
            &self.earlier,
            &["push", "-q", "origin", &format!("HEAD:{EARLIER}")],
        );
        assert_eq!(self.remote_tip(EARLIER), head);
        head
    }

    /// The base moves while the re-claim implements.
    fn advance_base(&self) {
        fs::write(self.repo.join("landed-later.txt"), "landed later\n").unwrap();
        git(&self.repo, &["add", "landed-later.txt"]);
        git(&self.repo, &["commit", "-q", "-m", "landed later"]);
        git(&self.repo, &["push", "-q", "origin", BASE]);
    }

    /// The claimed pipeline's `prepare_branch` and `sync_base` steps; returns
    /// the rebase output.
    fn sync(&self, host: &Host) -> Value {
        let prepared = action(
            host,
            "pr_prepare",
            &json!({
                "job_run_id": RUN_ID,
                "completed_task_ids": [TASK_ID],
                "workspace_path": self.claim,
                "base": BASE,
                "base_sync": "local",
            }),
        )
        .unwrap();
        assert_eq!(prepared["decision"], "rebase_required", "{prepared}");
        let mut input = json!({
            "job_run_id": RUN_ID,
            "completed_task_ids": [TASK_ID],
            "workspace_path": self.claim,
        });
        for field in [
            "head",
            "head_sha",
            "base",
            "base_ref",
            "base_sha",
            "remote_sha",
            "commits_behind",
            "sync_required",
        ] {
            input[field] = prepared[field].clone();
        }
        action(host, "git_rebase", &input).unwrap()
    }

    /// The claimed pipeline's `push` step input after `sync_base`.
    fn synced_push_input(&self, resumed: &Value, synced: &Value) -> Value {
        json!({
            "job_run_id": RUN_ID,
            "completed_task_ids": [TASK_ID],
            "workspace_path": self.claim,
            "branch": synced["head"],
            "base": synced["base"],
            "base_ref": synced["base_ref"],
            "rewrite_performed": synced["rewritten"],
            "rewrite_head_before": synced["head_sha_before"],
            "expected_remote_sha": synced["remote_sha_before"],
            "candidate_resume": resumed,
        })
    }

    fn open_input(&self, head: &str, resumed: &Value) -> Value {
        json!({
            "job_run_id": RUN_ID,
            "completed_task_ids": [TASK_ID],
            "workspace_path": self.claim,
            "head": head,
            "base": BASE,
            "base_ref": BASE,
            "base_sha": self.base_sha,
            "base_sync": "local",
            "candidate_resume": resumed,
        })
    }

    /// Gives a seeded pull request an author (when not Orbit's) and a body.
    fn describe_pr(&self, number: &str, author: Option<&str>, body: &str) {
        let dir = self.forge.join("prs").join(number);
        if let Some(author) = author {
            fs::write(dir.join("author"), author).unwrap();
        }
        fs::write(dir.join("body"), body).unwrap();
    }

    /// Seeds a pull request the earlier attempt did not open.
    fn add_pr(&self, number: &str, head: &str, state: &str, author: Option<&str>, names: &str) {
        let dir = self.forge.join("prs").join(number);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("head"), head).unwrap();
        fs::write(dir.join("base"), BASE).unwrap();
        fs::write(dir.join("state"), state).unwrap();
        self.describe_pr(number, author, &format!("Delivery of {names}."));
    }

    fn remote_tip(&self, branch: &str) -> String {
        git(
            &self.forge.join("remote.git"),
            &["rev-parse", &format!("refs/heads/{branch}")],
        )
    }

    fn pr_file(&self, number: &str, name: &str) -> String {
        fs::read_to_string(self.forge.join("prs").join(number).join(name)).unwrap_or_default()
    }

    fn open_prs(&self) -> Vec<String> {
        let mut open = fs::read_dir(self.forge.join("prs"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|number| self.pr_file(number, "state") == "OPEN")
            .collect::<Vec<_>>();
        open.sort();
        open
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.forge.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// The task the delivery actions read; everything else is the engine's own.
struct Host {
    repo: PathBuf,
    /// `pr.close_on_terminal`, which also closes a task's earlier delivery
    /// pull requests when a run publishes its own.
    close_superseded: bool,
}

impl Host {
    fn new(repo: &Path) -> Self {
        Self {
            repo: repo.to_path_buf(),
            close_superseded: false,
        }
    }

    fn closing_superseded(repo: &Path) -> Self {
        Self {
            close_superseded: true,
            ..Self::new(repo)
        }
    }
}

impl orbit_engine::test_support::NullRuntimeHost for Host {
    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        if task_id != TASK_ID {
            return Err(OrbitError::not_found(
                NotFoundKind::Task,
                task_id.to_string(),
            ));
        }
        let now = Utc::now();
        Ok(Task {
            job_run_machine: None,
            id: TASK_ID.to_string(),
            title: "Continue the earlier claim's candidate".to_string(),
            description: String::new(),
            acceptance_criteria: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: "Outcome: success\n\nChanges:\n- Continued.".to_string(),
            context_files: Vec::new(),
            created_by: None,
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::InProgress,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            pr_status: None,
            external_refs: Vec::new(),
            relations: Vec::new(),
            job_run_id: Some(RUN_ID.to_string()),
            crew: None,
            crew_source: None,
            orchestrator: None,
            origin: None,
            created_at: now,
            updated_at: now,
        })
    }

    fn apply_task_automation_update(
        &self,
        _task_id: &str,
        _update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo.to_string_lossy().into_owned())
    }

    fn pr_config(&self) -> PrConfig {
        PrConfig {
            close_superseded: self.close_superseded,
            ..PrConfig::default()
        }
    }
}

fn action(host: &Host, name: &str, input: &Value) -> Result<Value, OrbitError> {
    if name == "git_push" {
        git_fixture::assert_local_push_remote(&host.repo);
    }
    execute_deterministic_action(host, name, &json!({}), input, false, &HashMap::new(), None)
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_fixture::run(dir, args)
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("utf8 fixture path")
}

/// The `gh` surface the provider adapter calls, over any number of pull
/// requests: `prs/<number>/{head,base,state,comment}` under the forge, with
/// an optional `author` (else `orbit`) and single-line `body`.
const FAKE_GH: &str = r#"#!/bin/sh
set -eu
forge='__FORGE__'
remote="$forge/remote.git"
printf '%s %s\n' "$1" "${2:-}" >> "$forge/calls"

# The value following flag `$1` among the remaining arguments.
flag() {
    name=$1; shift
    while [ $# -gt 0 ]; do
        if [ "$1" = "$name" ]; then printf '%s' "$2"; return; fi
        shift
    done
}

url() { printf 'https://github.com/orbit/test/pull/%s' "$1"; }

case "$1 ${2:-}" in
    "pr list")
        # With `--head`, the open pull requests on that branch; without it,
        # every open one with its body.
        head=$(flag --head "$@")
        printf '['
        sep=
        for dir in "$forge"/prs/*; do
            [ "$(cat "$dir/state")" = OPEN ] || continue
            [ -z "$head" ] || [ "$(cat "$dir/head")" = "$head" ] || continue
            author=orbit
            [ ! -f "$dir/author" ] || author=$(cat "$dir/author")
            body=
            [ ! -f "$dir/body" ] || body=$(cat "$dir/body")
            printf '%s{"number":%s,"title":"Delivery","headRefName":"%s","author":{"login":"%s"},"body":"%s"}' \
                "$sep" "${dir##*/}" "$(cat "$dir/head")" "$author" "$body"
            sep=,
        done
        printf ']\n' ;;
    "pr create")
        last=$(ls "$forge/prs" | sort -n | tail -n 1)
        number=$(( ${last:-0} + 1 ))
        mkdir -p "$forge/prs/$number"
        flag --head "$@" > "$forge/prs/$number/head"
        flag --base "$@" > "$forge/prs/$number/base"
        printf 'OPEN' > "$forge/prs/$number/state"
        url "$number"; echo ;;
    "pr view")
        number=${3##*/}
        dir="$forge/prs/$number"
        head=$(cat "$dir/head")
        oid=$(git --git-dir="$remote" rev-parse --verify --quiet "refs/heads/$head" || true)
        printf '{"number":%s,"title":"Delivery","body":"","state":"%s","headRefName":"%s","headRefOid":"%s","baseRefName":"%s","files":[],"commits":[],"url":"%s"}\n' \
            "$number" "$(cat "$dir/state")" "$head" "$oid" "$(cat "$dir/base")" "$(url "$number")" ;;
    "pr close")
        flag --comment "$@" > "$forge/prs/$3/comment"
        printf 'CLOSED' > "$forge/prs/$3/state" ;;
    *) echo "fake gh: unsupported call: $*" >&2; exit 2 ;;
esac
"#;

/// A `git` that shadows the real one on `PATH`. When the engine's push is about
/// to run, a change waiting in `__CHANGE__` lands on the bare remote first, so
/// the push's lease is checked against a ref already moved, before Git
/// advertises anything. Other invocations, and a push with nothing waiting,
/// exec the real Git unchanged.
const PUSH_BOUNDARY_GIT: &str = r#"#!/bin/sh
set -eu
git='__GIT__'
remote='__REMOTE__'
change='__CHANGE__'
ref='__REF__'
subcommand=
skip=
for arg in "$@"; do
    if [ -n "$skip" ]; then skip=; continue; fi
    case "$arg" in
        -c) skip=1 ;;
        -*) ;;
        *) subcommand=$arg; break ;;
    esac
done
if [ "$subcommand" = push ] && [ -f "$change" ]; then
    action=$(cat "$change")
    mv "$change" "$change.applied"
    case "$action" in
        delete) "$git" --git-dir="$remote" update-ref -d "$ref" ;;
        *) "$git" --git-dir="$remote" update-ref "$ref" "$action" ;;
    esac
fi
exec "$git" "$@"
"#;

/// Run `body` in a copy of this test binary whose `PATH` starts with the
/// substitute `gh`, with disposable `$HOME` and no inherited authority.
fn isolated(test: &str, body: impl FnOnce(&Path)) {
    if std::env::var_os(CHILD_ENV).is_some() {
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
    let mut path = vec![bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .find_map(|dir| fs::canonicalize(dir.join("git")).ok())
        .expect("git on PATH");
    let qualified = format!(
        "{}::{test}",
        module_path!().split_once("::").expect("test module").1
    );
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
    git_fixture::configure_child(&mut command, &home, sandbox.path());
    command
        .args([&qualified, "--exact", "--nocapture", "--test-threads=1"])
        .current_dir(sandbox.path())
        .env("ORBIT_CLAIMED_PR_REUSE_REAL_GIT", real_git)
        .env(CHILD_ENV, test)
        .env(SANDBOX_ENV, sandbox.path())
        .env("PATH", std::env::join_paths(path).unwrap())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("TMPDIR", &tmp)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = test_env::run_child_test(&mut command, &qualified, sandbox.path());
    test_env::assert_child_test_passed(&qualified, output.status, output.stdout, output.stderr);
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn published_lease_contracts_1() {
    isolated("published_lease_contracts_1", |_| {
        crate::git_fixture::run_scenarios(&[
            (
                "a_resumed_published_candidate_republishes_through_its_open_pull_request",
                a_resumed_published_candidate_republishes_through_its_open_pull_request,
            ),
            (
                "an_unreusable_branch_closes_the_earlier_pull_request_for_the_new_one",
                an_unreusable_branch_closes_the_earlier_pull_request_for_the_new_one,
            ),
            (
                "an_advanced_adopted_branch_is_not_overwritten_after_a_base_rebase",
                an_advanced_adopted_branch_is_not_overwritten_after_a_base_rebase,
            ),
            (
                "a_deleted_adopted_branch_is_not_recreated",
                a_deleted_adopted_branch_is_not_recreated,
            ),
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn published_lease_contracts_2() {
    isolated("published_lease_contracts_2", |_| {
        crate::git_fixture::run_scenarios(&[
            (
                "an_unmoved_adopted_branch_is_replaced_after_a_base_rebase",
                an_unmoved_adopted_branch_is_replaced_after_a_base_rebase,
            ),
            (
                "an_unmoved_adopted_branch_fast_forwards_under_its_published_lease",
                an_unmoved_adopted_branch_fast_forwards_under_its_published_lease,
            ),
            (
                "a_fast_forward_does_not_recreate_an_adopted_branch_deleted_after_observation",
                a_fast_forward_does_not_recreate_an_adopted_branch_deleted_after_observation,
            ),
            (
                "a_fast_forward_does_not_move_an_adopted_branch_rewound_after_observation",
                a_fast_forward_does_not_move_an_adopted_branch_rewound_after_observation,
            ),
            (
                "a_fresh_attempt_closes_the_tasks_earlier_open_pull_request",
                a_fresh_attempt_closes_the_tasks_earlier_open_pull_request,
            ),
        ]);
    });
}
