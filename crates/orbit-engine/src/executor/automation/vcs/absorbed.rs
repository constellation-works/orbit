//! [ORB-14668] A conflict-recovered candidate its pinned base already holds.
//!
//! `sync_base` stops on a conflict, conflict recovery resolves it to the
//! base's side, and `rebase --continue` drops the now-empty pick: the branch
//! ends exactly on the pinned base (or on the advanced tip the host then
//! follows, when that drops the picks instead). When that base holds the
//! candidate's exact content (mode and blob) for every path the candidate
//! changed, the host certifies the continuation as *absorbed* with the oldest
//! commit reachable from it, and not from the candidate's original base, that
//! holds that content. A resolution that discarded the candidate for the
//! base's different content is refused and reaches the final-recovery agent.
//!
//! Nothing is left to deliver, so the `git_rebase` retry fails the step with
//! [`CANDIDATE_ABSORBED_MARKER`] instead of the generic empty-branch refusal,
//! and the job's final recovery hands the host a deterministic
//! `complete_no_diff` for the covering commit. The host applier keeps its own
//! reachability check, the run's completion authority (`review` unless the
//! run holds `done`) and, for a claimed leaf, the owner's settlement. No
//! review gate is exempted: the run publishes nothing.
//!
//! Each consumer re-observes the certified evidence through
//! [`verify_absorbed_candidate`]; a refusal names its [`AbsorbedReason`] and
//! leaves today's fail-closed path in place.

use std::fmt;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::Task;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::context::RuntimeHost;

use super::freshness::recorded_workspace_matches;
use super::git::{git_command_success, git_output, git_output_paths, git_output_raw};
use super::handoff::rebase_in_progress;

/// Leads a step failure whose candidate the pinned base already holds.
pub(crate) const CANDIDATE_ABSORBED_MARKER: &str = "[candidate_absorbed]";

/// The only step whose conflict recovery may end absorbed: the
/// pre-publication base synchronization. A completion step has already
/// published a reviewed PR, which this route does not settle.
pub(crate) const ABSORBING_STEP: &str = "sync_base";

/// Whether a step failure reports an absorbed candidate.
pub(crate) fn is_candidate_absorbed(message: &str) -> bool {
    message.contains(CANDIDATE_ABSORBED_MARKER)
}

/// Why a candidate left on its pinned base is not settled as absorbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbsorbedReason {
    /// No recovery checkpoint of this run's step records an absorbed candidate.
    NoAbsorbedCheckpoint,
    /// The stored checkpoint names another run, step, task or checkout, or is
    /// not the host's current certificate for this run and step.
    StaleCheckpoint,
    /// The checkout is not on the checkpointed branch.
    WrongBranch,
    /// The branch is not exactly on the pinned base.
    NotOnPinnedBase,
    /// A rebase is in progress in the checkout.
    RebaseInProgress,
    /// The checkout has tracked or untracked changes.
    DirtyWorktree,
    /// The base does not hold the candidate's content for every path it
    /// changed, or no commit reachable from that base introduced it.
    NoCoveringCommit,
    /// The covering commit is not reachable from the freshly resolved base ref.
    CoveringUnreachable,
    /// The task's title, description or criteria changed since recovery
    /// was admitted, or could not be read.
    TaskScopeChanged,
    /// Git or the host could not answer.
    Unverifiable,
}

impl AbsorbedReason {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::NoAbsorbedCheckpoint => "no_absorbed_checkpoint",
            Self::StaleCheckpoint => "stale_checkpoint",
            Self::WrongBranch => "wrong_branch",
            Self::NotOnPinnedBase => "not_on_pinned_base",
            Self::RebaseInProgress => "rebase_in_progress",
            Self::DirtyWorktree => "dirty_worktree",
            Self::NoCoveringCommit => "no_covering_commit",
            Self::CoveringUnreachable => "covering_unreachable",
            Self::TaskScopeChanged => "task_scope_changed",
            Self::Unverifiable => "unverifiable",
        }
    }
}

/// A typed refusal with what was observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AbsorbedRefusal {
    pub(crate) reason: AbsorbedReason,
    pub(crate) detail: String,
}

impl AbsorbedRefusal {
    pub(crate) fn new(reason: AbsorbedReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }

