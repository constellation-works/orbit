//! Committing a recovery's working-tree repair on the host.
//!
//! The recovery sandbox mounts the worktree's Git metadata read-only, so a
//! recovery agent can repair files but never commit them. When the recovered
//! retry or final resume follows a committed candidate, Orbit commits the
//! repair as the new head before validation reruns. The repair passes the same
//! path checks owner footprint
//! widening applies; a repair that fails one is refused before the index is
//! touched.

use std::path::Path;

use orbit_common::fs::selector::claim_new_path_is_safe;
use orbit_common::security::redaction::redact_all;

use super::super::git::{
    git_command_success, git_output, git_output_paths_with_env, git_output_raw_with_env,
    git_output_with_env,
};
use super::super::review_gate::uncommitted_paths;
use super::author::recovery_author;
use super::git_ops::{
    ensure_named_branch, ensure_no_unmerged_changes, git_commit_as, literal_pathspec, stage_paths,
    staged_changed_files,
};
use super::message::recovery_commit_message;
use super::scope::{NewPathPolicy, irregular_candidate_paths, task_candidate_paths};

/// Prefix of a step failure whose recovery repair Orbit refused to commit.
const RECOVERY_COMMIT_REFUSED: &str = "recovery_commit_refused";

/// The recovered step and what the run already decided about its candidate.
pub(crate) struct RecoveryCommitRequest<'a> {
    pub(crate) workspace_path: &'a Path,
    pub(crate) run_id: &'a str,
    pub(crate) failed_step_id: &'a str,
    pub(crate) recovery_activity: &'a str,
    /// Exact paths explained by an admitted CI repair, if applicable.
    pub(crate) repair_paths: Option<&'a [String]>,
    pub(crate) task_ids: &'a [String],
    /// The commit type of the tasks the repair belongs to.
    pub(crate) commit_type: orbit_types::workflow::CommitType,
    /// The recovery activity's resolved crew model, which authors the repair
    /// commit [ORB-16768]. `None` keeps the historical role identity.
    pub(crate) recovery_model: Option<String>,
    /// The commit step skipped a clean, no-diff-expected candidate: this run
    /// has no route that delivers a change.
    pub(crate) no_diff_route: bool,
    /// Heads a before-PR review attempt was admitted on or settled.
    pub(crate) reviewed_heads: &'a [String],
}

/// What the host did with the repair.
#[derive(Debug)]
pub(crate) enum RecoveryCommit {
    /// Recovery left nothing to commit.
    Clean,
    /// The repair is the new candidate head.
    Committed {
        commit_sha: String,
        paths: Vec<String>,
    },
}

/// Why a repair was not committed. Every refusal but `Uncommittable` is
/// decided before anything is staged.
#[derive(Debug)]
pub(crate) enum RecoveryCommitRefusal {
    /// Git or `.orbit` metadata, an environment file, or a malformed path.
    ProtectedPath(Vec<String>),
    /// A symlink or gitlink: footprint widening accepts only regular files.
    OutsideFootprint(Vec<String>),
    /// CI repair changed a path absent from its evidence record.
    UnexplainedPaths(Vec<String>),
    /// The run committed to delivering no change.
    NoDiffRoute,
    /// The candidate is the head a before-PR review was admitted on or
    /// settled; a repair commit would bypass that review.
    ReviewedCandidate(String),
    /// The worktree could not be inspected or committed.
    Uncommittable(String),
}

impl RecoveryCommitRefusal {
    /// The stable reason code.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::ProtectedPath(_) => "protected_path",
            Self::OutsideFootprint(_) => "outside_footprint",
            Self::UnexplainedPaths(_) => "unexplained_paths",
            Self::NoDiffRoute => "no_diff_route",
            Self::ReviewedCandidate(_) => "reviewed_candidate",
            Self::Uncommittable(_) => "uncommittable",
        }
    }
}

