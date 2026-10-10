//! [ORB-15308] A re-claimed task whose earlier claim already opened a pull
//! request republishes through that pull request, or closes it for the new
//! one, and never leaves two open.
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
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, execute_deterministic_action};
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

/// The earlier claim pushed its candidate and opened #41, then failed in its
/// before-landing review. The re-claim resumes it on a moved base: its
/// branch becomes the candidate's, the push replaces the published head
/// under a lease on it, and `pr_open` reuses #41 rather than opening another.
/// The earlier claim's retained worktree keeps its checkout and commits on a
/// branch renamed aside.
#[test]
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
#[test]
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

struct Fixture {
    forge: PathBuf,
    repo: PathBuf,
    /// The earlier claim's retained worktree.
    earlier: PathBuf,
    earlier_head: String,
    /// The re-claim's worktree, on [`OURS`] at the moved base.
    claim: PathBuf,
    base_sha: String,
}

impl Fixture {
    fn new(sandbox: &Path) -> Self {
        let forge = sandbox.join("forge");
        let remote = forge.join("remote.git");
        let repo = sandbox.join("repo");
        fs::create_dir_all(forge.join("prs")).unwrap();
        fs::create_dir_all(&repo).unwrap();
        git(sandbox, &["init", "-q", "--bare", path_str(&remote)]);
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
        let earlier = sandbox.join("earlier");
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

        let claim = sandbox.join("claim");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", OURS, path_str(&claim), BASE],
        );
        let claim = claim.canonicalize().unwrap();

        let gh = sandbox.join("bin").join("gh");
        fs::create_dir_all(gh.parent().unwrap()).unwrap();
        fs::write(&gh, FAKE_GH.replace("__FORGE__", path_str(&forge))).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            forge,
            repo,
            earlier,
            earlier_head,
            claim,
            base_sha,
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
}

impl Host {
    fn new(repo: &Path) -> Self {
        Self {
            repo: repo.to_path_buf(),
        }
    }
}

impl RuntimeHost for Host {
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
/// requests: `prs/<number>/{head,base,state,comment}` under the forge.
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
        head=$(flag --head "$@")
        printf '['
        sep=
        for dir in "$forge"/prs/*; do
            [ "$(cat "$dir/state")" = OPEN ] || continue
            [ "$(cat "$dir/head")" = "$head" ] || continue
            printf '%s{"number":%s,"title":"Delivery","headRefName":"%s","author":{"login":"orbit"}}' \
                "$sep" "${dir##*/}" "$head"
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

/// Run `body` in a copy of this test binary whose `PATH` starts with the
/// substitute `gh`, with disposable `$HOME` and no inherited authority.
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
    let mut path = vec![bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
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
