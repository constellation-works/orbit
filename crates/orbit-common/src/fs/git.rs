use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::OrbitError;
use crate::process::run_bounded_capped;

use super::io::with_exclusive_file_lock;

/// Lock file inside the git common directory that serializes Orbit-owned
/// updates of remote-tracking refs.
///
/// ORB-11269/ORB-11256 locked only task-pilot (`orbit-task-pilot-fetch`).
/// Delivery `fetch_remote_base` was a second writer of the same
/// `refs/remotes/origin/*` refs, so a linked worktree fetch and
/// `prepare_task_pilot` could CAS-fail each other. One common-dir lock
/// covers every Orbit fetch; do not add a second pilot-only lock.
pub const GIT_FETCH_LOCK_NAME: &str = "orbit-git-fetch";

/// Bounded attempts for a single Orbit-owned fetch, including the first try.
pub const GIT_FETCH_CAS_ATTEMPTS: u32 = 3;

const GIT_FETCH_CAS_RETRY_DELAY: Duration = Duration::from_millis(20);

/// Deadline [`run_git`] gives a local command. Local plumbing on Orbit's own
/// repository finishes in milliseconds, so only a wedged process reaches it.
/// It stays below [`GIT_REMOTE_TIMEOUT`]: a network command always gets the
/// longer bound.
pub const GIT_LOCAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Deadline for a command that materializes or deletes a whole checkout
/// (`worktree add`, `worktree remove`, `checkout`), as the engine gives
/// `worktree add`.
pub const GIT_CHECKOUT_TIMEOUT: Duration = Duration::from_secs(120);

/// Deadline for a command that talks to a remote (`fetch`, `ls-remote`,
/// `push`, `clone`). A single-branch fetch from the forge takes well under a
/// second. A fetch usually runs under [`with_git_fetch_lock`], whose waiters
/// give up after [`DEFAULT_FILE_LOCK_TIMEOUT`](super::file_lock::DEFAULT_FILE_LOCK_TIMEOUT),
/// so a stalled holder is killed while the next caller is still waiting.
pub const GIT_REMOTE_TIMEOUT: Duration = Duration::from_secs(15);

/// Longest any one holder keeps the [`with_git_fetch_lock`] lock. Waiters give
/// up after [`DEFAULT_FILE_LOCK_TIMEOUT`](super::file_lock::DEFAULT_FILE_LOCK_TIMEOUT),
/// so a holder that outlasts them turns one stalled fetch into a lock timeout
/// for every other caller. A holder with several attempts (delivery
/// `fetch_remote_base`) spends this as one total across its attempts and
/// backoff; the limit leaves a margin below the waiters' wait for the
/// supervisor to kill the last child. We bound the holder rather than widen
/// the waiters' wait: a waiter blocked for minutes behind a stalled network
/// call is a worse failure than the holder giving up early and retrying later.
pub const GIT_FETCH_LOCK_HOLD_LIMIT: Duration = Duration::from_secs(25);

// Pin the holder/waiter relationship: a change to either side that breaks it
// fails the build instead of reappearing as lock timeouts under a stalled fetch.
const _: () = assert!(
    GIT_REMOTE_TIMEOUT.as_millis() <= GIT_FETCH_LOCK_HOLD_LIMIT.as_millis(),
    "a single-fetch holder must fit within the fetch-lock hold limit"
);
const _: () = assert!(
    GIT_FETCH_LOCK_HOLD_LIMIT.as_millis() < super::file_lock::DEFAULT_FILE_LOCK_TIMEOUT.as_millis(),
    "the fetch-lock hold limit must stay below the wait of every fetch-lock waiter"
);

/// Bytes [`run_git`] keeps of each output stream. Callers parse ref lists,
/// commit headers and worktree lists, all far smaller.
pub const GIT_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentBranchStatus {
    Named(String),
    DetachedHead,
    NoCurrentBranch,
}

pub fn current_branch(workspace_path: &Path) -> Result<CurrentBranchStatus, OrbitError> {
    let symbolic = run_git(
        workspace_path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )?;
    if symbolic.success {
        let branch = symbolic.stdout.trim();
        if branch.is_empty() {
            return Ok(CurrentBranchStatus::NoCurrentBranch);
        }
        return Ok(CurrentBranchStatus::Named(branch.to_string()));
    }

    let verify_head = run_git(workspace_path, &["rev-parse", "--verify", "-q", "HEAD"])?;
    if verify_head.success {
        return Ok(CurrentBranchStatus::DetachedHead);
    }

    Ok(CurrentBranchStatus::NoCurrentBranch)
}

pub fn default_branch(workspace_path: &Path) -> Result<Option<String>, OrbitError> {
    for remote in preferred_remotes(workspace_path)? {
        if let Some(branch) = remote_default_branch(workspace_path, &remote)? {
            return Ok(Some(branch));
        }
    }

    let local_branches = local_branches(workspace_path)?;
    for branch in ["main", "master", "trunk", "develop", "development", "dev"] {
        if local_branches.iter().any(|candidate| candidate == branch) {
            return Ok(Some(branch.to_string()));
        }
    }

    if local_branches.len() == 1 {
        return Ok(local_branches.into_iter().next());
    }

    Ok(None)
}

