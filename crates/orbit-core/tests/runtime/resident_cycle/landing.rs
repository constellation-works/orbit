//! A resident holding the merge right lands a task's delivery pull request
//! through one audited call, and cannot call a task done while its pull request
//! is open: a cycle once marked a task done with its PR still open.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_core::application::task::TaskUpdateParams;
use orbit_core::runtime::task_pr_forge::{
    ForgePullRequest, LeasePullRequest, MergedPullRequest, PullRequestBlockers,
    PullRequestMergeOutcome, TaskPrForge,
};
use orbit_types::task::{ExternalRef, TaskComplexity};
use orbit_types::workflow::{RESIDENT_ACTION_EVENT, ResidentActionPhase, ResidentActionRow};

use super::*;

const BOT: &str = "orbit-bot";
const HEAD: &str = "0dbd8e2000000000000000000000000000000000";
const MERGE_COMMIT: &str = "03f7af7d6a1b2c3d4e5f60718293a4b5c6d7e8f9";
const PR: u64 = 205;

#[derive(Default)]
struct ForgeState {
    /// Open PRs by number, as the forge lists them.
    open: BTreeMap<u64, ForgePullRequest>,
    /// PRs the forge has merged.
    merged: BTreeMap<u64, MergedPullRequest>,
    /// The reason branch protection refuses a merge with, when it does.
    refusal: Option<String>,
    /// Required checks still running on the head.
    pending_checks: Vec<String>,
    /// Fail every read of an open pull request, as an outage would.
    unreachable: bool,
    /// Every merge request the forge received, with its head pin.
    merge_requests: Vec<(u64, String)>,
}

#[derive(Default)]
struct LandingForge(Mutex<ForgeState>);

impl LandingForge {
    fn delivery(number: u64, task: &str) -> ForgePullRequest {
        ForgePullRequest {
            number,
            head_branch: format!("orbit/{task}-47642870"),
            body: format!("Delivers {task}.\n\nOrbit run jrun-1"),
            author: BOT.to_string(),
        }
    }

    fn open(&self, number: u64, task: &str) {
        let pull = Self::delivery(number, task);
        self.0.lock().unwrap().open.insert(number, pull);
    }
}

impl TaskPrForge for LandingForge {
    fn open_pull_requests(&self, _repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        let state = self.0.lock().unwrap();
        if state.unreachable {
            return Err(OrbitError::Execution("forge unavailable".into()));
        }
        Ok(state.open.values().cloned().collect())
    }

    fn lease_pull_request(
        &self,
        _repo_root: &Path,
        binding: &str,
    ) -> Result<Option<LeasePullRequest>, OrbitError> {
        let number: u64 = binding.trim_start_matches('#').parse().unwrap();
        Ok(Some(LeasePullRequest {
            number,
            base_branch: "main".to_string(),
            merged: self.0.lock().unwrap().merged.contains_key(&number),
        }))
    }

    fn pull_request_blockers(
        &self,
        _repo_root: &Path,
        number: u64,
    ) -> Result<Option<PullRequestBlockers>, OrbitError> {
        let state = self.0.lock().unwrap();
        Ok(state.open.get(&number).map(|pull| PullRequestBlockers {
            pull: pull.clone(),
            head_sha: HEAD.to_string(),
            conflicting: false,
            failed_required_checks: Vec::new(),
            pending_required_checks: state.pending_checks.clone(),
            passed_required_checks: vec!["Test".to_string()],
        }))
    }

