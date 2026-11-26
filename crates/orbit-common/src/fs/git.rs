use crate::env_vars;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::OrbitError;
use crate::process::run_bounded_capped;

use super::io::{try_with_exclusive_file_lock, with_exclusive_file_lock};

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

/// Deadline for a bulk object copy into an empty repository: the full-ancestry
/// fetch that seeds a source-inspection slot. A cold copy of Orbit's own
/// history took about 2 s on an idle 32-core host. The same copy overran
/// [`GIT_LOCAL_TIMEOUT`] under drain load, so this bound sits far above the
/// idle time and still caps how long a wedged copy can hold its slot.
pub const GIT_BULK_COPY_TIMEOUT: Duration = Duration::from_secs(120);

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

/// What the checkout's HEAD resolves to, distinguishing detached and unresolved states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentBranchStatus {
    /// HEAD names a branch, including an unborn branch with no commits.
    Named(String),
    /// HEAD resolves to a commit without a symbolic branch.
    DetachedHead,
    /// Git reported neither a branch name nor a resolvable HEAD.
    NoCurrentBranch,
}

/// Read symbolic HEAD first, then probe the commit to distinguish a detached checkout.
pub fn current_branch(
    env: &crate::Env,
    workspace_path: &Path,
) -> Result<CurrentBranchStatus, OrbitError> {
    let symbolic = run_git(
        env,
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

    let verify_head = run_git(
        env,
        workspace_path,
        &["rev-parse", "--verify", "-q", "HEAD"],
    )?;
    if verify_head.success {
        return Ok(CurrentBranchStatus::DetachedHead);
    }

    Ok(CurrentBranchStatus::NoCurrentBranch)
}

/// Prefer a recorded remote default, then a conventional local branch or the sole local branch.
pub fn default_branch(
    env: &crate::Env,
    workspace_path: &Path,
) -> Result<Option<String>, OrbitError> {
    for remote in preferred_remotes(env, workspace_path)? {
        if let Some(branch) = remote_default_branch(env, workspace_path, &remote)? {
            return Ok(Some(branch));
        }
    }

    let local_branches = local_branches(env, workspace_path)?;
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

fn preferred_remotes(env: &crate::Env, workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    let remotes = run_git(env, workspace_path, &["remote"])?;
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
    env: &crate::Env,
    workspace_path: &Path,
    remote: &str,
) -> Result<Option<String>, OrbitError> {
    let remote_head = run_git(
        env,
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

fn local_branches(env: &crate::Env, workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    let branches = run_git(
        env,
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

/// Resolve Git's absolute common directory, shared by every linked worktree.
pub fn git_common_dir(env: &crate::Env, workspace_path: &Path) -> Result<PathBuf, OrbitError> {
    let output = run_git(
        env,
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

/// Derive the shared lock target for Orbit's remote-tracking ref updates.
pub fn git_fetch_lock_target(git_common_dir: &Path) -> PathBuf {
    git_common_dir.join(GIT_FETCH_LOCK_NAME)
}

/// Run `op` while holding the exclusive lock that serializes Orbit-owned
/// remote-tracking-ref updates across linked checkouts.
pub fn with_git_fetch_lock<T, E, F>(env: &crate::Env, workspace: &Path, op: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    let common =
        git_common_dir(env, workspace).map_err(|error| io::Error::other(error.to_string()))?;
    with_exclusive_file_lock(&git_fetch_lock_target(&common), "git fetch", op)
}

/// [`with_git_fetch_lock`] only when the lock is free now: `Ok(None)` when
/// another Orbit fetch holds it. For an optional remote-tracking-ref update
/// that a concurrent writer makes redundant, so it never waits on that writer.
pub fn try_with_git_fetch_lock<T, E, F>(
    env: &crate::Env,
    workspace: &Path,
    op: F,
) -> Result<Option<T>, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    let common =
        git_common_dir(env, workspace).map_err(|error| io::Error::other(error.to_string()))?;
    try_with_exclusive_file_lock(&git_fetch_lock_target(&common), "git fetch", op)
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

/// Retry only a local ref update race while the zero-based attempt still leaves budget.
pub fn should_retry_git_ref_cas(attempt: u32, stderr: &str) -> bool {
    attempt + 1 < GIT_FETCH_CAS_ATTEMPTS && is_git_ref_update_contention(stderr)
}

/// The pause between attempts to fetch after a ref update race.
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
    ("core.fsmonitor", Some("false")),
    // An integer worker count for parallel checkout; it names no program.
    ("checkout.workers", None),
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
    "log",
    "ls-tree",
    "ls-remote",
    "merge-base",
    "remote",
    "rev-parse",
    "status",
    "symbolic-ref",
    "update-ref",
    "worktree",
];

/// Long options that hand Git a program to execute. Git accepts any unique
/// prefix of a long option, so a prefix of these is refused too.
const PROGRAM_OPTIONS: &[&str] = &[
    "upload-pack",
    "receive-pack",
    "exec",
    "ext-diff",
    "textconv",
];

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
            if args.get(index + 1).is_none_or(|value| value.is_empty()) {
                return refuse("`--git-dir` has no path".to_string());
            }
            index += 2;
        } else if let Some(value) = arg.strip_prefix("--git-dir=") {
            if value.is_empty() {
                return refuse("`--git-dir=` has no path".to_string());
            }
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
pub fn run_git(
    env: &crate::Env,
    workspace_path: &Path,
    args: &[&str],
) -> Result<GitCommandOutput, OrbitError> {
    run_git_within(env, workspace_path, args, GIT_LOCAL_TIMEOUT)
}

/// Build a sanitized `git` [`Command`] rooted at `workspace_path` after
/// validating `args` through the same argv admission rules as [`run_git`].
/// The child runs under the C locale without `LANGUAGE`, so stderr
/// classification is stable, never prompts (`GIT_TERMINAL_PROMPT=0`), and has
/// inherited `GIT_DIR` and `GIT_CONFIG*` overrides removed.
pub fn git_command(
    env: &crate::Env,
    workspace_path: &Path,
    args: &[&str],
) -> Result<Command, OrbitError> {
    let admitted = admitted_git_args(args)?;
    let mut command = Command::new("git");
    command
        .args(&admitted)
        .current_dir(workspace_path)
        .env_clear()
        .envs(env.vars())
        .env(env_vars::LC_ALL, "C")
        .env_remove("LANGUAGE")
        .env("GIT_TERMINAL_PROMPT", "0");
    for (name, _) in env.vars() {
        if name == env_vars::GIT_DIR
            || name == "GIT_CONFIG"
            || name.to_string_lossy().starts_with("GIT_CONFIG_")
        {
            command.env_remove(name);
        }
    }
    Ok(command)
}

/// [`run_git`] with an explicit `deadline`.
///
/// Git runs in its own process group with [`GIT_OUTPUT_LIMIT`] of each stream
/// kept, and never prompts on a terminal. When `deadline` elapses the group is
/// killed and the error is [`OrbitError::ProcessTimeout`] naming the argv and
/// the workspace: a timeout is never an exit status, so a caller cannot read
/// it as Git's answer.
/// Children use the C locale without `LANGUAGE` so stderr classification is
/// stable, and discard inherited `GIT_DIR` and `GIT_CONFIG*` overrides. Callers
/// select repositories and admitted configuration explicitly through argv.
pub fn run_git_within(
    env: &crate::Env,
    workspace_path: &Path,
    args: &[&str],
    deadline: Duration,
) -> Result<GitCommandOutput, OrbitError> {
    let output = run_git_bytes_within(env, workspace_path, args, deadline)?;
    Ok(GitCommandOutput {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// [`run_git_within`] that keeps the child's streams as raw bytes.
///
/// A caller decoding NUL-delimited (`-z`) output needs the exact bytes: the
/// lossy text in [`GitCommandOutput`] replaces a byte sequence that is not
/// UTF-8, so a path read from it would no longer name the original entry.
/// The spawn, environment, deadline and output-limit rules are those of
/// [`run_git_within`].
pub fn run_git_bytes_within(
    env: &crate::Env,
    workspace_path: &Path,
    args: &[&str],
    deadline: Duration,
) -> Result<crate::process::CapturedOutput, OrbitError> {
    let mut command = git_command(env, workspace_path, args)?;
    run_bounded_capped(&mut command, deadline, GIT_OUTPUT_LIMIT).map_err(|error| match error {
        OrbitError::ProcessTimeout { timeout_ms, .. } => OrbitError::ProcessTimeout {
            timeout_ms,
            detail: format!("`git {}` in '{}'", args.join(" "), workspace_path.display()),
        },
        other => OrbitError::Execution(format!(
            "failed to run `git {}` in '{}': {other}",
            args.join(" "),
            workspace_path.display()
        )),
    })
}

/// What `git merge-base --is-ancestor` answered, as [`git_ancestry`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitAncestry {
    /// Exit 0: the first commit is an ancestor of (or equal to) the second.
    Ancestor,
    /// Exit 1: it is not.
    NotAncestor,
    /// Any other exit (such as 128, an unresolvable revision): Git could not
    /// answer. `code` is `None` when a signal ended the process.
    Unanswered {
        /// Child exit code, or `None` if the process was terminated by a signal.
        code: Option<i32>,
        /// Git's diagnostic for why it could not answer the ancestry question.
        stderr: String,
    },
}

/// Ask Git whether `ancestor` is an ancestor of `descendant` in `repo`.
///
/// Spawn and timeout failures propagate as [`run_git`] reports them. Each
/// caller decides whether [`GitAncestry::Unanswered`] reads as "not an
/// ancestor" or as an error.
pub fn git_ancestry(
    env: &crate::Env,
    repo: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<GitAncestry, OrbitError> {
    let answer = run_git(
        env,
        repo,
        &[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            ancestor,
            descendant,
        ],
    )?;
    Ok(match answer.code {
        Some(0) => GitAncestry::Ancestor,
        Some(1) => GitAncestry::NotAncestor,
        code => GitAncestry::Unanswered {
            code,
            stderr: answer.stderr,
        },
    })
}

/// A completed Git subprocess. Nonzero exit is available to callers as data.
pub struct GitCommandOutput {
    /// Whether Git exited normally with status zero.
    pub success: bool,
    /// Git's exit status; `None` when a signal ended the process.
    pub code: Option<i32>,
    /// Captured standard output decoded with lossy UTF-8.
    pub stdout: String,
    /// Captured diagnostic output decoded with lossy UTF-8.
    pub stderr: String,
}
