use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::context::{RuntimeHost, TaskAutomationUpdate};
use crate::executor::automation::input::input_string_field;

use super::super::git::{
    GitOutcome, GitTimeoutBudget, GitTimeoutBudgetGuard, base_sync_mode_from_input,
    git_command_success, git_failure_error, git_output, git_run, git_success, git_timeout_error,
    resolve_worktree_start_point,
};
use super::cleanup::remove_worktree;
use super::dependency_delivery::{
    DependencyDeliveryMode, dependency_delivery_mode_from_input,
    ensure_dependencies_delivered_into_base,
};
use super::merge::{checkout_holding_branch, ensure_clean_checkout};
use super::{WorktreeIdentity, is_registered_worktree};

const DEFAULT_BASE: &str = "main";

/// Create a worktree and branch for a single task or task bundle, stamp
/// `job_run_id` and `workspace_path` on every task in scope, and move them to
/// `in_progress`.
///
/// Generic automation — not tied to any specific workflow. Any
/// pipeline can reuse this by passing a `branch_prefix`.
pub(in crate::executor::automation) fn setup_worktree<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    // The worktree's identity is derived once, here and in gc, from the same
    // rule (ORB-10427) — never re-spelled per call site.
    let identity = WorktreeIdentity::from_input(input, None)?;
    let task_ids = &identity.task_ids;
    // `identity.run_id` names the stable checkout (epic pipelines pin
    // `epic-<task-id>`). Execution authority stays on the admitted job when
    // the dispatcher supplied `job_run_id`.
    let worktree_run_id = &identity.run_id;
    let job_run_id =
        input_string_field(input, "job_run_id").unwrap_or_else(|| worktree_run_id.clone());
    let base = input_string_field(input, "base")
        .or_else(|| input_string_field(input, "base_branch"))
        .unwrap_or_else(|| DEFAULT_BASE.to_string());
    let base_sync_mode = base_sync_mode_from_input(input)?;
    let dependency_delivery_mode = dependency_delivery_mode_from_input(input)?;
    let landing_mode = match (
        input_string_field(input, "landing_mode"),
        input_string_field(input, "ship_mode"),
    ) {
        (Some(mode), _) => Some(("landing_mode", mode)),
        (None, Some(mode)) => Some(("ship_mode", mode)),
        (None, None) => None,
    };

    let _timeout_budget = GitTimeoutBudgetGuard::install(GitTimeoutBudget::from_input(input)?);
    let repo_root_str = host.repo_root()?;
    let repo_root = Path::new(&repo_root_str);

    let start_point = resolve_worktree_start_point(repo_root, &base, base_sync_mode)?;
    // ORB-10380: `start_point` is a moving name (`origin/<base>`) shared by every
    // worktree off this `.git`; a sibling run's fetch or a merge can move it
    // mid-run. Resolve it exactly once here and hand the resulting commit id to
    // both the worktree creation and every downstream step that needs the base.
    // ORB-11639: this commit is also the only HEAD a reused branch or checkout
    // may already occupy; setup never republishes it against unexplained history.
    let base_sha = git_output(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{start_point}^{{commit}}"),
        ],
    )?;

    // ORB-10464 (F2026-07-038): a dependency being `done` says nothing about
    // whether its commits reached this base. Verify that before anything
    // exists on disk, so a refusal leaves no worktree, no branch, and every
    // task still in its pre-run status.
    if dependency_delivery_mode == DependencyDeliveryMode::Enforce {
        ensure_dependencies_delivered_into_base(
            host,
            repo_root,
            task_ids,
            &start_point,
            &base_sha,
        )?;
    }

    // ORB-11373: when delivering into a local checkout, verify that the landing
    // base checkout is clean before creating the worktree or admitting tasks.
    // An initialized dirty base (e.g. from `workspace init`) will deterministically
    // fail the final merge step, so catch it early before expensive agent runs.
    if let Some((field, mode)) = landing_mode {
        match mode.as_str() {
            "local" => {
                let base_checkout = checkout_holding_branch(repo_root, &base)?
                    .unwrap_or_else(|| repo_root.to_path_buf());
                ensure_clean_checkout(&base_checkout, "base branch checkout")?;
            }
            "pr" => {}
            other => {
                return Err(OrbitError::InvalidInput(format!(
                    "input.{field} must be 'local' or 'pr', got '{other}'"
                )));
            }
        }
    }

    let branch_name = branch_name_for_tasks(&identity.branch_prefix, task_ids);

    let worktree_path = identity.path(repo_root)?;

    // ORB-11639: `ensure_worktree` refuses an existing branch or registered
    // checkout whose HEAD is not `base_sha`, so admission never sees a
    // checkpoint that later commit provenance would reject.
    let branch_name = ensure_worktree(repo_root, &worktree_path, &base_sha, &branch_name)?;

    // ORB-10602: mount-anchor materialization deliberately does *not* happen
    // here any more. Setup only ever saw a snapshot of the task's context files
    // and the un-absolutized policy profile, so the grant set it computed could
    // not match the one the sandbox would enforce, and could not grow with the
    // run. Anchors are now derived from the effective profile at each spawn —
    // see `activity_job::cli_runner::spawn::spawn_linux_bwrap`.

    let workspace_path_str = worktree_path.to_string_lossy().to_string();

    for task_id in task_ids {
        host.admit_task_for_workflow(task_id, "worktree_setup")?;
        host.apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                job_run_id: Some(job_run_id.clone()),
                ..TaskAutomationUpdate::default()
            },
        )?;
    }

    Ok(worktree_setup_output(
        &job_run_id,
        workspace_path_str,
        branch_name,
        start_point,
        base_sha,
    ))
}

