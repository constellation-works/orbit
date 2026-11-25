//! Committing a recovery's working-tree repair on the host.
//!
//! The recovery sandbox mounts the worktree's Git metadata read-only, so a
//! recovery agent can repair files but never commit them. When the recovered
//! retry or final resume follows a committed candidate, Orbit commits the
//! repair as the new head before validation reruns. The repair passes the same
//! path checks owner footprint
//! widening applies; a repair that fails one is refused before the index is
//! touched.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::fs::selector::claim_new_path_is_safe;
use orbit_common::security::redaction::{redact_all, redact_all_bytes};

use super::super::git::{
    BINARY_PATCH_DIFF_ARGS, git_command_success, git_output, git_output_bytes,
    git_output_bytes_with_env, git_output_bytes_with_env_and_stdin, git_output_with_env,
};
use super::author::recovery_author;
use super::git_ops::{
    ensure_named_branch, ensure_no_unmerged_changes, git_commit_as, stage_paths,
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
    process_env: &orbit_common::Env,
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
    let paths = task_candidate_paths(process_env, &workspace_path, NewPathPolicy::Recovery)
        .map_err(uncommittable)?
        .into_iter()
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(RecoveryCommit::Clean);
    }
    if request.no_diff_route {
        return Err(RecoveryCommitRefusal::NoDiffRoute);
    }
    let head = git_output(
        process_env,
        &workspace_path,
        &["rev-parse", "--verify", "HEAD^{commit}"],
    )
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
    ensure_named_branch(process_env, &workspace_path).map_err(uncommittable)?;
    ensure_no_unmerged_changes(process_env, &workspace_path).map_err(uncommittable)?;

    stage_paths(process_env, &workspace_path, &paths).map_err(uncommittable)?;
    let staged = staged_changed_files(process_env, &workspace_path).map_err(uncommittable)?;
    if staged.is_empty() {
        return Ok(RecoveryCommit::Clean);
    }
    git_commit_as(
        process_env,
        &workspace_path,
        &message(request),
        &recovery_author(request.recovery_model.as_deref()),
    )
    .map_err(uncommittable)?;
    let commit_sha = git_output(
        process_env,
        &workspace_path,
        &["rev-parse", "--verify", "HEAD^{commit}"],
    )
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
/// `workspace_path` as a redacted binary-capable patch without mutating `.git/index`
/// or copying protected files into the patch or repository object store.
pub(in crate::executor::automation::vcs) fn capture_uncommitted_diff(
    process_env: &orbit_common::Env,
    workspace_path: &Path,
) -> Result<Option<Vec<u8>>, orbit_common::OrbitError> {
    let changed_paths = uncommitted_path_bytes(process_env, workspace_path)?;
    let safe_paths = changed_paths
        .iter()
        .filter(|path| is_safe_path_bytes(path))
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
    // Isolation is mandatory: without it `git add` writes ordinary-file blobs
    // into the shared object store before they are redacted.
    let (temp_objects, alternate_objects) =
        isolated_temp_object_store(process_env, workspace_path, dir.path())?;
    let env_storage = [
        ("GIT_INDEX_FILE", index_str),
        ("GIT_OBJECT_DIRECTORY", temp_objects),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternate_objects),
    ];
    let env = env_storage
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect::<Vec<_>>();

    let has_head = git_command_success(
        process_env,
        workspace_path,
        &["rev-parse", "--verify", "HEAD^{commit}"],
    )?;
    let mut head_entries = BTreeMap::new();
    let mut head_paths = BTreeSet::new();
    let mut head_gitlinks = BTreeSet::new();
    let mut head_protected = Vec::new();
    if has_head {
        git_output_with_env(process_env, workspace_path, &["read-tree", "HEAD"], &env)?;
        for StageEntry { entry, path, .. } in
            read_stage_entries(process_env, workspace_path, Some(&env))?
        {
            if !is_safe_path_bytes(&path) {
                head_protected.push(path.clone());
            }
            if entry.mode == b"160000" {
                head_gitlinks.insert(path.clone());
            }
            head_paths.insert(path.clone());
            head_entries.insert(path, entry);
        }
    }

    let mut live_cached_paths = BTreeSet::new();
    let mut live_staged: BTreeMap<Vec<u8>, Vec<(u8, IndexEntry)>> = BTreeMap::new();
    for StageEntry { entry, stage, path } in read_stage_entries(process_env, workspace_path, None)?
    {
        live_cached_paths.insert(path.clone());
        if is_safe_path_bytes(&path) {
            live_staged.entry(path).or_default().push((stage, entry));
        }
    }

    let stageable = stageable_safe_paths(
        workspace_path,
        &safe_paths,
        &head_paths,
        &head_gitlinks,
        &live_cached_paths,
    )?;

    // Exclude protected paths from both the worktree and HEAD: a file/directory
    // replacement can stage or delete descendants that were never safe pathspecs.
    // Removing HEAD-protected paths from the temporary base tree avoids passing
    // pathspecs to `git diff`, which has no NUL `--pathspec-from-file` mode.
    let base_ref = if head_protected.is_empty() {
        has_head.then(|| "HEAD".to_string())
    } else {
        git_rm_cached_nul(process_env, workspace_path, &head_protected, &env)?;
        Some(git_output_with_env(
            process_env,
            workspace_path,
            &["write-tree"],
            &env,
        )?)
    };

    let mut protected = changed_paths
        .into_iter()
        .filter(|path| !is_safe_path_bytes(path))
        .collect::<Vec<_>>();
    protected.extend(head_protected);
    protected.sort();
    protected.dedup();

    if !stageable.is_empty() {
        git_rm_cached_nul(
            process_env,
            workspace_path,
            &stageable.remove_from_index,
            &env,
        )?;

        // Only emit `:(exclude,literal)` for protected paths beneath a directory in
        // `normal_add`; an ignored top-level protected path (such as a force-added
        // `.env`) passed as an exclude pathspec makes non-force `git add` fail.
        let normal_excludes = protected
            .iter()
            .filter(|path| {
                stageable
                    .normal_add_dirs
                    .iter()
                    .any(|dir| path_is_under_dir(path, dir))
            })
            .collect::<Vec<_>>();
        git_add_nul(
            process_env,
            workspace_path,
            false,
            &stageable.normal_add,
            &normal_excludes,
            &env,
        )?;
        git_add_nul(
            process_env,
            workspace_path,
            true,
            &stageable.staged_addition_add,
            &[],
            &env,
        )?;
    }

    let worktree_entries = read_stage_entries(process_env, workspace_path, Some(&env))?
        .into_iter()
        .map(|StageEntry { entry, path, .. }| (path, entry))
        .collect::<BTreeMap<_, _>>();
    let mut staged_remove = Vec::new();
    let mut staged_update = Vec::new();
    let mut retained_notes = Vec::new();
    for path in &safe_paths {
        let head = head_entries.get(path.as_slice());
        let worktree = worktree_entries.get(path.as_slice());
        let Some(live) = live_staged.get(path.as_slice()) else {
            if head.is_some() {
                if worktree == head {
                    staged_remove.push(path.clone());
                } else if worktree.is_some() {
                    retained_notes.push(RetainedIndexNote {
                        path: path.clone(),
                        reason: "staged deletion superseded by worktree file",
                    });
                }
            }
            continue;
        };
        if live.len() != 1 || live[0].0 != 0 {
            let reason = if worktree == head || worktree.is_none() {
                "unmerged stages in retained index"
            } else {
                "unmerged stages in retained index superseded by worktree file"
            };
            retained_notes.push(RetainedIndexNote {
                path: path.clone(),
                reason,
            });
            continue;
        }
        let staged_entry = &live[0].1;
        if head == Some(staged_entry) || worktree == Some(staged_entry) {
            continue;
        }
        if worktree == head && !has_path_prefix_conflict(path, &worktree_entries) {
            staged_update.push((path.clone(), staged_entry));
        } else {
            let reason = if worktree == head {
                "staged file conflicts with worktree directory entries"
            } else if worktree.is_none() {
                "staged edits superseded by worktree deletion"
            } else {
                "staged content superseded by worktree changes"
            };
            retained_notes.push(RetainedIndexNote {
                path: path.clone(),
                reason,
            });
        }
    }
    git_rm_cached_nul(process_env, workspace_path, &staged_remove, &env)?;
    git_update_index_info_nul(process_env, workspace_path, &staged_update, &env)?;

    let mut diff_args = vec!["-c", "core.quotePath=true", "diff", "--cached"];
    diff_args.extend_from_slice(BINARY_PATCH_DIFF_ARGS);
    if let Some(base) = base_ref.as_deref() {
        diff_args.push(base);
    }
    let diff = git_output_bytes_with_env(process_env, workspace_path, &diff_args, &env)?;
    let has_diff = !diff.iter().all(u8::is_ascii_whitespace);
    if !has_diff && retained_notes.is_empty() {
        return Ok(None);
    }
    let mut output = if retained_notes.is_empty() {
        Vec::new()
    } else {
        format_retained_index_header(&retained_notes)
    };
    if has_diff {
        output.extend_from_slice(&diff);
    }
    Ok(Some(redact_all_bytes(&output)))
}