fn preferred_remotes(workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    let remotes = run_git(workspace_path, &["remote"])?;
    if !remotes.success {
        return Ok(Vec::new());
    }

    let mut names: Vec<String> = remotes
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    names.sort();
    if let Some(origin_index) = names.iter().position(|name| name == "origin") {
        let origin = names.remove(origin_index);
        names.insert(0, origin);
    }
    Ok(names)
}

fn remote_default_branch(
    workspace_path: &Path,
    remote: &str,
) -> Result<Option<String>, OrbitError> {
    let remote_head = run_git(
        workspace_path,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            &format!("refs/remotes/{remote}/HEAD"),
        ],
    )?;
    if !remote_head.success {
        return Ok(None);
    }

    let head = remote_head.stdout.trim();
    if let Some(branch) = head.strip_prefix(&format!("{remote}/")) {
        return Ok(Some(branch.to_string()));
    }
    if !head.is_empty() {
        return Ok(Some(head.to_string()));
    }
    Ok(None)
}

fn local_branches(workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    let branches = run_git(
        workspace_path,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?;
    if !branches.success {
        return Ok(Vec::new());
    }

    Ok(branches
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

pub fn git_common_dir(workspace_path: &Path) -> Result<PathBuf, OrbitError> {
    let output = run_git(
        workspace_path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if !output.success {
        return Err(OrbitError::Execution(format!(
            "unable to locate shared Git directory in '{}': {}",
            workspace_path.display(),
            output.stderr.trim()
        )));
    }
    let raw = output.stdout.trim();
    if raw.is_empty() {
        return Err(OrbitError::Execution(format!(
            "unable to locate shared Git directory in '{}'",
            workspace_path.display()
        )));
    }
    Ok(PathBuf::from(raw))
}

pub fn git_fetch_lock_target(git_common_dir: &Path) -> PathBuf {
    git_common_dir.join(GIT_FETCH_LOCK_NAME)
}

/// Run `op` while holding the exclusive lock that serializes Orbit-owned
/// remote-tracking-ref updates across linked checkouts.
pub fn with_git_fetch_lock<T, E, F>(workspace: &Path, op: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    let common = git_common_dir(workspace).map_err(|error| io::Error::other(error.to_string()))?;
    with_exclusive_file_lock(&git_fetch_lock_target(&common), "git fetch", op)
}

/// True when git refused a ref update because another process raced the same
/// remote-tracking ref. Auth and network failures stay false so callers do
/// not retry them.
pub fn is_git_ref_update_contention(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    if looks_like_remote_auth_or_network_failure(&text) {
        return false;
    }
    text.contains("cannot lock ref") || text.contains("unable to update local ref")
}

pub fn should_retry_git_ref_cas(attempt: u32, stderr: &str) -> bool {
    attempt + 1 < GIT_FETCH_CAS_ATTEMPTS && is_git_ref_update_contention(stderr)
}

pub fn git_fetch_cas_retry_delay() -> Duration {
    GIT_FETCH_CAS_RETRY_DELAY
}

fn looks_like_remote_auth_or_network_failure(stderr_lower: &str) -> bool {
    stderr_lower.contains("authentication failed")
        || stderr_lower.contains("could not resolve host")
        || stderr_lower.contains("unable to access")
        || stderr_lower.contains("terminal prompts disabled")
        || stderr_lower.contains("could not read username")
        || stderr_lower.contains("permission denied (publickey)")
        || stderr_lower.contains("the requested url returned error: 401")
        || stderr_lower.contains("the requested url returned error: 403")
}

/// `-c` overrides `run_git` accepts, as `(key, required value)`. Keys compare
/// case-insensitively, as Git's do. None of them names a program for Git to
/// run, and `core.hooksPath` is admitted only when it disables hooks.
const ADMITTED_CONFIG_OVERRIDES: &[(&str, Option<&str>)] = &[
    ("core.hooksPath", Some("/dev/null")),
    ("gc.auto", None),
    ("protocol.file.allow", None),
    ("user.name", None),
    ("user.email", None),
];

/// The read, checkout and history plumbing `run_git` drives. Commands that
/// exist to run caller-named programs (`rebase -x`, `bisect run`,
/// `submodule foreach`, `difftool -x`, `archive --exec`) and aliases stay out.
const ADMITTED_SUBCOMMANDS: &[&str] = &[
    "add",
    "branch",
    "cat-file",
    "checkout",
    "commit",
    "fetch",
    "for-each-ref",
    "init",
    "ls-tree",
    "merge-base",
    "remote",
    "rev-parse",
    "status",
    "symbolic-ref",
    "worktree",
];

/// Long options that hand Git a program to execute. Git accepts any unique
/// prefix of a long option, so a prefix of these is refused too.
const PROGRAM_OPTIONS: &[&str] = &["upload-pack", "receive-pack", "exec"];

/// Admit an argv for `git` before it reaches the process boundary.
///
/// `run_git` never goes through a shell, so an argument can only become a
/// command by asking Git to run one. This refuses every argv-borne way to do
/// that: a NUL byte, a global option other than an admitted `-c` override or
/// `--git-dir`, a subcommand outside [`ADMITTED_SUBCOMMANDS`], a long option
/// naming a program, and an `init` from a non-empty `--template`, whose hooks
/// later commands would run.
fn admitted_git_args<'a>(args: &[&'a str]) -> Result<Vec<&'a str>, OrbitError> {
    let refuse = |reason: String| {
        Err(OrbitError::InvalidInput(format!(
            "refusing to run git: {reason}"
        )))
    };
    if args.iter().any(|arg| arg.contains('\0')) {
        return refuse("an argument contains a NUL byte".to_string());
    }

    let mut index = 0;
    while let Some(&arg) = args.get(index) {
        if !arg.starts_with('-') {
            break;
        }
        if arg == "-c" {
            let Some(&setting) = args.get(index + 1) else {
                return refuse("`-c` has no setting".to_string());
            };
            if !is_admitted_config_override(setting) {
                return refuse(format!("config override `{setting}` is not admitted"));
            }
            index += 2;
        } else if arg == "--git-dir" {
            index += 2;
        } else if arg.starts_with("--git-dir=") {
            index += 1;
        } else {
            return refuse(format!("global option `{arg}` is not admitted"));
        }
    }

    let Some(&subcommand) = args.get(index) else {
        return refuse("no subcommand".to_string());
    };
    if !ADMITTED_SUBCOMMANDS.contains(&subcommand) {
        return refuse(format!("subcommand `{subcommand}` is not admitted"));
    }

    for &arg in &args[index + 1..] {
        let Some(option) = arg.strip_prefix("--").filter(|option| !option.is_empty()) else {
            continue;
        };
        let (name, value) = match option.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (option, None),
        };
        if PROGRAM_OPTIONS
            .iter()
            .any(|program_option| program_option.starts_with(name))
        {
            return refuse(format!("option `{arg}` names a program for git to run"));
        }
        if subcommand == "init" && "template".starts_with(name) && value != Some("") {
            return refuse(format!("option `{arg}` installs hooks from a template"));
        }
    }

    Ok(args.to_vec())
}