    fn merge_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
        head_sha: &str,
    ) -> Result<PullRequestMergeOutcome, OrbitError> {
        let mut state = self.0.lock().unwrap();
        state.merge_requests.push((number, head_sha.to_string()));
        if let Some(reason) = state.refusal.clone() {
            return Ok(PullRequestMergeOutcome::Refused { reason });
        }
        let pull = state.open.remove(&number).unwrap();
        state.merged.insert(
            number,
            MergedPullRequest {
                pull,
                base_branch: "main".to_string(),
                head_sha: head_sha.to_string(),
                merge_commit: MERGE_COMMIT.to_string(),
            },
        );
        Ok(PullRequestMergeOutcome::Merged {
            merge_commit: MERGE_COMMIT.to_string(),
        })
    }

    fn merged_pull_requests(
        &self,
        _repo_root: &Path,
    ) -> Result<Vec<MergedPullRequest>, OrbitError> {
        Ok(self.0.lock().unwrap().merged.values().cloned().collect())
    }

    fn merged_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
    ) -> Result<Option<MergedPullRequest>, OrbitError> {
        Ok(self.0.lock().unwrap().merged.get(&number).cloned())
    }

    fn authenticated_login(&self, _repo_root: &Path) -> Result<String, OrbitError> {
        Ok(BOT.to_string())
    }

    fn close_pull_request(
        &self,
        _repo_root: &Path,
        _number: u64,
        _comment: &str,
    ) -> Result<(), OrbitError> {
        Ok(())
    }
}

/// A cycle fixture on `forge`, with a live cycle holding `rights`. Returns the
/// assignment and the cycle's run.
fn live_cycle(fx: &mut Fixture, forge: &Arc<LandingForge>, rights: &[&str]) -> (String, String) {
    fx.runtime = fx.runtime.clone().with_task_pr_forge(forge.clone());
    let watched = fx.task("Watched");
    let mandate = fx.mandate(&watched);
    if !rights.is_empty() {
        fx.grant(&mandate, rights);
    }
    let run = fx.cycle_run(&mandate, "resident-cycle-landing");
    fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    (mandate, run)
}

/// A delivered task in review, with `prs` recorded the way delivery records them.
fn review_task(fx: &Fixture, prs: &[u64]) -> String {
    fx.runtime
        .add_task(TaskAddParams {
            title: "Delivered leaf".into(),
            description: "A delivered leaf.".into(),
            acceptance_criteria: vec!["The leaf is observable.".into()],
            plan: "Fixture plan.".into(),
            context_files: vec!["file:.gitignore".into()],
            complexity: TaskComplexity::Low,
            status: Some(TaskStatus::Review),
            external_refs: prs
                .iter()
                .map(|number| ExternalRef::github_pr(number.to_string()).unwrap())
                .collect(),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string()
}

fn land(fx: &Fixture, task: &str) -> Result<Value, String> {
    call(
        &fx.runtime,
        "orbit.workflow.land",
        json!({"task_id": task, "workspace": fx.runtime.workspace_id().unwrap(), "model": "codex"}),
        McpCapability::Agent,
    )
}

fn mark_done(fx: &Fixture, task: &str) -> Result<Value, String> {
    call(
        &fx.runtime,
        "orbit.task.update",
        json!({"id": task, "status": "done", "execution_summary": "Delivered.", "model": "codex"}),
        McpCapability::Agent,
    )
}

fn status(fx: &Fixture, task: &str) -> TaskStatus {
    fx.runtime.get_task(task).unwrap().status
}

/// The ledger rows the cycle recorded for `tool`, latest state per key.
fn action_rows(fx: &Fixture, mandate: &str, tool: &str) -> Vec<ResidentActionRow> {
    let mut rows: Vec<ResidentActionRow> = Vec::new();
    for row in fx
        .runtime
        .get_task_history(mandate)
        .unwrap()
        .iter()
        .filter(|entry| entry.event == RESIDENT_ACTION_EVENT)
        .filter_map(|entry| serde_json::from_str::<ResidentActionRow>(entry.note.as_deref()?).ok())
        .filter(|row| row.tool == tool)
    {
        rows.retain(|existing| existing.key != row.key);
        rows.push(row);
    }
    rows
}

#[test]
fn a_cycle_with_the_merge_right_lands_a_delivery_through_one_audited_call() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::landing::a_cycle_with_the_merge_right_lands_a_delivery_through_one_audited_call",
    ) {
        return;
    }
    let mut fx = fixture();
    let forge = Arc::new(LandingForge::default());
    let (mandate, run) = live_cycle(&mut fx, &forge, &["merge"]);
    let task = review_task(&fx, &[PR]);
    forge.open(PR, &task);

    let landed = fx.as_cycle_agent(&run, || land(&fx, &task)).unwrap();

    assert_eq!(landed["task_id"], task.as_str(), "{landed}");
    assert_eq!(landed["pull_request"], PR, "{landed}");
    assert_eq!(landed["merge_commit"], MERGE_COMMIT, "{landed}");
    assert_eq!(landed["status"], "done", "{landed}");
    assert!(landed["resident_action_key"].is_string(), "{landed}");
    assert_eq!(status(&fx, &task), TaskStatus::Done);
    assert_eq!(
        forge.0.lock().unwrap().merge_requests,
        vec![(PR, HEAD.to_string())],
        "the merge is pinned to the head whose checks were read"
    );
    let rows = action_rows(&fx, &mandate, "orbit.workflow.land");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].phase, ResidentActionPhase::Ok, "{rows:?}");
    assert_eq!(rows[0].subject.as_deref(), Some(task.as_str()), "{rows:?}");
}