impl std::fmt::Display for RecoveryCommitRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{RECOVERY_COMMIT_REFUSED}:{}: ", self.code())?;
        match self {
            Self::ProtectedPath(paths) => write!(
                f,
                "the repair changes protected paths {paths:?}; a recovery may not change Git or \
                 `.orbit` metadata or environment files"
            ),
            Self::OutsideFootprint(paths) => write!(
                f,
                "the repair leaves non-regular files {paths:?}; the task footprint widens only \
                 over regular files, never a symlink or gitlink"
            ),
            Self::UnexplainedPaths(paths) => {
                write!(f, "the CI repair has unexplained paths {paths:?}")
            }
            Self::NoDiffRoute => f.write_str(
                "the commit step skipped this no-diff-expected candidate, so the run has no \
                 route that delivers the repair",
            ),
            Self::ReviewedCandidate(head) => write!(
                f,
                "candidate {head} is under before-PR review; a repair commit would bypass \
                 that review"
            ),
            Self::Uncommittable(detail) => f.write_str(detail),
        }?;
        if matches!(self, Self::Uncommittable(_)) {
            f.write_str(". Orbit stopped recovery; inspect the retained worktree and HEAD")
        } else {
            f.write_str(". Orbit committed nothing and left the repair in the worktree")
        }
    }
}

/// Commit what recovery left in the worktree as a new candidate head with
/// recovery provenance, or refuse it before any Git mutation.
pub(crate) fn commit_recovery_repair(
    request: &RecoveryCommitRequest<'_>,
) -> Result<RecoveryCommit, RecoveryCommitRefusal> {
    let uncommittable =
        |error: orbit_common::OrbitError| RecoveryCommitRefusal::Uncommittable(error.to_string());
    let workspace_path = request.workspace_path.canonicalize().map_err(|error| {
        RecoveryCommitRefusal::Uncommittable(format!(
            "workspace '{}' is not readable: {error}",
            request.workspace_path.display()
        ))
    })?;
    let paths = task_candidate_paths(&workspace_path, NewPathPolicy::Recovery)
        .map_err(uncommittable)?
        .into_iter()
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(RecoveryCommit::Clean);
    }
    if request.no_diff_route {
        return Err(RecoveryCommitRefusal::NoDiffRoute);
    }
    let head = git_output(&workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])
        .map_err(uncommittable)?;
    if request.reviewed_heads.contains(&head) {
        return Err(RecoveryCommitRefusal::ReviewedCandidate(head));
    }
    let protected = paths
        .iter()
        .filter(|path| !claim_new_path_is_safe(path))
        .cloned()
        .collect::<Vec<_>>();
    if !protected.is_empty() {
        return Err(RecoveryCommitRefusal::ProtectedPath(protected));
    }
    let irregular = irregular_candidate_paths(&workspace_path, &paths);
    if !irregular.is_empty() {
        return Err(RecoveryCommitRefusal::OutsideFootprint(irregular));
    }
    if let Some(explained) = request.repair_paths {
        let unexplained = paths
            .iter()
            .filter(|path| !explained.contains(path))
            .cloned()
            .collect::<Vec<_>>();
        if !unexplained.is_empty() || explained.iter().any(|path| !paths.contains(path)) {
            return Err(RecoveryCommitRefusal::UnexplainedPaths(paths));
        }
    }
    ensure_named_branch(&workspace_path).map_err(uncommittable)?;
    ensure_no_unmerged_changes(&workspace_path).map_err(uncommittable)?;

    stage_paths(&workspace_path, &paths).map_err(uncommittable)?;
    let staged = staged_changed_files(&workspace_path).map_err(uncommittable)?;
    if staged.is_empty() {
        return Ok(RecoveryCommit::Clean);
    }
    git_commit_as(
        &workspace_path,
        &message(request),
        &recovery_author(request.recovery_model.as_deref()),
    )
    .map_err(uncommittable)?;
    let commit_sha = git_output(&workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])
        .map_err(uncommittable)?;
    Ok(RecoveryCommit::Committed {
        commit_sha,
        // The pre-stage inventory has rename detection disabled, retaining
        // both endpoints for scope evidence and task-context reconciliation.
        paths,
    })
}