fn is_admitted_config_override(setting: &str) -> bool {
    let (key, value) = match setting.split_once('=') {
        Some((key, value)) => (key, Some(value)),
        None => (setting, None),
    };
    ADMITTED_CONFIG_OVERRIDES
        .iter()
        .any(|(admitted_key, required_value)| {
            admitted_key.eq_ignore_ascii_case(key)
                && required_value.is_none_or(|required| value == Some(required))
        })
}

/// Run `git` with `args` in `workspace_path` within [`GIT_LOCAL_TIMEOUT`].
/// An argv the admission above refuses fails with [`OrbitError::InvalidInput`]
/// before any process starts. A network or whole-checkout command passes its
/// own deadline through [`run_git_within`].
pub fn run_git(workspace_path: &Path, args: &[&str]) -> Result<GitCommandOutput, OrbitError> {
    run_git_within(workspace_path, args, GIT_LOCAL_TIMEOUT)
}

/// [`run_git`] with an explicit `deadline`.
///
/// Git runs in its own process group with [`GIT_OUTPUT_LIMIT`] of each stream
/// kept, and never prompts on a terminal. When `deadline` elapses the group is
/// killed and the error is [`OrbitError::ProcessTimeout`] naming the argv and
/// the workspace: a timeout is never an exit status, so a caller cannot read
/// it as Git's answer.
pub fn run_git_within(
    workspace_path: &Path,
    args: &[&str],
    deadline: Duration,
) -> Result<GitCommandOutput, OrbitError> {
    let admitted = admitted_git_args(args)?;
    let mut command = Command::new("git");
    command
        .args(&admitted)
        .current_dir(workspace_path)
        .env("GIT_TERMINAL_PROMPT", "0");
    let output =
        run_bounded_capped(&mut command, deadline, GIT_OUTPUT_LIMIT).map_err(
            |error| match error {
                OrbitError::ProcessTimeout { timeout_ms, .. } => OrbitError::ProcessTimeout {
                    timeout_ms,
                    detail: format!("`git {}` in '{}'", args.join(" "), workspace_path.display()),
                },
                other => OrbitError::Execution(format!(
                    "failed to run `git {}` in '{}': {other}",
                    args.join(" "),
                    workspace_path.display()
                )),
            },
        )?;

    Ok(GitCommandOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

pub struct GitCommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}