#[test]
fn a_branch_protection_refusal_is_recorded_and_leaves_the_task_in_review() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::landing::a_branch_protection_refusal_is_recorded_and_leaves_the_task_in_review",
    ) {
        return;
    }
    let mut fx = fixture();
    let forge = Arc::new(LandingForge::default());
    let (mandate, run) = live_cycle(&mut fx, &forge, &["merge"]);
    let task = review_task(&fx, &[PR]);
    forge.open(PR, &task);
    forge.0.lock().unwrap().refusal = Some("Required review is missing".to_string());

    let error = fx
        .as_cycle_agent(&run, || land(&fx, &task))
        .expect_err("branch protection refuses");

    assert!(error.contains("task_landing_refused"), "{error}");
    assert!(error.contains("Required review is missing"), "{error}");
    assert_eq!(status(&fx, &task), TaskStatus::Review);
    let rows = action_rows(&fx, &mandate, "orbit.workflow.land");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].phase, ResidentActionPhase::Refused, "{rows:?}");

    // Orbit refuses unfinished required checks before asking the forge.
    {
        let mut state = forge.0.lock().unwrap();
        state.refusal = None;
        state.merge_requests.clear();
        state.pending_checks = vec!["Test".to_string()];
    }
    let error = fx
        .as_cycle_agent(&run, || land(&fx, &task))
        .expect_err("a required check is still running");
    assert!(error.contains("task_landing_refused"), "{error}");
    assert!(forge.0.lock().unwrap().merge_requests.is_empty());
    assert_eq!(status(&fx, &task), TaskStatus::Review);
}

#[test]
fn landing_needs_the_merge_right_and_a_cycle() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::landing::landing_needs_the_merge_right_and_a_cycle",
    ) {
        return;
    }
    let mut fx = fixture();
    let forge = Arc::new(LandingForge::default());
    let (_, run) = live_cycle(&mut fx, &forge, &["dispatch"]);
    let task = review_task(&fx, &[PR]);
    forge.open(PR, &task);

    let error = fx
        .as_cycle_agent(&run, || land(&fx, &task))
        .expect_err("the right is missing");
    assert!(error.contains("resident_right_refused"), "{error}");

    // A session with no cycle behind it is held to the operator row.
    let error = land(&fx, &task).expect_err("an agent session is no operator");
    assert!(error.contains("capability"), "{error}");

    assert!(forge.0.lock().unwrap().merge_requests.is_empty());
    assert_eq!(status(&fx, &task), TaskStatus::Review);
}

#[test]
fn a_repeated_landing_settles_a_merge_whose_reply_was_lost_without_merging_again() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::landing::a_repeated_landing_settles_a_merge_whose_reply_was_lost_without_merging_again",
    ) {
        return;
    }
    let mut fx = fixture();
    let forge = Arc::new(LandingForge::default());
    let (_, run) = live_cycle(&mut fx, &forge, &["merge"]);
    let task = review_task(&fx, &[PR]);
    forge.open(PR, &task);
    forge
        .merge_pull_request(Path::new("."), PR, HEAD)
        .expect("the first attempt merged");
    forge.0.lock().unwrap().merge_requests.clear();

    let landed = fx.as_cycle_agent(&run, || land(&fx, &task)).unwrap();

    assert_eq!(landed["status"], "done", "{landed}");
    assert_eq!(landed["merge_commit"], MERGE_COMMIT, "{landed}");
    assert!(forge.0.lock().unwrap().merge_requests.is_empty());
    assert_eq!(status(&fx, &task), TaskStatus::Done);
}