fn message(request: &RecoveryCommitRequest<'_>) -> String {
    recovery_commit_message(
        request.commit_type,
        request.task_ids,
        request.run_id,
        request.failed_step_id,
        request.recovery_activity,
    )
}

/// Capture uncommitted tracked, staged, and untracked changes on safe paths in
/// `workspace_path` as a redacted unified patch without mutating `.git/index`
/// or copying protected files into the patch or repository object store.
pub(in crate::executor::automation::vcs) fn capture_uncommitted_diff(
    workspace_path: &Path,
) -> Result<Option<String>, orbit_common::OrbitError> {
    let changed_paths = uncommitted_paths(workspace_path)?;
    let safe_paths = changed_paths
        .iter()
        .filter(|path| claim_new_path_is_safe(path))
        .cloned()
        .collect::<Vec<_>>();
    if safe_paths.is_empty() {
        return Ok(None);
    }

    let dir = tempfile::tempdir().map_err(|error| {
        orbit_common::OrbitError::Io(format!("failed to create scratch diff index dir: {error}"))
    })?;
    let index_path = dir.path().join("index");
    let index_str = index_path.to_string_lossy().into_owned();
    let (temp_objects, alternate_objects) = isolated_temp_object_store(workspace_path, dir.path())?;
    let env_storage = [
        ("GIT_INDEX_FILE", index_str),
        ("GIT_OBJECT_DIRECTORY", temp_objects),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternate_objects),
    ];
    let env = env_storage
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect::<Vec<_>>();

    let has_head =
        git_command_success(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if has_head {
        git_output_with_env(workspace_path, &["read-tree", "HEAD"], &env)?;
    }

    // A literal pathspec also matches descendants. Exclude protected names
    // from both the worktree and HEAD: a file/directory replacement can stage
    // or delete descendants that were never themselves safe pathspecs.
    let mut protected = changed_paths
        .into_iter()
        .filter(|path| !claim_new_path_is_safe(path))
        .collect::<Vec<_>>();
    if has_head {
        protected.extend(
            git_output_paths_with_env(workspace_path, &["ls-files", "--cached", "-z"], &env)?
                .into_iter()
                .filter(|path| !claim_new_path_is_safe(path)),
        );
    }
    protected.sort();
    protected.dedup();
    let exclusions = protected
        .iter()
        .map(|path| format!(":(exclude,literal){path}"))
        .collect::<Vec<_>>();

    let stageable = stageable_safe_paths(workspace_path, &safe_paths, has_head, &env)?;
    if stageable.is_empty() {
        return Ok(None);
    }

    let mut add_args = vec!["add".to_string(), "-A".to_string(), "--".to_string()];
    add_args.extend(stageable.iter().map(|path| literal_pathspec(path)));
    add_args.extend(exclusions.iter().cloned());
    let add_arg_refs = add_args.iter().map(String::as_str).collect::<Vec<_>>();
    git_output_with_env(workspace_path, &add_arg_refs, &env)?;

    let mut diff_args = vec!["diff", "--cached", "--no-renames"];
    if has_head {
        diff_args.push("HEAD");
    }
    diff_args.push("--");
    diff_args.extend(exclusions.iter().map(String::as_str));
    let diff = git_output_raw_with_env(workspace_path, &diff_args, &env)?;
    if diff.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(redact_all(&diff)))
    }
}