// pub(crate) widened for tests/ layout migration (ORB-00240); test reaches via
// exposed surface per docs/design-patterns/test_layout.md. (Logged via
// orbit.task.update model=grok on ORB-00240 before this edit for the visibility
// change on internal test helpers.)
pub(crate) fn worktree_setup_output(
    run_id: &str,
    workspace_path: String,
    head_ref: String,
    base_ref: String,
    base_sha: String,
) -> Value {
    json!({
        "job_run_id": run_id,
        "batch_id": run_id,
        "workspace_path": workspace_path,
        "head_ref": head_ref,
        // The moving name, for steps that legitimately reconcile against the
        // live base (`sync_base`, `pr_open`).
        "base_ref": base_ref,
        // ORB-10380 / ORB-11639: the immutable commit this worktree was created
        // at, and the actual HEAD of a successful setup. Steps that must reason
        // about the history this run authored consume this; a reused checkout
        // is admitted only when its HEAD already equals this id.
        "base_sha": base_sha,
    })
}

// pub(crate) widened for tests/ layout migration (ORB-00240); test reaches via
// exposed surface per docs/design-patterns/test_layout.md. (Logged via
// orbit.task.update model=grok on ORB-00240 before this edit for the visibility
// change on internal test helpers.)
pub(crate) fn ensure_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    start_point: &str,
    branch_name: &str,
) -> Result<String, OrbitError> {
    let target = git_output(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{start_point}^{{commit}}"),
        ],
    )?;

    if worktree_path.exists() {
        // Only a worktree this repository registered is ours to reuse or
        // remove. "Is some git work tree" would also match an unrelated
        // checkout that happens to sit at the resolved path.
        if is_registered_worktree(repo_root, worktree_path)? {
            match inspect_registered_worktree(repo_root, worktree_path, &target, branch_name)? {
                RegisteredWorktree::Usable { branch, head } => {
                    refuse_stale_branch(&branch, &head, &target, worktree_path)?;
                    return Ok(branch);
                }
                RegisteredWorktree::Incomplete {
                    retained_work,
                    evidence,
                } => {
                    if retained_work {
                        return Err(OrbitError::Execution(format!(
                            "worktree path '{}' is an incomplete checkout of this repository and retains work; refusing to admit or reset it. {evidence}",
                            worktree_path.display()
                        )));
                    }
                    remove_worktree(repo_root, worktree_path, None, true)?;
                }
            }
        } else if is_empty_dir(worktree_path)? {
            std::fs::remove_dir(worktree_path).map_err(|error| {
                OrbitError::Execution(format!(
                    "failed to remove empty invalid worktree path '{}': {error}",
                    worktree_path.display()
                ))
            })?;
        } else {
            return Err(OrbitError::Execution(format!(
                "worktree path '{}' exists but is not a worktree of this repository; move it aside or remove it before retrying",
                worktree_path.display()
            )));
        }
    }

    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            OrbitError::Execution(format!(
                "failed to create worktree directory '{}': {error}",
                parent.display()
            ))
        })?;
    }

    git_success(repo_root, &["worktree", "prune"])?;
    add_worktree(repo_root, worktree_path, &target, branch_name)
}

enum RegisteredWorktree {
    Usable {
        branch: String,
        head: String,
    },
    Incomplete {
        retained_work: bool,
        evidence: String,
    },
}

