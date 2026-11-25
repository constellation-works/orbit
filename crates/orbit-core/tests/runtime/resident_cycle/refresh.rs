//! A resident holding the `refresh` right brings a held or review delivery pull
//! request up to its base branch as one conflict-free merge commit and starts
//! its required checks, keyed on the action ledger, without rewriting published
//! commits or landing the PR; landing accepts conflict-free refreshed heads
//! under the prior review while refusing any other commit after the reviewed
//! head (ORB-26834).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::runtime::task_pr_forge::{
    BaseMergeInspection, ForgePullRequest, LeasePullRequest, MergedPullRequest,
    PullRequestBlockers, PullRequestMergeOutcome, PullRequestRefreshOutcome, TaskPrForge,
    inspect_base_merge,
};
use orbit_core::{FederatedWorkspaceTarget, WorkspaceCatalog, WorkspaceScope};
use orbit_store::contracts::TaskArtifactUpdateParams;
use orbit_types::task::{ArtifactWriter, ExternalRef, TaskArtifact, TaskComplexity, TaskStatus};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    RESIDENT_ACTION_EVENT, REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, ResidentActionPhase,
    ResidentActionRow, ReviewBudget, ReviewCertificate, ReviewConsumption, ReviewVerdict,
    ReviewerIdentity,
};
use serde_json::{Value, json};

use super::*;

const BOT: &str = "orbit-bot";
const PR: u64 = 412;
const MERGE_COMMIT: &str = "9876543210abcdef9876543210abcdef98765432";

#[derive(Default)]
struct RefreshForgeState {
    open: BTreeMap<u64, ForgePullRequest>,
    heads: BTreeMap<u64, String>,
    merged: BTreeMap<u64, MergedPullRequest>,
    passed_checks: BTreeMap<u64, Vec<String>>,
    pending_checks: BTreeMap<u64, Vec<String>>,
    failed_checks: BTreeMap<u64, Vec<String>>,
    checks_started_on: Vec<(u64, String)>,
    merge_requests: Vec<(u64, String)>,
    refresh_calls: Vec<(u64, String)>,
}

/// Real-Git-backed forge for refresh and refreshed-landing tests: uses
/// production [`inspect_base_merge`] to detect up-to-date, clean, conflicting
/// (with exact paths) and unrelated-history merges, and creates a real two-parent
/// merge commit via `git commit-tree` when clean without modifying the working tree.
#[derive(Default)]
struct RefreshForge(Mutex<RefreshForgeState>);

impl RefreshForge {
    fn open_delivery(&self, number: u64, task: &str, head_branch: &str, head_sha: &str) {
        let mut state = self.0.lock().unwrap();
        state.open.insert(
            number,
            ForgePullRequest {
                number,
                head_branch: head_branch.to_string(),
                body: format!("Delivers {task}.\n\nOrbit run jrun-1"),
                author: BOT.to_string(),
            },
        );
        state.heads.insert(number, head_sha.to_string());
    }

    fn open_foreign(&self, number: u64, head_branch: &str, author: &str, head_sha: &str) {
        let mut state = self.0.lock().unwrap();
        state.open.insert(
            number,
            ForgePullRequest {
                number,
                head_branch: head_branch.to_string(),
                body: "Manual PR not opened by Orbit.".to_string(),
                author: author.to_string(),
            },
        );
        state.heads.insert(number, head_sha.to_string());
    }

    fn set_head(&self, number: u64, head_sha: &str) {
        self.0
            .lock()
            .unwrap()
            .heads
            .insert(number, head_sha.to_string());
    }

    fn head(&self, number: u64) -> String {
        self.0.lock().unwrap().heads[&number].clone()
    }

    fn set_checks(&self, number: u64, passed: &[&str], pending: &[&str], failed: &[&str]) {
        let mut state = self.0.lock().unwrap();
        state.passed_checks.insert(
            number,
            passed.iter().map(|name| (*name).to_string()).collect(),
        );
        state.pending_checks.insert(
            number,
            pending.iter().map(|name| (*name).to_string()).collect(),
        );
        state.failed_checks.insert(
            number,
            failed.iter().map(|name| (*name).to_string()).collect(),
        );
    }
}

