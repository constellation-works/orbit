//! Git mechanics for the before-PR review gate [ORB-11333].
//!
//! The engine collects candidate identity (base and head commits and trees,
//! the implementation commits with the attribution Git recorded) and appends
//! reviewer-authored repair commits with the reviewer's own agent-family
//! identity. Whether a gate applies, who may review, what the verdict means
//! and where the evidence is persisted are Core decisions.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::CommitIdentity;
use orbit_types::workflow::automation::SourceRevision;

pub use super::baseline::{BaseFailureCheck, BaseFailureVerdict, verify_base_failure};
use super::commit::{
    commit_reviewer_repairs_in, reviewer_repair_identity, stage_everything, staged_paths,
};
use super::git::{
    base_sync_mode_from_input, git_command_success, git_failure_error, git_output, git_output_raw,
    git_run_bytes, git_success, git_timeout_error, resolve_worktree_start_point,
};

/// Commit-message trailer naming the review attempt a repair commit belongs to.
pub const REVIEW_ATTEMPT_TRAILER: &str = "Orbit-Review-Attempt";

/// The catalog activity a before-PR reviewer runs as.
pub const REVIEWER_ACTIVITY: &str = "agent_review_repair";

/// The pinned candidate a reviewer is handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateIdentity {
    pub base: SourceRevision,
    pub head: SourceRevision,
    /// Commits in `base..head`, oldest first.
    pub commits: Vec<CommitIdentity>,
}

/// Resolve a commit-ish to its commit and tree ids.
pub fn revision(workspace_path: &Path, spec: &str) -> Result<SourceRevision, OrbitError> {
    let commit = git_output(
        workspace_path,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{spec}^{{commit}}"),
        ],
    )?;
    let tree = git_output(
        workspace_path,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{commit}^{{tree}}"),
        ],
    )?;
    Ok(SourceRevision { commit, tree })
}

/// The base the checked-out candidate was synchronized onto: the merge base
/// of HEAD and the configured base ref under the run's sync mode. After the
/// pipeline's rebase step this is the base tip the candidate sits on.
pub fn synchronized_base(
    workspace_path: &Path,
    base: &str,
    base_sync: &str,
) -> Result<SourceRevision, OrbitError> {
    let mode = base_sync_mode_from_input(&serde_json::json!({ "base_sync": base_sync }))?;
    let base_ref = resolve_worktree_start_point(workspace_path, base, mode)?;
    let merge_base = git_output(workspace_path, &["merge-base", "HEAD", &base_ref])?;
    revision(workspace_path, &merge_base)
}

/// The candidate currently checked out, pinned against `base_sha`.
pub fn candidate_identity(
    workspace_path: &Path,
    base_sha: &str,
) -> Result<CandidateIdentity, OrbitError> {
    candidate_identity_at(workspace_path, base_sha, "HEAD")
}

/// The candidate at commit-ish `head`, pinned against `base_sha`.
pub fn candidate_identity_at(
    workspace_path: &Path,
    base_sha: &str,
    head: &str,
) -> Result<CandidateIdentity, OrbitError> {
    let base = revision(workspace_path, base_sha)?;
    let head = revision(workspace_path, head)?;
    let commits = commits_between(workspace_path, &base.commit, &head.commit)?;
    Ok(CandidateIdentity {
        base,
        head,
        commits,
    })
}

/// Commits in `base..head` with the attribution Git recorded, oldest first.
pub fn commits_between(
    workspace_path: &Path,
    base: &str,
    head: &str,
) -> Result<Vec<CommitIdentity>, OrbitError> {
    let raw = git_output_raw(
        workspace_path,
        &[
            "log",
            "--reverse",
            "--first-parent",
            "--format=%H%x1f%T%x1f%an <%ae>%x1f%cn <%ce>%x1f%s%x1e",
            &format!("{base}..{head}"),
        ],
    )?;
    raw.split('\u{1e}')
        .map(str::trim)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let mut fields = record.split('\u{1f}');
            let mut next = |name: &str| {
                fields.next().map(str::to_string).ok_or_else(|| {
                    OrbitError::Execution(format!(
                        "git log omitted the {name} field for a candidate commit"
                    ))
                })
            };
            Ok(CommitIdentity {
                commit: next("commit")?,
                tree: next("tree")?,
                author: next("author")?,
                committer: next("committer")?,
                subject: next("subject")?,
            })
        })
        .collect()
}