    /// The refusal as the final-recovery agent reads it.
    pub(crate) fn evidence(&self) -> Value {
        json!({ "reason": self.reason.code(), "detail": self.detail })
    }
}

impl fmt::Display for AbsorbedRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "absorbed_candidate_refused: {}: {}",
            self.reason.code(),
            self.detail
        )
    }
}

impl From<OrbitError> for AbsorbedRefusal {
    fn from(error: OrbitError) -> Self {
        Self::new(AbsorbedReason::Unverifiable, error.to_string())
    }
}

/// A verified absorbed candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AbsorbedCandidate {
    pub(crate) covering_commit: String,
    /// The base the branch sits on: the pin, or the tip recovery followed.
    pub(crate) base_sha: String,
    pub(crate) base_ref: String,
}

impl AbsorbedCandidate {
    /// The step failure that hands this candidate to verified settlement.
    pub(crate) fn failure_text(&self, head: &str) -> String {
        format!(
            "{CANDIDATE_ABSORBED_MARKER} git_rebase: conflict recovery left '{head}' on base \
             '{}' with no candidate commit; covering commit {} on '{}' already carries the \
             candidate's change, so verified no-diff settlement completes the task",
            self.base_sha, self.covering_commit, self.base_ref
        )
    }
}

/// What a candidate means: its title, description and acceptance criteria.
/// Context selectors are preparation hints the run itself widens.
pub(crate) fn task_scope_digest(task: &Task) -> String {
    let scope = json!({
        "title": task.title,
        "description": task.description,
        "acceptance_criteria": task.acceptance_criteria,
    });
    format!("{:x}", Sha256::digest(scope.to_string().as_bytes()))
}

/// The paths the candidate `original_base..original_head` changed, and the
/// oldest commit in `original_base..landed_base` whose tree holds the
/// candidate's version (mode and blob) of every one of them. `None` when the
/// candidate changed nothing, or when `landed_base` itself does not hold that
/// version: the resolution then dropped the candidate rather than finding it
/// already landed. A base commit that merely touched the same paths, as the
/// commit a rebase conflicted on always did, is no evidence.
pub(crate) fn covering_commit(
    root: &Path,
    original_base: &str,
    original_head: &str,
    landed_base: &str,
) -> Result<Option<(String, Vec<String>)>, OrbitError> {
    let candidate_paths = git_output_paths(
        root,
        &[
            "--literal-pathspecs",
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            original_base,
            original_head,
            "--",
        ],
    )?;
    if candidate_paths.is_empty() {
        return Ok(None);
    }
    // `diff-tree --quiet` exits 0 only when both trees agree on every path;
    // a difference and a Git error both leave the candidate uncovered.
    let holds_candidate = |commit: &str| {
        let mut args = vec![
            "--literal-pathspecs",
            "diff-tree",
            "-r",
            "--quiet",
            "--no-renames",
            commit,
            original_head,
            "--",
        ];
        args.extend(candidate_paths.iter().map(String::as_str));
        git_command_success(root, &args)
    };
    if !holds_candidate(landed_base)? {
        return Ok(None);
    }
    let range = format!("{original_base}..{landed_base}");
    let mut args = vec![
        "--literal-pathspecs",
        "rev-list",
        "--reverse",
        range.as_str(),
        "--",
    ];
    args.extend(candidate_paths.iter().map(String::as_str));
    for commit in git_output_raw(root, &args)?.lines().map(str::trim) {
        if !commit.is_empty() && holds_candidate(commit)? {
            return Ok(Some((commit.to_string(), candidate_paths)));
        }
    }
    Ok(None)
}