impl TaskPrForge for RefreshForge {
    fn open_pull_requests(&self, _repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(self.0.lock().unwrap().open.values().cloned().collect())
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
            head_sha: state.heads[&number].clone(),
            conflicting: false,
            failed_required_checks: state
                .failed_checks
                .get(&number)
                .cloned()
                .unwrap_or_default(),
            pending_required_checks: state
                .pending_checks
                .get(&number)
                .cloned()
                .unwrap_or_default(),
            passed_required_checks: state
                .passed_checks
                .get(&number)
                .cloned()
                .unwrap_or_default(),
            unreported_required_checks: Vec::new(),
            requires_status_checks: None,
        }))
    }

    fn update_pull_request_branch(
        &self,
        repo_root: &Path,
        number: u64,
        head_branch: &str,
        base_branch: &str,
        expected_head_sha: &str,
    ) -> Result<PullRequestRefreshOutcome, OrbitError> {
        let mut state = self.0.lock().unwrap();
        state
            .refresh_calls
            .push((number, expected_head_sha.to_string()));
        let current_head = state.heads.get(&number).cloned().unwrap_or_default();
        if current_head != expected_head_sha {
            return Ok(PullRequestRefreshOutcome::Refused {
                reason: format!("head moved to {current_head}"),
            });
        }
        let base_sha = git_out(
            repo_root,
            &["rev-parse", &format!("{base_branch}^{{commit}}")],
        );
        match inspect_base_merge(
            &orbit_common::Env::capture(),
            repo_root,
            expected_head_sha,
            &base_sha,
        )? {
            BaseMergeInspection::AlreadyUpToDate => {
                Ok(PullRequestRefreshOutcome::AlreadyUpToDate {
                    head_sha: expected_head_sha.to_string(),
                })
            }
            BaseMergeInspection::UnrelatedHistory => Ok(PullRequestRefreshOutcome::Refused {
                reason: "unrelated histories cannot be updated without rewriting published commits"
                    .to_string(),
            }),
            BaseMergeInspection::Conflicting { conflicting_paths } => {
                Ok(PullRequestRefreshOutcome::Conflicting {
                    conflicting_paths,
                    reason: format!(
                        "merging `{base_branch}` into pull request #{number} conflicts"
                    ),
                })
            }
            BaseMergeInspection::Clean { merged_tree } => {
                let new_head = git_out(
                    repo_root,
                    &[
                        "commit-tree",
                        &merged_tree,
                        "-p",
                        expected_head_sha,
                        "-p",
                        &base_sha,
                        "-m",
                        &format!("Merge branch '{base_branch}' into {head_branch}"),
                    ],
                );
                let _ = git_out(
                    repo_root,
                    &[
                        "update-ref",
                        &format!("refs/heads/{head_branch}"),
                        &new_head,
                    ],
                );
                state.heads.insert(number, new_head.clone());
                state.checks_started_on.push((number, new_head.clone()));
                // Starting required checks leaves them pending until CI finishes.
                state.passed_checks.insert(number, Vec::new());
                state
                    .pending_checks
                    .insert(number, vec!["Check / Clippy / Test".to_string()]);
                state.failed_checks.insert(number, Vec::new());
                Ok(PullRequestRefreshOutcome::Updated { head_sha: new_head })
            }
        }
    }

    fn merge_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
        head_sha: &str,
        _: &orbit_types::workflow::MergeMessage,
    ) -> Result<PullRequestMergeOutcome, OrbitError> {
        let mut state = self.0.lock().unwrap();
        state.merge_requests.push((number, head_sha.to_string()));
        let pull = state.open.remove(&number).unwrap();
        state.merged.insert(
            number,
            MergedPullRequest {
                pull,
                url: format!("https://github.com/owner/repo/pull/{number}"),
                base_branch: "main".to_string(),
                head_sha: head_sha.to_string(),
                merge_commit: MERGE_COMMIT.to_string(),
                merged_at: Utc::now(),
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

fn git_out(repo: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .args([
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=orbit-test@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit_file(repo: &Path, branch: &str, rel_path: &str, content: &str, message: &str) -> String {
    git_out(repo, &["checkout", "-q", branch]);
    let full = repo.join(rel_path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, content).unwrap();
    git_out(repo, &["add", rel_path]);
    git_out(repo, &["commit", "-qm", message]);
    git_out(repo, &["rev-parse", "HEAD"])
}

fn delivery_task(fx: &Fixture, status: TaskStatus, prs: &[u64]) -> String {
    fx.runtime
        .add_task(TaskAddParams {
            title: "Held delivery leaf".into(),
            description: "A held delivery leaf.".into(),
            acceptance_criteria: vec!["The leaf is observable.".into()],
            plan: "Fixture plan.".into(),
            context_files: vec!["file:.gitignore".into()],
            complexity: TaskComplexity::Low,
            status: Some(status),
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

fn live_cycle(fx: &mut Fixture, forge: &Arc<RefreshForge>, rights: &[&str]) -> (String, String) {
    fx.runtime = fx.runtime.clone().with_task_pr_forge(forge.clone());
    let watched = fx.task("Watched");
    let mandate = fx.mandate(&watched);
    if !rights.is_empty() {
        fx.grant(&mandate, rights);
    }
    let run = fx.cycle_run(&mandate, "resident-cycle-refresh");
    fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    (mandate, run)
}

fn call_refresh(runtime: &OrbitRuntime, task: &str, workspace: &str) -> Result<Value, String> {
    let env = orbit_common::Env::capture();
    runtime
        .execute_tool_command_dispatch_with_session_context_and_env(
            &env,
            "orbit.workflow",
            json!({
                "action": "refresh",
                "task_id": task,
                "workspace": workspace,
                "model": "codex"
            }),
            None,
            None,
            ToolEntryPoint::Mcp,
            session(McpCapability::Agent),
        )
        .map(|outcome| outcome.value)
        .map_err(|error| error.to_string())
}

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

struct TwoWorkspaces {
    targets: Vec<FederatedWorkspaceTarget>,
    runtimes: OnceLock<Vec<OrbitRuntime>>,
}

impl WorkspaceCatalog for TwoWorkspaces {
    fn resolve_scope(
        &self,
        scope: &WorkspaceScope,
    ) -> Result<Vec<FederatedWorkspaceTarget>, OrbitError> {
        Ok(match scope {
            WorkspaceScope::Selectors(selectors) => self
                .targets
                .iter()
                .filter(|target| {
                    selectors.iter().any(|selector| {
                        selector == &target.workspace_id || selector == &target.name
                    })
                })
                .cloned()
                .collect(),
            _ => self.targets.clone(),
        })
    }

    fn open(
        &self,
        target: &FederatedWorkspaceTarget,
    ) -> Result<orbit_runtime::RuntimeKernel, OrbitError> {
        let index = self
            .targets
            .iter()
            .position(|known| known.workspace_id == target.workspace_id)
            .unwrap();
        Ok(self.runtimes.get().unwrap()[index].clone().into_kernel())
    }
}

/// Record a passing system review certificate for `task_id` pinned to
/// `reviewed_head` on `base_commit`, in both the task artifact manifest and
/// the owner's immutable review ledger.
fn record_passing_review_certificate(
    fx: &Fixture,
    task_id: &str,
    base_commit: &str,
    reviewed_head: &str,
) {
    let task = fx.runtime.get_task(task_id).unwrap();
    let digest = orbit_automation::review::combined_task_meaning_digest(&[(
        task_id.to_string(),
        orbit_automation::review::task_meaning_digest(&task).unwrap(),
    )])
    .unwrap();
    let base_tree = git_out(&fx.repo, &["rev-parse", &format!("{base_commit}^{{tree}}")]);
    let head_tree = git_out(
        &fx.repo,
        &["rev-parse", &format!("{reviewed_head}^{{tree}}")],
    );
    let cert = ReviewCertificate {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: format!("rev-attempt-{task_id}-{reviewed_head}"),
        lineage_key: format!("lineage-{task_id}"),
        task_ids: vec![task_id.to_string()],
        task_meaning_digest: digest,
        repository: "owner/repo".to_string(),
        base: SourceRevision {
            commit: base_commit.to_string(),
            tree: base_tree,
        },
        reviewed_candidate: SourceRevision {
            commit: reviewed_head.to_string(),
            tree: head_tree.clone(),
        },
        final_candidate: SourceRevision {
            commit: reviewed_head.to_string(),
            tree: head_tree,
        },
        implementation_commits: Vec::new(),
        repair_commits: Vec::new(),
        verdict: ReviewVerdict::Accept,
        assurance: None,
        findings: Vec::new(),
        validation: Vec::new(),
        required_validation_commands: None,
        baseline_commands: Vec::new(),
        validation_complete: true,
        retained_obligations: Vec::new(),
        retired_validation: Vec::new(),
        validation_scope: Vec::new(),
        reviewer: ReviewerIdentity {
            crew: "sol".to_string(),
            provider: "codex".to_string(),
            model: "test".to_string(),
            reasoning_effort: None,
            implementer_model: None,
            same_model_as_implementer: false,
        },
        consumed: ReviewConsumption { seconds: 1 },
        budget: ReviewBudget { minutes: 10 },
        escalation: None,
        selectors_widened: Vec::new(),
        evidence_carried: None,
        baseline_red: Vec::new(),
        host_overrides: Vec::new(),
        known_red: Vec::new(),
        host_evidence: Vec::new(),
        owed_evidence: Vec::new(),
        owner_owed_evidence: Vec::new(),
        host_validation: Vec::new(),
        resumed_hold_attempt: None,
        issued_at: Utc::now(),
    };
    let ws = fx.runtime.workspace_id().unwrap();
    fx.runtime
        .sqlite_store()
        .unwrap()
        .write(|tx| orbit_core::test_support::review_ledger::record_certificate(tx, &ws, &cert))
        .unwrap();
    let artifact =
        TaskArtifact::from_text(REVIEW_GATE_ARTIFACT, serde_json::to_string(&cert).unwrap());
    fx.runtime
        .stores()
        .task_artifacts()
        .upsert_task_artifacts(
            task_id,
            TaskArtifactUpdateParams {
                actor: "system".into(),
                writer: Some(ArtifactWriter::System),
                upsert_artifacts: vec![artifact],
                ..Default::default()
            },
        )
        .unwrap();
}

#[cfg(unix)]
#[test]
fn a_cycle_with_refresh_updates_a_behind_delivery_pr_starts_checks_and_is_noop_when_up_to_date() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::refresh::a_cycle_with_refresh_updates_a_behind_delivery_pr_starts_checks_and_is_noop_when_up_to_date",
    ) {
        return;
    }
    let mut fx = fixture();
    let forge = Arc::new(RefreshForge::default());
    let (mandate, run) = live_cycle(&mut fx, &forge, &["refresh"]);
    let ws = fx.runtime.workspace_id().unwrap();
    let task = delivery_task(&fx, TaskStatus::Blocked, &[PR]);
    let head_branch = format!("orbit/{task}-47642870");

    // Create the delivery branch off initial `main`, then advance `main` on a
    // disjoint file so the merge is conflict-free.
    git_out(&fx.repo, &["checkout", "-qb", &head_branch, "main"]);
    let pr_head = commit_file(
        &fx.repo,
        &head_branch,
        "feature.txt",
        "delivery change\n",
        "feat: delivery commit",
    );
    let base_tip = commit_file(
        &fx.repo,
        "main",
        "flake_fix.txt",
        "fixed flaky test\n",
        "fix: base branch flake fix",
    );
    forge.open_delivery(PR, &task, &head_branch, &pr_head);

    let refreshed = fx
        .as_brokered_cycle(&mandate, &run, || call_refresh(&fx.runtime, &task, &ws))
        .expect("refresh succeeds on a conflict-free behind delivery PR");

    assert_eq!(refreshed["task_id"], task.as_str(), "{refreshed}");
    assert_eq!(refreshed["pull_request"], PR, "{refreshed}");
    assert_eq!(refreshed["base_branch"], "main", "{refreshed}");
    assert_eq!(refreshed["previous_head"], pr_head.as_str(), "{refreshed}");
    assert_eq!(refreshed["outcome"], "updated", "{refreshed}");
    assert_eq!(refreshed["checks_started"], true, "{refreshed}");
    assert!(refreshed["resident_action_key"].is_string(), "{refreshed}");

    let new_head = refreshed["head_sha"].as_str().unwrap().to_string();
    assert_ne!(new_head, pr_head);
    assert_eq!(forge.head(PR), new_head);
    // Verify it is one merge commit of `base_tip` into `pr_head` (never rebased).
    let parents: Vec<String> = git_out(&fx.repo, &["rev-list", "--parents", "-n", "1", &new_head])
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect();
    assert_eq!(
        parents,
        vec![pr_head.clone(), base_tip.clone()],
        "refresh must create one merge commit keeping the published commit as first parent"
    );
    assert_eq!(
        forge.0.lock().unwrap().checks_started_on,
        vec![(PR, new_head.clone())],
        "required checks must start on the refreshed head"
    );
    // Refresh never lands the PR or changes task status.
    assert!(forge.0.lock().unwrap().merge_requests.is_empty());
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().status,
        TaskStatus::Blocked
    );

    let rows = action_rows(&fx, &mandate, "orbit.workflow.refresh");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].phase, ResidentActionPhase::Ok, "{rows:?}");
    assert_eq!(rows[0].subject.as_deref(), Some(task.as_str()), "{rows:?}");

    // Repeating refresh when the head already contains `base_tip` is a no-op.
    let repeated = fx
        .as_brokered_cycle(&mandate, &run, || call_refresh(&fx.runtime, &task, &ws))
        .expect("repeated refresh on up-to-date head is a no-op");
    assert_eq!(repeated["outcome"], "already_up_to_date", "{repeated}");
    assert_eq!(repeated["checks_started"], false, "{repeated}");
    assert_eq!(repeated["head_sha"], new_head.as_str(), "{repeated}");
    assert_eq!(forge.head(PR), new_head);
    assert_eq!(
        forge.0.lock().unwrap().checks_started_on.len(),
        1,
        "no-op refresh must not retrigger checks"
    );
}

#[cfg(unix)]
#[test]
fn refresh_requires_the_right_and_covered_workspace_scope() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::refresh::refresh_requires_the_right_and_covered_workspace_scope",
    ) {
        return;
    }
    let pr_head = {
        let mut fx = fixture();
        let forge = Arc::new(RefreshForge::default());
        let (mandate, run) = live_cycle(&mut fx, &forge, &["dispatch", "merge"]);
        let ws = fx.runtime.workspace_id().unwrap();
        let task = delivery_task(&fx, TaskStatus::Review, &[PR]);
        let head_branch = format!("orbit/{task}-47642870");
        git_out(&fx.repo, &["checkout", "-qb", &head_branch, "main"]);
        let pr_head = commit_file(&fx.repo, &head_branch, "a.txt", "a\n", "feat: a");
        let _base_tip = commit_file(&fx.repo, "main", "b.txt", "b\n", "fix: b");
        forge.open_delivery(PR, &task, &head_branch, &pr_head);

        // Without the `refresh` right, the cycle is refused as `resident_right_refused`.
        let err = fx
            .as_brokered_cycle(&mandate, &run, || call_refresh(&fx.runtime, &task, &ws))
            .expect_err("missing refresh right must be refused");
        assert!(err.contains("resident_right_refused"), "{err}");
        assert!(err.contains("refresh"), "{err}");
        assert_eq!(forge.head(PR), pr_head);
        assert!(action_rows(&fx, &mandate, "orbit.workflow.refresh").is_empty());

        // Outside a resident cycle, an agent session is held to the operator row.
        let err = call_refresh(&fx.runtime, &task, &ws)
            .expect_err("an agent session outside a cycle is no operator");
        assert!(err.contains("capability"), "{err}");
        pr_head
    };

    // Now test scope refusal: grant `refresh`, add a second uncovered workspace,
    // and attempt to refresh a task outside the covered workspaces.
    let mut fx2 = fixture();
    let forge2 = Arc::new(RefreshForge::default());
    fx2.runtime = fx2.runtime.clone().with_task_pr_forge(forge2.clone());
    let other_root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let other_repo = other_root.path().join("other");
    std::fs::create_dir_all(other_repo.join(".orbit")).unwrap();
    std::fs::write(other_repo.join(".orbit/config.toml"), CONFIG).unwrap();
    let other_raw = OrbitRuntime::from_roots(&fx2.global, &other_repo.join(".orbit"))
        .unwrap()
        .with_task_pr_forge(forge2.clone());
    let home_id = fx2.runtime.workspace_id().unwrap();
    let other_id = other_raw.workspace_id().unwrap();
    let catalog = Arc::new(TwoWorkspaces {
        targets: vec![
            FederatedWorkspaceTarget {
                workspace_id: home_id.clone(),
                name: fx2.runtime.workspace_label(),
                repo_root: fx2.repo.clone(),
            },
            FederatedWorkspaceTarget {
                workspace_id: other_id.clone(),
                name: "other".into(),
                repo_root: other_repo,
            },
        ],
        runtimes: OnceLock::new(),
    });
    let home_rt = fx2.runtime.clone().with_workspace_catalog(catalog.clone());
    let other_rt = other_raw.with_workspace_catalog(catalog.clone());
    let _ = catalog
        .runtimes
        .set(vec![home_rt.clone(), other_rt.clone()]);
    fx2.runtime = home_rt;

    let watched = fx2.task("Watched");
    let mandate2 = fx2.mandate(&watched);
    fx2.grant(&mandate2, &["refresh"]);
    let run2 = fx2.cycle_run(&mandate2, "resident-cycle-refresh-scope");
    fx2.runtime.begin_resident_cycle(&run2, Utc::now()).unwrap();
    let foreign_task = other_rt
        .add_task(TaskAddParams {
            title: "Uncovered workspace leaf".into(),
            description: "Outside covered workspaces.".into(),
            acceptance_criteria: vec!["Done.".into()],
            plan: "Plan.".into(),
            context_files: vec!["file:.gitignore".into()],
            complexity: TaskComplexity::Low,
            status: Some(TaskStatus::Review),
            external_refs: vec![ExternalRef::github_pr(PR.to_string()).unwrap()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    forge2.open_delivery(
        PR,
        &foreign_task,
        &format!("orbit/{foreign_task}-11111111"),
        &pr_head,
    );

    let err = fx2
        .as_brokered_cycle(&mandate2, &run2, || {
            call_refresh(&fx2.runtime, &foreign_task, &other_id)
        })
        .expect_err("naming an uncovered workspace must be refused");
    assert!(err.contains("resident_scope_refused"), "{err}");
    assert!(forge2.0.lock().unwrap().refresh_calls.is_empty());
    assert!(action_rows(&fx2, &mandate2, "orbit.workflow.refresh").is_empty());
}

#[cfg(unix)]
#[test]
fn a_conflicting_refresh_is_refused_names_conflicting_paths_and_leaves_head_unchanged() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::refresh::a_conflicting_refresh_is_refused_names_conflicting_paths_and_leaves_head_unchanged",
    ) {
        return;
    }
    let mut fx = fixture();
    let forge = Arc::new(RefreshForge::default());
    let (mandate, run) = live_cycle(&mut fx, &forge, &["refresh"]);
    let ws = fx.runtime.workspace_id().unwrap();
    let task = delivery_task(&fx, TaskStatus::Blocked, &[PR]);
    let head_branch = format!("orbit/{task}-47642870");

    // Seed shared files on `main`, then make conflicting edits on both branches.
    commit_file(&fx.repo, "main", "src/alpha.txt", "base\n", "seed alpha");
    commit_file(&fx.repo, "main", "docs/beta.txt", "base\n", "seed beta");
    git_out(&fx.repo, &["checkout", "-qb", &head_branch, "main"]);
    commit_file(
        &fx.repo,
        &head_branch,
        "src/alpha.txt",
        "pr alpha\n",
        "pr edit alpha",
    );
    let pr_head = commit_file(
        &fx.repo,
        &head_branch,
        "docs/beta.txt",
        "pr beta\n",
        "pr edit beta",
    );
    commit_file(
        &fx.repo,
        "main",
        "src/alpha.txt",
        "main alpha\n",
        "main edit alpha",
    );
    commit_file(
        &fx.repo,
        "main",
        "docs/beta.txt",
        "main beta\n",
        "main edit beta",
    );
    forge.open_delivery(PR, &task, &head_branch, &pr_head);

    let err = fx
        .as_brokered_cycle(&mandate, &run, || call_refresh(&fx.runtime, &task, &ws))
        .expect_err("conflicting refresh must be refused");

    assert!(err.contains("task_refresh_refused"), "{err}");
    assert!(
        err.contains("src/alpha.txt"),
        "must name src/alpha.txt: {err}"
    );
    assert!(
        err.contains("docs/beta.txt"),
        "must name docs/beta.txt: {err}"
    );
    assert_eq!(forge.head(PR), pr_head, "head must stay unchanged");
    assert_eq!(
        git_out(&fx.repo, &["rev-parse", &head_branch]),
        pr_head,
        "git branch must stay unchanged"
    );
    assert!(forge.0.lock().unwrap().checks_started_on.is_empty());
    let rows = action_rows(&fx, &mandate, "orbit.workflow.refresh");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].phase, ResidentActionPhase::Refused, "{rows:?}");
}

#[cfg(unix)]
#[test]
fn refresh_refuses_a_pr_owned_by_a_live_run_and_a_pr_not_published_by_orbit() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::refresh::refresh_refuses_a_pr_owned_by_a_live_run_and_a_pr_not_published_by_orbit",
    ) {
        return;
    }
    let mut fx = fixture();
    let forge = Arc::new(RefreshForge::default());
    let (mandate, run) = live_cycle(&mut fx, &forge, &["refresh"]);
    let ws = fx.runtime.workspace_id().unwrap();

    // Case 1: PR owned by a live delivery run.
    let live_task = delivery_task(&fx, TaskStatus::Review, &[PR]);
    let live_branch = format!("orbit/{live_task}-47642870");
    git_out(&fx.repo, &["checkout", "-qb", &live_branch, "main"]);
    let live_head = commit_file(&fx.repo, &live_branch, "live.txt", "live\n", "live commit");
    commit_file(&fx.repo, "main", "base1.txt", "base1\n", "base commit");
    forge.open_delivery(PR, &live_task, &live_branch, &live_head);

    let live_run = fx
        .jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids": [live_task]})),
            None,
        )
        .unwrap();
    fx.jobs
        .mark_job_run_running(&live_run.run_id, Utc::now(), std::process::id())
        .unwrap();
    fx.runtime
        .update_task_as_human(
            &live_task,
            TaskUpdateParams {
                job_run_id: Some(Some(live_run.run_id.clone())),
                ..Default::default()
            },
            "system".into(),
        )
        .unwrap();

    let err = fx
        .as_brokered_cycle(&mandate, &run, || {
            call_refresh(&fx.runtime, &live_task, &ws)
        })
        .expect_err("a PR owned by a live run must be refused");
    assert!(err.contains("task_refresh_refused"), "{err}");
    assert_eq!(forge.head(PR), live_head);
    assert!(forge.0.lock().unwrap().refresh_calls.is_empty());

    // Case 2: PR not published by an Orbit delivery (foreign branch & author).
    let foreign_pr = PR + 1;
    let foreign_task = delivery_task(&fx, TaskStatus::Review, &[foreign_pr]);
    forge.open_foreign(foreign_pr, "feature/manual-branch", "alice", &live_head);

    let err = fx
        .as_brokered_cycle(&mandate, &run, || {
            call_refresh(&fx.runtime, &foreign_task, &ws)
        })
        .expect_err("a non-Orbit PR must be refused");
    assert!(err.contains("task_refresh_refused"), "{err}");
    assert!(forge.0.lock().unwrap().refresh_calls.is_empty());

    // Case 3: Task with no Orbit delivery PR recorded at all.
    let no_pr_task = delivery_task(&fx, TaskStatus::Blocked, &[]);
    let err = fx
        .as_brokered_cycle(&mandate, &run, || {
            call_refresh(&fx.runtime, &no_pr_task, &ws)
        })
        .expect_err("a task with no recorded delivery PR must be refused");
    assert!(err.contains("task_refresh_refused"), "{err}");
    assert!(forge.0.lock().unwrap().refresh_calls.is_empty());
}