/// Paths the reviewer changed in the worktree and has not committed,
/// including new files.
pub fn uncommitted_paths(workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    let status = git_output_raw(
        workspace_path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut paths = Vec::new();
    let mut fields = status.split('\0');
    while let Some(entry) = fields.next() {
        if entry.is_empty() {
            continue;
        }
        let mut codes = entry.chars();
        let (Some(index_state), Some(_worktree_state)) = (codes.next(), codes.next()) else {
            continue;
        };
        // A rename or copy is followed by a second NUL-terminated field holding
        // the source path, which belongs to the record before it rather than
        // starting a new record.
        if matches!(index_state, 'R' | 'C') {
            let _ = fields.next();
        }
        let Some(path) = entry.get(3..) else {
            continue;
        };
        if !path.is_empty() {
            paths.push(path.to_string());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Commit every uncommitted change as one reviewer-attributed repair commit.
/// Returns `None` when the worktree was clean.
pub fn commit_reviewer_repairs(
    workspace_path: &Path,
    reviewer_model: &str,
    message: &str,
) -> Result<Option<CommitIdentity>, OrbitError> {
    stage_everything(workspace_path)?;
    if staged_paths(workspace_path)?.is_empty() {
        return Ok(None);
    }
    commit_reviewer_repairs_in(workspace_path, reviewer_model, message)?;
    let head = revision(workspace_path, "HEAD")?;
    let mut commits = commits_between(workspace_path, &format!("{}^", head.commit), &head.commit)?;
    commits
        .pop()
        .map(Some)
        .ok_or_else(|| OrbitError::Execution("reviewer repair commit was not recorded".into()))
}

/// The repair commit a settlement already made for `attempt_id`, when HEAD
/// is exactly that commit.
///
/// A settlement interrupted after committing reviewer repairs leaves HEAD
/// one commit past the admitted candidate. That commit is the gate's own
/// only when its sole parent is the admitted candidate, its message carries
/// the attempt's [`REVIEW_ATTEMPT_TRAILER`], and Git recorded the reviewer's
/// author and Orbit's committer. Anything else is someone else's change and
/// yields `None`.
pub fn review_repair_at_head(
    workspace_path: &Path,
    candidate_commit: &str,
    attempt_id: &str,
    reviewer_model: &str,
) -> Result<Option<CommitIdentity>, OrbitError> {
    let head = revision(workspace_path, "HEAD")?;
    let lineage = git_output(
        workspace_path,
        &["rev-list", "--parents", "-n", "1", &head.commit],
    )?;
    let parents = lineage.split_whitespace().skip(1).collect::<Vec<_>>();
    if parents != [candidate_commit] {
        return Ok(None);
    }
    let message = git_output_raw(
        workspace_path,
        &["log", "-1", "--format=%B", "--end-of-options", &head.commit],
    )?;
    let trailer = format!("{REVIEW_ATTEMPT_TRAILER}: {attempt_id}");
    if !message.lines().any(|line| line.trim() == trailer) {
        return Ok(None);
    }
    let Some(commit) = commits_between(workspace_path, candidate_commit, &head.commit)?.pop()
    else {
        return Ok(None);
    };
    let (author, committer) = reviewer_repair_identity(reviewer_model);
    let owned = commit.author == author && commit.committer == committer;
    Ok(owned.then_some(commit))
}

/// Paths `commit` changed against its first parent, reported the way
/// [`uncommitted_paths`] reports them before the commit: a rename or copy
/// by its destination only.
pub fn committed_paths(workspace_path: &Path, commit: &str) -> Result<Vec<String>, OrbitError> {
    let raw = git_output_raw(
        workspace_path,
        &[
            "diff-tree",
            "-r",
            "-M",
            "--no-commit-id",
            "--name-status",
            "-z",
            "--end-of-options",
            commit,
        ],
    )?;
    let mut paths = Vec::new();
    let mut fields = raw.split('\0').filter(|field| !field.is_empty());
    while let Some(status) = fields.next() {
        // A rename or copy names its source before the destination.
        if status.starts_with(['R', 'C']) {
            let _ = fields.next();
        }
        if let Some(path) = fields.next() {
            paths.push(path.to_string());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// What a managed landing looks like against the reviewed candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandedFacts {
    /// The integration branch immediately before the landing span.
    pub base_at_landing: SourceRevision,
    pub is_candidate_commit: bool,
    pub parents: usize,
    pub span_commits: usize,
}

/// Whether `ancestor` is `descendant` or in its history. An object the
/// checkout does not hold is not contained.
pub fn contains_commit(
    workspace_path: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, OrbitError> {
    git_command_success(
        workspace_path,
        &[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            ancestor,
            descendant,
        ],
    )
}

/// Publish a held candidate `commit` on `origin` as
/// `orbit-evidence/<branch>`, where `<branch>` is the worktree's current
/// branch, and return that ref. The candidate otherwise exists only in this
/// worktree, and another machine fetches it from `origin` to run a named check
/// at it. The ref is apart from the delivery branch, so a later delivery of
/// the task pushes its own history unhindered, and the task's next hold
/// replaces it.
pub fn publish_held_candidate(workspace_path: &Path, commit: &str) -> Result<String, OrbitError> {
    let branch = git_output(
        workspace_path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )?
    .trim()
    .to_string();
    if branch.is_empty() || branch.starts_with('-') {
        return Err(OrbitError::Execution(
            "the worktree is not on a named branch".to_string(),
        ));
    }
    let target = format!("refs/heads/orbit-evidence/{branch}");
    git_success(
        workspace_path,
        &["push", "--quiet", "origin", &format!("+{commit}:{target}")],
    )?;
    Ok(target)
}

/// [ORB-14450] The stable patch id of the whole change from `base` to
/// `head`, taken as one diff so a squash or a rebase of the same change
/// yields the same id; `None` when the range changes nothing.
pub fn patch_id(
    workspace_path: &Path,
    base: &str,
    head: &str,
) -> Result<Option<String>, OrbitError> {
    let run = |args: &[&str], stdin: Option<&[u8]>| {
        let outcome = git_run_bytes(workspace_path, args, stdin)?;
        if outcome.timed_out {
            return Err(git_timeout_error(
                workspace_path,
                args,
                outcome.timeout_ms,
                &outcome.stderr,
            ));
        }
        if !outcome.success {
            return Err(git_failure_error(workspace_path, args, &outcome.stderr));
        }
        Ok(outcome.stdout)
    };
    let diff = run(
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--full-index",
            "--binary",
            "--end-of-options",
            base,
            head,
        ],
        None,
    )?;
    if diff.is_empty() {
        return Ok(None);
    }
    let id = run(&["patch-id", "--stable"], Some(&diff))?;
    Ok(String::from_utf8_lossy(&id)
        .split_whitespace()
        .next()
        .map(ToOwned::to_owned))
}

/// Fetch the landed commit from `origin` so it can be read locally. A
/// failure is reported, never masked: an unreadable landing is uncovered.
pub fn fetch_landed_commit(workspace_path: &Path, landed_commit: &str) -> Result<(), OrbitError> {
    if revision(workspace_path, landed_commit).is_ok() {
        return Ok(());
    }
    git_success(
        workspace_path,
        &[
            "fetch",
            "--quiet",
            "--end-of-options",
            "origin",
            landed_commit,
        ],
    )
}

/// Read the landing span for a certificate with `candidate_commit_count`
/// commits: a fast-forward keeps the candidate commit, a merge commit has
/// two parents, replayed commits sit on the reviewed base tree, and anything
/// else is one squash commit on its first parent.
pub fn landed_candidate_facts(
    workspace_path: &Path,
    landed: &SourceRevision,
    candidate_commit: &str,
    candidate_base_tree: &str,
    candidate_commit_count: usize,
) -> Result<LandedFacts, OrbitError> {
    let parents = git_output(
        workspace_path,
        &["rev-list", "--parents", "-n", "1", &landed.commit],
    )?
    .split_whitespace()
    .count()
    .saturating_sub(1);
    let is_candidate_commit = landed.commit == candidate_commit;
    let replayed = candidate_commit_count.max(1);
    let base_after_span = format!("{}~{replayed}", landed.commit);
    let span_base = revision(workspace_path, &base_after_span).ok();
    let replayed_on_reviewed_base = (is_candidate_commit || (parents <= 1 && replayed > 1))
        && span_base
            .as_ref()
            .is_some_and(|base| base.tree == candidate_base_tree);
    let (base_at_landing, span_commits) = if replayed_on_reviewed_base {
        (
            span_base.ok_or_else(|| {
                OrbitError::Execution("landing span base vanished while reading it".into())
            })?,
            replayed,
        )
    } else {
        (
            revision(workspace_path, &format!("{}^1", landed.commit))?,
            1,
        )
    };
    Ok(LandedFacts {
        base_at_landing,
        is_candidate_commit,
        parents,
        span_commits,
    })
}

/// One required validation command's result, run exactly as the delivery
/// validation steps run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredValidationRun {
    pub command: String,
    pub passed: bool,
    pub exit_code: i32,
    pub timed_out: bool,
    /// `environment` when a tool was missing, `candidate` for any other
    /// failure, `None` when the command passed.
    pub failure_kind: Option<String>,
    pub output: String,
    /// How the validation environment was resolved.
    pub environment: serde_json::Value,
}

/// Run one required validation command in `workspace_path` with the shared
/// validation runner: the same shell, environment, timeout and failure
/// classification as delivery validation.
pub fn run_required_validation<H: crate::context::RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    command: &str,
) -> Result<RequiredValidationRun, OrbitError> {
    let run = super::required_command::run_required_command(host, workspace_path, command)?;
    Ok(RequiredValidationRun {
        failure_kind: run.failure_kind().as_str().map(str::to_string),
        environment: run.environment_record(),
        command: run.command,
        passed: run.passed,
        exit_code: run.exit_code,
        timed_out: run.timed_out,
        output: run.output,
    })
}