/// Re-observe the certified absorbed continuation of `run_id`'s `step_id`
/// in `workspace`, for `task_id`: the certificate and its identity, the
/// checkout's branch, HEAD and cleanliness, the covering commit, its
/// reachability from the freshly resolved base ref, and the task's scope.
pub(crate) fn verify_absorbed_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    step_id: &str,
    task_id: &str,
    workspace: &Path,
) -> Result<AbsorbedCandidate, AbsorbedRefusal> {
    use AbsorbedReason as Reason;
    let checkpoint = host
        .read_run_state(run_id)?
        .and_then(|state| state.rebase_recovery_checkpoints.get(step_id).cloned())
        .filter(|checkpoint| checkpoint.get("absorbed").is_some_and(Value::is_object))
        .ok_or_else(|| {
            AbsorbedRefusal::new(
                Reason::NoAbsorbedCheckpoint,
                format!("run {run_id} step `{step_id}` recorded no absorbed continuation"),
            )
        })?;
    let text =
        |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    if step_id != ABSORBING_STEP
        || text(&checkpoint, "run_id").as_deref() != Some(run_id)
        || text(&checkpoint, "step_id").as_deref() != Some(step_id)
        || checkpoint["task_ids"] != json!([task_id])
        || !recorded_workspace_matches(
            checkpoint.get("workspace_path").and_then(Value::as_str),
            workspace,
        )
    {
        return Err(AbsorbedRefusal::new(
            Reason::StaleCheckpoint,
            "the checkpoint describes another run, step, task or checkout",
        ));
    }
    if !host.verify_rebase_recovery(run_id, step_id, &checkpoint)? {
        return Err(AbsorbedRefusal::new(
            Reason::StaleCheckpoint,
            "the stored checkpoint is not the host's current certificate for this run and step",
        ));
    }
    let field = |value: &Value, key: &str| {
        text(value, key).ok_or_else(|| {
            AbsorbedRefusal::new(
                Reason::StaleCheckpoint,
                format!("the certified checkpoint has no `{key}`"),
            )
        })
    };
    let absorbed = &checkpoint["absorbed"];
    let branch = field(&checkpoint, "head")?;
    let head_sha = field(&checkpoint, "head_sha")?;
    let pinned = field(&checkpoint, "target_base_sha")?;
    let landed = field(&checkpoint, "base_sha")?;
    let original_head = field(&checkpoint, "head_sha_before")?;
    let original_base = field(&checkpoint, "original_base_sha")?;
    let base_ref = field(&checkpoint, "base_ref")?;
    let covering = field(absorbed, "covering_commit")?;
    let recorded_scope = field(absorbed, "task_scope_digest")?;

    // A stopped rebase detaches HEAD, so it is named before the branch.
    if rebase_in_progress(workspace)? {
        return Err(AbsorbedRefusal::new(
            Reason::RebaseInProgress,
            "rebase-merge or rebase-apply state is present",
        ));
    }
    let checked_out = git_output(workspace, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if checked_out != branch {
        return Err(AbsorbedRefusal::new(
            Reason::WrongBranch,
            format!("'{checked_out}' is checked out, not '{branch}'"),
        ));
    }
    let head = git_output(workspace, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if head != landed
        || head_sha != landed
        || !git_command_success(
            workspace,
            &["merge-base", "--is-ancestor", &pinned, &landed],
        )?
    {
        return Err(AbsorbedRefusal::new(
            Reason::NotOnPinnedBase,
            format!("'{branch}' is at {head}, not the base {landed} its pinned rebase landed on"),
        ));
    }
    if !git_output_raw(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err(AbsorbedRefusal::new(
            Reason::DirtyWorktree,
            "the checkout has uncommitted or untracked changes",
        ));
    }
    match covering_commit(workspace, &original_base, &original_head, &landed)? {
        Some((found, _)) if found == covering => {}
        _ => {
            return Err(AbsorbedRefusal::new(
                Reason::NoCoveringCommit,
                format!(
                    "{landed} does not hold the candidate {original_head}'s content as \
                     introduced by {covering}"
                ),
            ));
        }
    }
    let live = git_output(
        workspace,
        &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
    )?;
    if !git_command_success(
        workspace,
        &["merge-base", "--is-ancestor", &covering, &live],
    )? {
        return Err(AbsorbedRefusal::new(
            Reason::CoveringUnreachable,
            format!("{covering} is not reachable from '{base_ref}' ({live})"),
        ));
    }
    let scope = host
        .get_task(task_id)
        .map(|task| task_scope_digest(&task))
        .map_err(|error| AbsorbedRefusal::new(Reason::TaskScopeChanged, error.to_string()))?;
    if scope != recorded_scope {
        return Err(AbsorbedRefusal::new(
            Reason::TaskScopeChanged,
            format!(
                "task {task_id}'s title, description or criteria changed since recovery was \
                 admitted"
            ),
        ));
    }
    Ok(AbsorbedCandidate {
        covering_commit: covering,
        base_sha: landed,
        base_ref,
    })
}