/// Point temporary index object writes at a scratch directory backed by the
/// repository's object store so captured diffs never add blobs to `.git/objects`.
/// Fails when the store cannot be isolated; the caller captures nothing then.
fn isolated_temp_object_store(
    workspace_path: &Path,
    temp_root: &Path,
) -> Result<(String, String), orbit_common::OrbitError> {
    let raw_objects = git_output(workspace_path, &["rev-parse", "--git-path", "objects"])?;
    let objects_path = Path::new(&raw_objects);
    let abs_objects = if objects_path.is_absolute() {
        objects_path.to_path_buf()
    } else {
        workspace_path.join(objects_path)
    };
    // Isolation is mandatory: without it `git add` writes the original bytes of
    // every ordinary file into the shared object store before redaction runs
    // [STD-05 §R13]. Refuse before anything is staged.
    let canonical_objects = abs_objects.canonicalize().map_err(|error| {
        orbit_common::OrbitError::Io(format!(
            "cannot isolate scratch diff objects: object directory '{}' is not readable: {error}",
            abs_objects.display()
        ))
    })?;
    let alternate = std::env::join_paths([&canonical_objects]).map_err(|error| {
        orbit_common::OrbitError::Io(format!(
            "cannot isolate scratch diff objects: object directory '{}' cannot be listed as an \
             alternate: {error}",
            canonical_objects.display()
        ))
    })?;
    let temp_objects = temp_root.join("objects");
    orbit_common::fs::io::create_private_dir_all(&temp_objects).map_err(|error| {
        orbit_common::OrbitError::Io(format!(
            "failed to create scratch diff objects dir: {error}"
        ))
    })?;
    Ok((
        temp_objects.to_string_lossy().into_owned(),
        alternate.to_string_lossy().into_owned(),
    ))
}

/// Filter `safe_paths` to paths that exist in the worktree or (for deletions)
/// in the temporary `HEAD` index so `git add -A` never receives a missing pathspec.
fn stageable_safe_paths(
    workspace_path: &Path,
    safe_paths: &[String],
    has_head: bool,
    env: &[(&str, &str)],
) -> Result<Vec<String>, orbit_common::OrbitError> {
    let mut stageable = Vec::new();
    let mut missing_on_disk = Vec::new();
    for path in safe_paths {
        if std::fs::symlink_metadata(workspace_path.join(path)).is_ok() {
            stageable.push(path.clone());
        } else if has_head {
            missing_on_disk.push(literal_pathspec(path));
        }
    }
    if !missing_on_disk.is_empty() {
        let mut list_args = vec!["ls-files", "--cached", "-z", "--"];
        list_args.extend(missing_on_disk.iter().map(String::as_str));
        stageable.extend(git_output_paths_with_env(workspace_path, &list_args, env)?);
    }
    Ok(stageable)
}

/// Capture any uncommitted diff in `workspace_path` and persist it as a redacted
/// task artifact (falling back to `.orbit/tmp/` if the task record rejects writes).
pub(crate) fn persist_uncommitted_diff_artifact<H: crate::RuntimeHost + ?Sized>(
    host: &H,
    task_id: &str,
    workspace_path: &Path,
    artifact_path: &str,
) -> Result<Option<String>, String> {
    let Some(diff) = capture_uncommitted_diff(workspace_path).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let diff = redact_all(&diff);
    let update = crate::TaskAutomationUpdate {
        upsert_artifacts: vec![orbit_types::task::TaskArtifact::from_text(
            artifact_path,
            &diff,
        )],
        ..crate::TaskAutomationUpdate::default()
    };
    match host.apply_task_automation_update(task_id, update) {
        Ok(()) => Ok(Some(format!("task artifact `{artifact_path}`"))),
        Err(task_error) => {
            let task_error = redact_all(&task_error.to_string());
            orbit_common::fs::io::write_private_text_beneath(
                workspace_path,
                Path::new(".orbit/tmp"),
                artifact_path,
                &diff,
            )
            .map_err(|io_err| {
                format!(
                    "task artifact write failed ({task_error}) and scratch write failed ({io_err})"
                )
            })?;
            Ok(Some(format!(
                "`.orbit/tmp/{artifact_path}` (task artifact write failed: {task_error})"
            )))
        }
    }
}