#[test]
fn a_cycle_cannot_call_a_task_done_while_its_pull_request_is_open() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::landing::a_cycle_cannot_call_a_task_done_while_its_pull_request_is_open",
    ) {
        return;
    }
    let mut fx = fixture();
    let forge = Arc::new(LandingForge::default());
    let (_, run) = live_cycle(&mut fx, &forge, &["merge"]);
    let task = review_task(&fx, &[PR]);
    forge.open(PR, &task);

    let error = fx
        .as_cycle_agent(&run, || mark_done(&fx, &task))
        .expect_err("its pull request is open");
    assert!(error.contains("resident_done_pr_open"), "{error}");
    assert!(error.contains("`orbit.workflow` action `land`"), "{error}");
    assert_eq!(status(&fx, &task), TaskStatus::Review);

    // A forge that cannot answer does not vouch for the move.
    forge.0.lock().unwrap().unreachable = true;
    let error = fx
        .as_cycle_agent(&run, || mark_done(&fx, &task))
        .expect_err("the forge cannot answer");
    assert!(error.contains("resident_lookup_refused"), "{error}");
    assert_eq!(status(&fx, &task), TaskStatus::Review);
    forge.0.lock().unwrap().unreachable = false;

    // No open pull request, no objection: this guard only holds a task back
    // while one is open.
    let no_pr = review_task(&fx, &[]);
    fx.as_cycle_agent(&run, || mark_done(&fx, &no_pr)).unwrap();
    assert_eq!(status(&fx, &no_pr), TaskStatus::Done);

    // An operator is not held to it.
    fx.runtime
        .update_task_as_human(
            &task,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                execution_summary: Some("Delivered.".into()),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    assert_eq!(status(&fx, &task), TaskStatus::Done);
}

/// The production forge reads a 4xx as the forge's refusal and anything else as
/// an unknown outcome, because only the first proves nothing merged.
#[cfg(unix)]
#[test]
fn the_github_forge_tells_a_refused_merge_from_an_unknown_outcome() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::landing::the_github_forge_tells_a_refused_merge_from_an_unknown_outcome",
    ) {
        return;
    }
    use orbit_core::runtime::task_pr_forge::GhTaskPrForge;
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let gh = root.path().join("gh");
    let answer = root.path().join("answer");
    std::fs::write(
        &gh,
        format!(
            r#"#!/bin/sh
case "$1 $2" in
 'repo view') echo '{{"squashMergeAllowed":true,"rebaseMergeAllowed":false,"mergeCommitAllowed":false}}';;
 'api '*) . '{}';;
 *) exit 99;;
esac
"#,
            answer.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        root.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let _env = orbit_common::test_env::scoped([("PATH", Some(path.as_str()))]);
    let merge = |script: &str| {
        std::fs::write(&answer, script).unwrap();
        GhTaskPrForge.merge_pull_request(root.path(), PR, HEAD)
    };

    let merged = merge(&format!(
        "printf '{{\"merged\":true,\"sha\":\"{MERGE_COMMIT}\"}}'"
    ))
    .unwrap();
    assert_eq!(
        merged,
        PullRequestMergeOutcome::Merged {
            merge_commit: MERGE_COMMIT.to_string()
        }
    );

    let refused =
        merge("echo 'gh: Required status check is expected (HTTP 405)' >&2; exit 1").unwrap();
    assert!(
        matches!(&refused, PullRequestMergeOutcome::Refused { reason } if reason.contains("Required status check")),
        "{refused:?}"
    );

    let outage = merge("echo 'gh: Server Error (HTTP 502)' >&2; exit 1");
    assert!(outage.is_err(), "{outage:?}");
    let throttled = merge("echo 'gh: rate limited (HTTP 429)' >&2; exit 1");
    assert!(throttled.is_err(), "{throttled:?}");
}