/// Read-only completeness check. Deleted tracked files, dirty files, and extra
/// commits are retained work on a usable checkout — they do not prove the
/// checkout is incomplete and must not be restored or deleted here.
fn inspect_registered_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    start_point: &str,
    branch_name: &str,
) -> Result<RegisteredWorktree, OrbitError> {
    let inside = git_command_success(worktree_path, &["rev-parse", "--is-inside-work-tree"])?;
    let head_ok = git_command_success(worktree_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let git_dir = git_output(worktree_path, &["rev-parse", "--absolute-git-dir"]).ok();
    let index_ok = git_dir
        .as_ref()
        .map(|dir| Path::new(dir).join("index").is_file())
        .unwrap_or(false);
    let branch = git_output(
        worktree_path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .ok();
    let head = git_output(worktree_path, &["rev-parse", "--verify", "HEAD^{commit}"]).ok();
    let status = git_output(worktree_path, &["status", "--porcelain"]).ok();
    let unique_commits = branch_has_unique_commits(
        repo_root,
        branch.as_deref().unwrap_or(branch_name),
        start_point,
    )
    .unwrap_or(true);
    let evidence = format!(
        "HEAD={}, branch={}, gitdir={}, index={}, status={}, unique_commits={unique_commits}. Completeness does not treat deleted tracked files as an incomplete checkout (they may be intended edits). Timeout recovery is not conflict or failure-handoff recovery. Stale-branch provenance refuses a complete checkout whose HEAD is not the requested base without resetting it.",
        head.as_deref().unwrap_or("missing"),
        branch.as_deref().unwrap_or("missing"),
        git_dir.as_deref().unwrap_or("missing"),
        if index_ok { "present" } else { "missing" },
        status
            .as_deref()
            .map(|value| if value.trim().is_empty() {
                "clean"
            } else {
                "dirty"
            })
            .unwrap_or("unreadable")
    );

    if inside && head_ok && index_ok {
        let Some(branch) = branch else {
            return Ok(RegisteredWorktree::Incomplete {
                retained_work: true,
                evidence,
            });
        };
        let Some(head) = head else {
            return Ok(RegisteredWorktree::Incomplete {
                retained_work: true,
                evidence,
            });
        };
        return Ok(RegisteredWorktree::Usable { branch, head });
    }

    let retained_work = unique_commits
        || status
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
    Ok(RegisteredWorktree::Incomplete {
        retained_work,
        evidence,
    })
}

fn branch_has_unique_commits(
    repo_root: &Path,
    branch_name: &str,
    start_point: &str,
) -> Result<bool, OrbitError> {
    let branch_ref = format!("refs/heads/{branch_name}");
    if !git_command_success(repo_root, &["show-ref", "--verify", "--quiet", &branch_ref])? {
        return Ok(false);
    }
    let tip = git_output(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{branch_name}^{{commit}}"),
        ],
    )?;
    Ok(tip.trim() != start_point.trim())
}

fn add_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    target: &str,
    branch_name: &str,
) -> Result<String, OrbitError> {
    let worktree_path_arg = worktree_path.to_string_lossy().into_owned();
    let branch_ref = format!("refs/heads/{branch_name}");
    let args = if git_command_success(repo_root, &["show-ref", "--verify", "--quiet", &branch_ref])?
    {
        let tip = git_output(
            repo_root,
            &[
                "rev-parse",
                "--verify",
                &format!("{branch_name}^{{commit}}"),
            ],
        )?;
        refuse_stale_branch(branch_name, &tip, target, Path::new(&branch_ref))?;
        vec![
            "worktree".to_string(),
            "add".to_string(),
            worktree_path_arg,
            branch_name.to_string(),
        ]
    } else {
        vec![
            "worktree".to_string(),
            "add".to_string(),
            "-b".to_string(),
            branch_name.to_string(),
            worktree_path_arg,
            target.to_string(),
        ]
    };
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let outcome = git_run(repo_root, &arg_refs)?;
    if outcome.timed_out {
        return Err(recover_worktree_add_timeout(
            repo_root,
            worktree_path,
            branch_name,
            target,
            &outcome,
            &arg_refs,
        ));
    }
    if !outcome.success {
        return Err(git_failure_error(repo_root, &arg_refs, &outcome.stderr));
    }
    let head = git_output(worktree_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    refuse_stale_branch(branch_name, &head, target, worktree_path)?;
    Ok(branch_name.to_string())
}

/// ORB-11639: a reused branch or checkout may be admitted only when its actual
/// HEAD already equals the freshly resolved base. Setup never resets, cleans,
/// or republishes unexplained history as a new `base_sha`.
fn refuse_stale_branch(
    branch: &str,
    tip: &str,
    base: &str,
    location: &Path,
) -> Result<(), OrbitError> {
    if tip.trim() == base.trim() {
        return Ok(());
    }
    Err(OrbitError::Execution(format!(
        "worktree setup refusing stale branch '{branch}' at tip {tip}; requested base {base}. \
         Left '{}' untouched. Setup does not reset or discard a retained candidate, and does not \
         publish a new base_sha for unexplained history. Inspect the leftover, then move the \
         branch aside or delete it only after confirming no retained work is needed; retrying \
         setup without that recovery will refuse again.",
        location.display()
    )))
}

fn remove_owned_incomplete_worktree(
    repo_root: &Path,
    worktree_path: &Path,
) -> Result<(), OrbitError> {
    match remove_worktree(repo_root, worktree_path, None, true) {
        Ok(()) => Ok(()),
        Err(error) => {
            if worktree_path.exists() {
                std::fs::remove_dir_all(worktree_path).map_err(|fs_error| {
                    OrbitError::Execution(format!(
                        "{error}; also failed to delete '{}': {fs_error}",
                        worktree_path.display()
                    ))
                })?;
            }
            git_success(repo_root, &["worktree", "prune"])?;
            Ok(())
        }
    }
}

fn recover_worktree_add_timeout(
    repo_root: &Path,
    worktree_path: &Path,
    branch_name: &str,
    start_point: &str,
    outcome: &GitOutcome,
    args: &[&str],
) -> OrbitError {
    let timeout = git_timeout_error(repo_root, args, outcome.timeout_ms, &outcome.stderr);
    let registered = is_registered_worktree(repo_root, worktree_path).unwrap_or(false);
    if !registered && !worktree_path.exists() {
        return OrbitError::Execution(format!(
            "{timeout}; worktree add timed out before registration. This is timeout recovery, not conflict or failure-handoff recovery."
        ));
    }

    let inspection = if registered {
        inspect_registered_worktree(repo_root, worktree_path, start_point, branch_name).ok()
    } else {
        None
    };
    let can_remove = matches!(
        &inspection,
        Some(RegisteredWorktree::Incomplete {
            retained_work: false,
            ..
        })
    );

    if registered && can_remove {
        if let Err(error) = remove_owned_incomplete_worktree(repo_root, worktree_path) {
            return OrbitError::Execution(format!(
                "{timeout}; failed to remove incomplete owned worktree '{}': {error}. Leave it in place and inspect before retrying. This is timeout recovery, not conflict or failure-handoff recovery.",
                worktree_path.display()
            ));
        }
        return OrbitError::Execution(format!(
            "{timeout}; removed incomplete owned worktree '{}'. Retry must create a complete checkout; this is timeout recovery, not conflict or failure-handoff recovery.",
            worktree_path.display()
        ));
    }

    let evidence = match &inspection {
        Some(RegisteredWorktree::Incomplete { evidence, .. }) => evidence.clone(),
        Some(RegisteredWorktree::Usable { branch, head }) => {
            format!("usable attached branch '{branch}' at {head}")
        }
        None => format!("registered={registered}, path={}", worktree_path.display()),
    };
    OrbitError::Execution(format!(
        "{timeout}; leaving checkout '{}' in place ({evidence}). Retry will not admit an incomplete checkout. This is timeout recovery, not conflict or failure-handoff recovery.",
        worktree_path.display()
    ))
}

fn is_empty_dir(path: &Path) -> Result<bool, OrbitError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to inspect worktree path '{}': {error}",
            path.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Ok(false);
    }

    let mut entries = std::fs::read_dir(path).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to read worktree path '{}': {error}",
            path.display()
        ))
    })?;
    Ok(entries.next().is_none())
}

fn branch_name_for_tasks(branch_prefix: &str, task_ids: &[String]) -> String {
    if task_ids.len() == 1 {
        let short_ts = format!(
            "{:08x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        );
        return format!("{branch_prefix}/{}-{short_ts}", task_ids[0]);
    }

    let mut sorted_ids = task_ids.to_vec();
    sorted_ids.sort();
    let digest = Sha256::digest(sorted_ids.join(","));
    let bundle_hash = format!("{digest:x}");
    format!("{branch_prefix}/bundle-{}", &bundle_hash[..8])
}