#[test]
fn landing_accepts_conflict_free_refreshed_head_with_passing_checks_and_refuses_other_commits() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::refresh::landing_accepts_conflict_free_refreshed_head_with_passing_checks_and_refuses_other_commits",
    ) {
        return;
    }
    let mut fx = fixture();
    // Enable pre-landing review policy so landing enforces review coverage.
    std::fs::write(
        fx.repo.join(".orbit/config.toml"),
        format!(
            "{CONFIG}\n[review]\nbefore_landing = true\n\n[workflow.purpose_crews]\nreview = [\"sol\"]\n"
        ),
    )
    .unwrap();
    let forge = Arc::new(RefreshForge::default());
    fx.runtime = OrbitRuntime::from_roots(&fx.global, &fx.repo.join(".orbit"))
        .unwrap()
        .with_task_pr_forge(forge.clone());

    let base0 = git_out(&fx.repo, &["rev-parse", "main"]);

    // --- Part A: Accepted conflict-free refresh chain (two base refreshes) ---
    let task_ok = delivery_task(&fx, TaskStatus::Review, &[PR]);
    let branch_ok = format!("orbit/{task_ok}-aaaaaaaa");
    git_out(&fx.repo, &["checkout", "-qb", &branch_ok, "main"]);
    let reviewed_head = commit_file(
        &fx.repo,
        &branch_ok,
        "deliverable.txt",
        "reviewed implementation\n",
        "feat: reviewed implementation",
    );
    record_passing_review_certificate(&fx, &task_ok, &base0, &reviewed_head);

    // Advance base twice and perform two conflict-free refresh merges onto the PR.
    let base1 = commit_file(&fx.repo, "main", "base_fix_1.txt", "fix 1\n", "fix: base 1");
    let tree1 = match inspect_base_merge(
        &orbit_common::Env::capture(),
        &fx.repo,
        &reviewed_head,
        &base1,
    )
    .unwrap()
    {
        BaseMergeInspection::Clean { merged_tree } => merged_tree,
        other => panic!("expected clean merge 1, got {other:?}"),
    };
    let refresh1 = git_out(
        &fx.repo,
        &[
            "commit-tree",
            &tree1,
            "-p",
            &reviewed_head,
            "-p",
            &base1,
            "-m",
            "Merge branch 'main' into delivery",
        ],
    );
    let base2 = commit_file(&fx.repo, "main", "base_fix_2.txt", "fix 2\n", "fix: base 2");
    let tree2 = match inspect_base_merge(&orbit_common::Env::capture(), &fx.repo, &refresh1, &base2)
        .unwrap()
    {
        BaseMergeInspection::Clean { merged_tree } => merged_tree,
        other => panic!("expected clean merge 2, got {other:?}"),
    };
    let refresh2 = git_out(
        &fx.repo,
        &[
            "commit-tree",
            &tree2,
            "-p",
            &refresh1,
            "-p",
            &base2,
            "-m",
            "Merge branch 'main' into delivery",
        ],
    );
    forge.open_delivery(PR, &task_ok, &branch_ok, &refresh2);

    // Before checks pass on the refreshed head, landing refuses.
    forge.set_checks(PR, &[], &["Check / Clippy / Test"], &[]);
    let pending_err = fx
        .runtime
        .land_task_pull_request(&task_ok, Some("operator"))
        .expect_err("pending checks on refreshed head must refuse landing")
        .to_string();
    assert!(
        pending_err.contains("task_landing_refused"),
        "{pending_err}"
    );
    assert!(forge.0.lock().unwrap().merge_requests.is_empty());

    // Absent checks on the refreshed head must also refuse landing.
    forge.set_checks(PR, &[], &[], &[]);
    let absent_err = fx
        .runtime
        .land_task_pull_request(&task_ok, Some("operator"))
        .expect_err("refreshed head with no passed checks must refuse landing")
        .to_string();
    assert!(absent_err.contains("task_landing_refused"), "{absent_err}");
    assert!(forge.0.lock().unwrap().merge_requests.is_empty());

    // Once required checks pass on `refresh2`, landing accepts the prior review
    // and merges `refresh2` cleanly!
    forge.set_checks(PR, &["Check / Clippy / Test"], &[], &[]);
    let landed = fx
        .runtime
        .land_task_pull_request(&task_ok, Some("operator"))
        .expect("refreshed head with clean base merges and passing checks must land");
    assert_eq!(landed.status, TaskStatus::Done);
    assert_eq!(landed.pull_request, PR);
    assert_eq!(
        forge.0.lock().unwrap().merge_requests,
        vec![(PR, refresh2.clone())]
    );

    // --- Part B: Any other commit after the reviewed head is refused ---
    let pr_bad = PR + 10;
    let task_bad = delivery_task(&fx, TaskStatus::Review, &[pr_bad]);
    let branch_bad = format!("orbit/{task_bad}-bbbbbbbb");
    git_out(&fx.repo, &["checkout", "-qb", &branch_bad, &base0]);
    let reviewed_bad = commit_file(
        &fx.repo,
        &branch_bad,
        "other.txt",
        "reviewed\n",
        "feat: reviewed",
    );
    record_passing_review_certificate(&fx, &task_bad, &base0, &reviewed_bad);
    forge.open_delivery(pr_bad, &task_bad, &branch_bad, &reviewed_bad);
    forge.set_checks(pr_bad, &["Check / Clippy / Test"], &[], &[]);

    // B1: A regular edit commit after the reviewed head.
    let extra_commit = commit_file(
        &fx.repo,
        &branch_bad,
        "other.txt",
        "unreviewed edit\n",
        "chore: unreviewed edit",
    );
    forge.set_head(pr_bad, &extra_commit);
    let err = fx
        .runtime
        .land_task_pull_request(&task_bad, Some("operator"))
        .expect_err("extra edit commit after reviewed head must be refused")
        .to_string();
    assert!(
        err.contains("task_landing_refused") && err.contains("pre-landing review"),
        "{err}"
    );

    // B2: A merge commit of base that smuggles an extra edit (tree != clean merge-tree).
    let smuggled_tree = git_out(
        &fx.repo,
        &["rev-parse", &format!("{extra_commit}^{{tree}}")],
    );
    let dirty_merge = git_out(
        &fx.repo,
        &[
            "commit-tree",
            &smuggled_tree,
            "-p",
            &reviewed_bad,
            "-p",
            &base2,
            "-m",
            "Merge branch 'main' with smuggled edit",
        ],
    );
    forge.set_head(pr_bad, &dirty_merge);
    let err = fx
        .runtime
        .land_task_pull_request(&task_bad, Some("operator"))
        .expect_err("merge commit with conflict resolution / extra edit must be refused")
        .to_string();
    assert!(
        err.contains("task_landing_refused") && err.contains("pre-landing review"),
        "{err}"
    );

    // B3: A merge commit merging a non-base branch commit.
    git_out(&fx.repo, &["checkout", "-qb", "side-branch", &base0]);
    let side_commit = commit_file(&fx.repo, "side-branch", "side.txt", "side\n", "side commit");
    let side_tree = match inspect_base_merge(
        &orbit_common::Env::capture(),
        &fx.repo,
        &reviewed_bad,
        &side_commit,
    )
    .unwrap()
    {
        BaseMergeInspection::Clean { merged_tree } => merged_tree,
        other => panic!("expected clean side merge, got {other:?}"),
    };
    let non_base_merge = git_out(
        &fx.repo,
        &[
            "commit-tree",
            &side_tree,
            "-p",
            &reviewed_bad,
            "-p",
            &side_commit,
            "-m",
            "Merge non-base branch",
        ],
    );
    forge.set_head(pr_bad, &non_base_merge);
    let err = fx
        .runtime
        .land_task_pull_request(&task_bad, Some("operator"))
        .expect_err("merging a non-base branch must be refused")
        .to_string();
    assert!(
        err.contains("task_landing_refused") && err.contains("pre-landing review"),
        "{err}"
    );

    assert_eq!(
        forge.0.lock().unwrap().merge_requests.len(),
        1,
        "none of the unreviewed heads may merge"
    );
    assert_eq!(
        fx.runtime.get_task(&task_bad).unwrap().status,
        TaskStatus::Review
    );

    forge.set_head(pr_bad, &extra_commit);
    // A fresh review of the changed head does not waive its required checks.
    record_passing_review_certificate(&fx, &task_bad, &base0, &extra_commit);
    forge.set_checks(pr_bad, &[], &[], &["Check / Clippy / Test"]);
    let err = fx
        .runtime
        .land_task_pull_request(&task_bad, Some("operator"))
        .expect_err("the reviewed changed head still has a failed required check")
        .to_string();
    assert!(
        err.contains("task_landing_refused") && err.contains("Check / Clippy / Test"),
        "{err}"
    );
    assert!(
        !forge
            .0
            .lock()
            .unwrap()
            .merge_requests
            .iter()
            .any(|(pr, _)| *pr == pr_bad)
    );
}

#[test]
fn resident_grant_adds_and_removes_refresh_like_the_other_rights() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::refresh::resident_grant_adds_and_removes_refresh_like_the_other_rights",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Watched");
    let mandate = fx.mandate(&watched);
    let ws = fx.runtime.workspace_id().unwrap();

    let added = fx.operator_call(
        "orbit.resident.grant",
        json!({"task_id": mandate, "add": ["refresh"], "workspaces": [ws]}),
    );
    assert_eq!(added["outcome"], "granted", "{added}");
    assert_eq!(added["authority"]["rights"], json!(["refresh"]), "{added}");

    let removed = fx.operator_call(
        "orbit.resident.grant",
        json!({"task_id": mandate, "remove": ["refresh"]}),
    );
    assert_eq!(removed["outcome"], "narrowed", "{removed}");
    assert_eq!(removed["authority"]["rights"], json!([]), "{removed}");
}