/// Point temporary index object writes at a scratch directory backed by the
/// repository's object store so captured diffs never add blobs to `.git/objects`.
/// Fails when the repository object directory cannot be isolated (for example a
/// path the alternates list cannot represent) so capture stops before staging.
fn isolated_temp_object_store(
    process_env: &orbit_common::Env,
    workspace_path: &Path,
    temp_root: &Path,
) -> Result<(String, String), orbit_common::OrbitError> {
    let raw_objects = git_output(
        process_env,
        workspace_path,
        &["rev-parse", "--git-path", "objects"],
    )?;
    let objects_path = Path::new(&raw_objects);
    let abs_objects = if objects_path.is_absolute() {
        objects_path.to_path_buf()
    } else {
        workspace_path.join(objects_path)
    };
    let isolation_failed = |reason: String| {
        orbit_common::OrbitError::Io(format!("cannot isolate scratch diff objects: {reason}"))
    };
    let canonical_objects = abs_objects
        .canonicalize()
        .map_err(|error| isolation_failed(format!("object directory unavailable ({error})")))?;
    let alternate = std::env::join_paths([&canonical_objects]).map_err(|error| {
        isolation_failed(format!("object directory path unsupported ({error})"))
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

fn uncommitted_path_bytes(
    process_env: &orbit_common::Env,
    workspace_path: &Path,
) -> Result<Vec<Vec<u8>>, orbit_common::OrbitError> {
    let status = git_output_bytes(
        process_env,
        workspace_path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut paths = Vec::new();
    let mut fields = status.split(|byte| *byte == 0);
    while let Some(entry) = fields.next() {
        if entry.is_empty() {
            continue;
        }
        let Some((&index_state, &worktree_state)) = entry.first().zip(entry.get(1)) else {
            continue;
        };
        // A rename or copy record carries its source as the next NUL field whichever
        // column reports it: an intent-to-add destination is reported as ` R` with
        // only the worktree column set.
        let renamed = matches!(index_state, b'R' | b'C') || matches!(worktree_state, b'R' | b'C');
        let source = if renamed {
            fields.next().filter(|source| !source.is_empty())
        } else {
            None
        };
        let Some(path) = entry.get(3..) else {
            continue;
        };
        if !path.is_empty() {
            paths.push(path.to_vec());
        }
        if let Some(source) = source {
            paths.push(source.to_vec());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn split_nul_bytes(stdout: &[u8]) -> Vec<Vec<u8>> {
    stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn is_safe_path_bytes(path: &[u8]) -> bool {
    // `claim_new_path_is_safe` matches ASCII segment and delimiter patterns;
    // lossy UTF-8 maps non-UTF-8 bytes to U+FFFD, never matching ASCII.
    !path.contains(&0) && claim_new_path_is_safe(&String::from_utf8_lossy(path))
}

fn append_pathspec(buf: &mut Vec<u8>, prefix: &[u8], path: &[u8]) {
    buf.extend_from_slice(prefix);
    buf.extend_from_slice(path);
    buf.push(0);
}

fn path_from_git_bytes(bytes: &[u8]) -> Result<Cow<'_, Path>, orbit_common::OrbitError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(Cow::Borrowed(Path::new(std::ffi::OsStr::from_bytes(bytes))))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(|s| Cow::Borrowed(Path::new(s)))
            .map_err(|_| {
                orbit_common::OrbitError::Execution(format!(
                    "uncommitted path `{}` is not valid UTF-8 and cannot be probed on this host",
                    bytes.escape_ascii()
                ))
            })
    }
}

#[derive(Clone, PartialEq, Eq)]
struct IndexEntry {
    mode: Vec<u8>,
    oid: Vec<u8>,
}

struct StageEntry {
    entry: IndexEntry,
    stage: u8,
    path: Vec<u8>,
}

struct RetainedIndexNote {
    path: Vec<u8>,
    reason: &'static str,
}

fn read_stage_entries(
    process_env: &orbit_common::Env,
    workspace_path: &Path,
    env: Option<&[(&str, &str)]>,
) -> Result<Vec<StageEntry>, orbit_common::OrbitError> {
    let args = ["ls-files", "--stage", "-z"];
    let stdout = match env {
        Some(env) => git_output_bytes_with_env(process_env, workspace_path, &args, env)?,
        None => git_output_bytes(process_env, workspace_path, &args)?,
    };
    let mut entries = Vec::new();
    for raw in split_nul_bytes(&stdout) {
        let Some(tab) = raw.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let header = &raw[..tab];
        let mut parts = header.split(|byte| *byte == b' ');
        let (Some(mode), Some(oid), Some(stage_bytes)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some(stage) = std::str::from_utf8(stage_bytes)
            .ok()
            .and_then(|value| value.parse::<u8>().ok())
        else {
            continue;
        };
        entries.push(StageEntry {
            entry: IndexEntry {
                mode: mode.to_vec(),
                oid: oid.to_vec(),
            },
            stage,
            path: raw[tab + 1..].to_vec(),
        });
    }
    Ok(entries)
}

fn has_path_prefix_conflict(path: &[u8], entries: &BTreeMap<Vec<u8>, IndexEntry>) -> bool {
    entries.keys().any(|existing| {
        existing.as_slice() != path
            && (path_is_under_dir(existing, path) || path_is_under_dir(path, existing))
    })
}

fn git_update_index_info_nul(
    process_env: &orbit_common::Env,
    workspace_path: &Path,
    updates: &[(Vec<u8>, &IndexEntry)],
    env: &[(&str, &str)],
) -> Result<(), orbit_common::OrbitError> {
    if updates.is_empty() {
        return Ok(());
    }
    let mut input = Vec::new();
    for (path, entry) in updates {
        input.extend_from_slice(&entry.mode);
        input.push(b' ');
        input.extend_from_slice(&entry.oid);
        input.push(b'\t');
        input.extend_from_slice(path);
        input.push(0);
    }
    git_output_bytes_with_env_and_stdin(
        process_env,
        workspace_path,
        &["update-index", "-z", "--index-info"],
        &input,
        env,
    )?;
    Ok(())
}

fn format_retained_index_header(notes: &[RetainedIndexNote]) -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(
        b"# Orbit retained-index note: the following safe paths have distinct staged state\n\
          # in the retained live index not represented by this worktree patch:\n",
    );
    for note in notes {
        header.extend_from_slice(b"# - ");
        for byte in note.path.escape_ascii() {
            header.push(byte);
        }
        header.extend_from_slice(b" (");
        header.extend_from_slice(note.reason.as_bytes());
        header.extend_from_slice(b")\n");
    }
    header
}

fn git_rm_cached_nul(
    process_env: &orbit_common::Env,
    workspace_path: &Path,
    paths: &[Vec<u8>],
    env: &[(&str, &str)],
) -> Result<(), orbit_common::OrbitError> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut rm_input = Vec::new();
    for path in paths {
        append_pathspec(&mut rm_input, b":(literal)", path);
    }
    git_output_bytes_with_env_and_stdin(
        process_env,
        workspace_path,
        &[
            "rm",
            "--cached",
            "-q",
            "--ignore-unmatch",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ],
        &rm_input,
        env,
    )?;
    Ok(())
}

fn git_add_nul(
    process_env: &orbit_common::Env,
    workspace_path: &Path,
    force: bool,
    paths: &[Vec<u8>],
    excludes: &[&Vec<u8>],
    env: &[(&str, &str)],
) -> Result<(), orbit_common::OrbitError> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut add_input = Vec::new();
    for path in paths {
        append_pathspec(&mut add_input, b":(literal)", path);
    }
    for path in excludes {
        append_pathspec(&mut add_input, b":(exclude,literal)", path);
    }
    let args: &[&str] = if force {
        &[
            "add",
            "-f",
            "-A",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ]
    } else {
        &["add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"]
    };
    git_output_bytes_with_env_and_stdin(process_env, workspace_path, args, &add_input, env)?;
    Ok(())
}

fn path_is_under_dir(path: &[u8], dir: &[u8]) -> bool {
    let dir = dir.strip_suffix(b"/").unwrap_or(dir);
    dir == b"."
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with(b"/"))
}

struct StageableSafePaths {
    remove_from_index: Vec<Vec<u8>>,
    normal_add: Vec<Vec<u8>>,
    normal_add_dirs: Vec<Vec<u8>>,
    staged_addition_add: Vec<Vec<u8>>,
}

impl StageableSafePaths {
    fn is_empty(&self) -> bool {
        self.remove_from_index.is_empty()
            && self.normal_add.is_empty()
            && self.staged_addition_add.is_empty()
    }
}

/// Partition `safe_paths` by how the temporary `HEAD` index can stage each
/// change without triggering `.gitignore` failures on staged live-index
/// additions or descending into directories that could hold ignored secrets.
fn stageable_safe_paths(
    workspace_path: &Path,
    safe_paths: &[Vec<u8>],
    head_paths: &BTreeSet<Vec<u8>>,
    head_gitlinks: &BTreeSet<Vec<u8>>,
    live_cached_paths: &BTreeSet<Vec<u8>>,
) -> Result<StageableSafePaths, orbit_common::OrbitError> {
    let mut stageable = StageableSafePaths {
        remove_from_index: Vec::new(),
        normal_add: Vec::new(),
        normal_add_dirs: Vec::new(),
        staged_addition_add: Vec::new(),
    };
    for path in safe_paths {
        let rel = path_from_git_bytes(path)?;
        match std::fs::symlink_metadata(workspace_path.join(rel.as_ref())) {
            Ok(meta) if meta.file_type().is_dir() => {
                if head_paths.contains(path.as_slice()) {
                    if head_gitlinks.contains(path.as_slice()) {
                        stageable.normal_add.push(path.clone());
                        stageable.normal_add_dirs.push(path.clone());
                    } else {
                        // A tracked file/symlink replaced by a directory: remove the
                        // old index entry without descending into ignored descendants.
                        stageable.remove_from_index.push(path.clone());
                    }
                } else if !live_cached_paths.contains(path.as_slice()) {
                    // Untracked non-ignored directory (e.g. a nested repository).
                    stageable.normal_add.push(path.clone());
                    stageable.normal_add_dirs.push(path.clone());
                }
            }
            Ok(_) => {
                if !head_paths.contains(path.as_slice())
                    && live_cached_paths.contains(path.as_slice())
                {
                    // Non-directory path already staged in the live index: safe to
                    // force-add into the temporary index even if `.gitignore` matches.
                    stageable.staged_addition_add.push(path.clone());
                } else {
                    stageable.normal_add.push(path.clone());
                }
            }
            Err(_) if head_paths.contains(path.as_slice()) => {
                stageable.remove_from_index.push(path.clone());
            }
            // A staged path whose parent directory was replaced on disk by a
            // regular file cannot exist there (ENOTDIR), so it is absent like
            // a deleted path; other lookup failures stay hard errors.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => {
                return Err(orbit_common::OrbitError::Io(format!(
                    "failed to inspect uncommitted path `{}`: {error}",
                    path.escape_ascii()
                )));
            }
        }
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
    let Some(diff) =
        capture_uncommitted_diff(&host.process_env(), workspace_path).map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let diff = redact_all_bytes(&diff);
    let update = crate::TaskAutomationUpdate {
        upsert_artifacts: vec![orbit_types::task::TaskArtifact::from_bytes(
            artifact_path,
            diff.clone(),
        )],
        ..crate::TaskAutomationUpdate::default()
    };
    match host.apply_task_automation_update(task_id, update) {
        Ok(()) => Ok(Some(format!("task artifact `{artifact_path}`"))),
        Err(task_error) => {
            let task_error = redact_all(&task_error.to_string());
            orbit_common::fs::io::write_private_bytes_beneath(
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
