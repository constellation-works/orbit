//! Telling a red base and a network flake apart from a candidate's own
//! failure [ORB-14258].
//!
//! The integration branch has no merge gates, so a required command can be
//! red on the very base every candidate starts from, and a check that calls
//! out to the network can fail for a reason no commit controls. Neither says
//! anything about the candidate. Both validation steps therefore run each
//! required command through [`run_validation_command`], which reruns a
//! network-inconclusive failure (curl status `000`, DNS, a TLS timeout) up to
//! [`NETWORK_RETRIES`] times with backoff, and judge a failure that survives
//! through [`compare_with_base`]:
//!
//! - the command is rerun on the candidate's synchronized base in a clean,
//!   detached worktree of that commit, under the managed worktree root and
//!   never inside the Git common directory, whose Linux protection scan
//!   refuses the symlinks a checkout can hold;
//! - the base result is cached in the repository's Git common directory per
//!   `(base, command)` behind an exclusive file lock, so concurrent
//!   candidates on one host share a single base run;
//! - a base that fails the same way (same exit status, same timeout) makes the
//!   failure typed `[baseline_red]`: no recovery repairs it, and the
//!   task is held until the base moves to a commit where the command passes
//!   ([`baseline_hold_status`]);
//! - a base that passes leaves the failure the candidate's, rejected as before.
//!
//! A base run that cannot be set up, or whose own result is a missing tool or
//! a network failure, is inconclusive and also leaves the failure the
//! candidate's.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::file_lock::{FileLockOptions, acquire_exclusive_file_lock};
use orbit_common::fs::git::git_common_dir;
use orbit_common::fs::io::atomic_write_bytes;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::BaselineRedHold;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::git::{fetch_remote_base, git_output, git_run};
use super::required_command::{RequiredCommandRun, run_required_command};
use super::worktree::scratch_checkout_path;

/// Reruns of a network-inconclusive failure, after the first attempt.
pub(super) const NETWORK_RETRIES: u32 = 2;
/// Wait before each rerun of a network-inconclusive failure.
const NETWORK_RETRY_BACKOFF: [Duration; NETWORK_RETRIES as usize] =
    [Duration::from_secs(2), Duration::from_secs(6)];
/// Longest a candidate waits for another candidate's run of the same base
/// command: one validation timeout, plus slack for worktree setup.
const BASE_LOCK_TIMEOUT: Duration = Duration::from_secs(50 * 60);
/// How often one process may refresh a held base from `origin`.
const HOLD_FETCH_INTERVAL: Duration = Duration::from_secs(120);
/// Directory under the Git common directory holding base results.
const CACHE_DIR: &str = "orbit-baseline";
/// Version 2 moved the base checkout out of the common directory, so a
/// version 1 result's output names paths under the old checkout root.
const CACHE_SCHEMA_VERSION: u32 = 2;
/// Where version 1 checked bases out, under [`CACHE_DIR`]. Any checkout left
/// there trips the Linux Git protection scan until it is removed.
const LEGACY_WORKTREES_DIR: &str = "worktrees";

/// Evidence that a failed command could not reach the network.
fn network_inconclusive(output: &str) -> Option<String> {
    static PATTERN: OnceLock<Option<Regex>> = OnceLock::new();
    let pattern = PATTERN
        .get_or_init(|| {
            Regex::new(
                r"(?im)^.*(?:\bstatus 000\b|\bhttp(?:_code)?[ :=]+000\b|\bcurl: \((?:5|6|7|28|35|56)\)|could not resolve (?:host|proxy)|temporary failure in name resolution|name or service not known|\beai_again\b|getaddrinfo .*(?:failed|enotfound)|tls handshake timeout|ssl connection timeout|handshake (?:timed out|operation timed out)|tls: handshake timeout).*$",
            )
            .ok()
        })
        .as_ref()?;
    pattern
        .find(output)
        .map(|line| line.as_str().trim().to_string())
}

/// Run one required command, rerunning a network-inconclusive failure with
/// backoff. The returned run records how many reruns it took.
pub(super) fn run_validation_command<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    command: &str,
) -> Result<RequiredCommandRun, OrbitError> {
    let mut run = run_required_command(host, workspace_path, command)?;
    for backoff in NETWORK_RETRY_BACKOFF {
        if run.passed || run.timed_out || run.missing_tool.is_some() {
            break;
        }
        let Some(evidence) = network_inconclusive(&run.output) else {
            break;
        };
        tracing::info!(
            command = run.command,
            evidence,
            retry = run.network_retries + 1,
            "required validation was network-inconclusive; retrying"
        );
        std::thread::sleep(backoff);
        let retries = run.network_retries + 1;
        run = run_required_command(host, workspace_path, command)?;
        run.network_retries = retries;
    }
    if !run.passed && !run.timed_out && run.missing_tool.is_none() {
        run.network_evidence = network_inconclusive(&run.output);
    }
    Ok(run)
}

/// One required command's result on a base commit, as cached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseCommandResult {
    pub schema_version: u32,
    pub base_sha: String,
    pub command: String,
    pub passed: bool,
    pub exit_code: i32,
    pub timed_out: bool,
    pub output: String,
    pub network_retries: u32,
    pub validation_env: Value,
    pub recorded_at: chrono::DateTime<Utc>,
}

/// What rerunning a failed command on the candidate's base showed.
pub(super) struct BaselineCheck {
    pub(super) base_sha: String,
    /// The base result, or why there is none.
    pub(super) result: Result<BaseCommandResult, String>,
    /// Whether the result came from another candidate's run of this base.
    pub(super) cached: bool,
}

impl BaselineCheck {
    /// Whether the base fails exactly as the candidate did: same exit status
    /// and the same timeout outcome. A base result that is itself a missing
    /// tool or a network failure was never cached and never gets here.
    pub(super) fn reproduces(&self, run: &RequiredCommandRun) -> bool {
        self.result.as_ref().is_ok_and(|base| {
            !base.passed && base.exit_code == run.exit_code && base.timed_out == run.timed_out
        })
    }

    /// `passed`, `failed` or `inconclusive`.
    pub(super) fn decision(&self) -> &'static str {
        match &self.result {
            Ok(base) if base.passed => "passed",
            Ok(_) => "failed",
            Err(_) => "inconclusive",
        }
    }

    /// The summary recorded beside the candidate's own result; `log` names
    /// the attached base log.
    pub(super) fn record(&self, log: Option<&str>) -> Value {
        json!({
            "base_sha": self.base_sha,
            "decision": self.decision(),
            "exit_code": self.result.as_ref().ok().map(|base| base.exit_code),
            "timed_out": self.result.as_ref().ok().map(|base| base.timed_out),
            "cached": self.cached,
            "reason": self.result.as_ref().err(),
            "log": log,
        })
    }

    /// The captured base log: identity, result and output.
    pub(super) fn log(&self, run_id: &str) -> Value {
        match &self.result {
            Ok(base) => json!({
                "schema_version": 1,
                "role": "baseline",
                "run_id": run_id,
                "base_sha": base.base_sha,
                "command": base.command,
                "exit_code": base.exit_code,
                "timed_out": base.timed_out,
                "passed": base.passed,
                "network_retries": base.network_retries,
                "output": base.output,
                "validation_env": base.validation_env,
                "recorded_at": base.recorded_at,
                "cached": self.cached,
            }),
            Err(reason) => json!({
                "schema_version": 1,
                "role": "baseline",
                "run_id": run_id,
                "base_sha": self.base_sha,
                "inconclusive": reason,
            }),
        }
    }
}

/// Rerun `command` on `base_sha`, or read another candidate's run of it.
///
/// Never fails the step: anything that keeps the base from being judged is an
/// inconclusive check, which leaves the candidate's failure standing.
pub(super) fn compare_with_base<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    base_sha: &str,
    command: &str,
) -> BaselineCheck {
    let inconclusive = |reason: String| BaselineCheck {
        base_sha: base_sha.to_string(),
        result: Err(reason),
        cached: false,
    };
    let cache = match cache_paths(workspace_path, base_sha, command) {
        Ok(cache) => cache,
        Err(error) => return inconclusive(format!("locate the base result cache: {error}")),
    };
    if let Some(result) = read_cached(&cache.result) {
        return BaselineCheck {
            base_sha: base_sha.to_string(),
            result: Ok(result),
            cached: true,
        };
    }
    let _guard = match acquire_exclusive_file_lock(
        &cache.lock,
        "required validation base run",
        FileLockOptions {
            timeout: BASE_LOCK_TIMEOUT,
            warn_after: Duration::from_secs(30),
        },
    ) {
        Ok(guard) => guard,
        Err(error) => return inconclusive(format!("lock the base result cache: {error}")),
    };
    // Another candidate may have finished the same base run while this one
    // waited for the lock.
    if let Some(result) = read_cached(&cache.result) {
        return BaselineCheck {
            base_sha: base_sha.to_string(),
            result: Ok(result),
            cached: true,
        };
    }
    remove_legacy_checkouts(workspace_path, &cache.legacy_worktrees);
    match run_on_base(host, workspace_path, &cache.worktree, base_sha, command) {
        Ok(run) => {
            if run.missing_tool.is_some() {
                return inconclusive(format!(
                    "the base run of '{}' lacked a tool in the validation environment",
                    run.command
                ));
            }
            if let Some(evidence) = &run.network_evidence {
                return inconclusive(format!(
                    "the base run of '{}' could not reach the network: {evidence}",
                    run.command
                ));
            }
            let result = BaseCommandResult {
                schema_version: CACHE_SCHEMA_VERSION,
                base_sha: base_sha.to_string(),
                command: run.command.clone(),
                passed: run.passed,
                exit_code: run.exit_code,
                timed_out: run.timed_out,
                validation_env: run.environment_record(),
                output: run.output,
                network_retries: run.network_retries,
                recorded_at: Utc::now(),
            };
            if let Err(error) = serde_json::to_vec(&result)
                .map_err(std::io::Error::other)
                .and_then(|bytes| atomic_write_bytes(&cache.result, &bytes))
            {
                tracing::warn!(
                    path = %cache.result.display(),
                    "could not cache the required validation base result: {error}"
                );
            }
            BaselineCheck {
                base_sha: base_sha.to_string(),
                result: Ok(result),
                cached: false,
            }
        }
        Err(error) => inconclusive(format!("run the command on base {base_sha}: {error}")),
    }
}

struct CachePaths {
    result: PathBuf,
    lock: PathBuf,
    worktree: PathBuf,
    legacy_worktrees: PathBuf,
}

fn cache_paths(repo: &Path, base_sha: &str, command: &str) -> Result<CachePaths, OrbitError> {
    let key = sha256_hex(format!("{}\0{}", base_sha.trim(), command.trim()).as_bytes());
    let dir = git_common_dir(repo)?.join(CACHE_DIR);
    Ok(CachePaths {
        result: dir.join(format!("{key}.json")),
        lock: dir.join(format!("{key}.lock")),
        worktree: scratch_checkout_path(repo, &format!("{CACHE_DIR}-{key}"))?,
        legacy_worktrees: dir.join(LEGACY_WORKTREES_DIR),
    })
}

fn read_cached(path: &Path) -> Option<BaseCommandResult> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<BaseCommandResult>(&bytes)
        .ok()
        .filter(|result| result.schema_version == CACHE_SCHEMA_VERSION)
}

/// Run `command` in a fresh detached worktree of `base_sha`, then remove it.
fn run_on_base<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    worktree: &Path,
    base_sha: &str,
    command: &str,
) -> Result<RequiredCommandRun, OrbitError> {
    in_detached_worktree(workspace_path, worktree, base_sha, |checkout| {
        run_validation_command(host, checkout, command)
    })?
}

/// Check `commit` out into a fresh detached worktree at `worktree`, run `f`
/// there, then remove the worktree whatever `f` returned.
pub(super) fn in_detached_worktree<T>(
    workspace_path: &Path,
    worktree: &Path,
    commit: &str,
    f: impl FnOnce(&Path) -> T,
) -> Result<T, OrbitError> {
    remove_worktree(workspace_path, worktree);
    if let Some(parent) = worktree.parent() {
        orbit_common::fs::io::create_private_dir_all(parent).map_err(|error| {
            OrbitError::Execution(format!("create '{}': {error}", parent.display()))
        })?;
    }
    let path = worktree.to_string_lossy();
    let added = git_run(
        workspace_path,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            "--end-of-options",
            &path,
            commit,
        ],
    )?;
    if !added.success {
        remove_worktree(workspace_path, worktree);
        return Err(OrbitError::Execution(format!(
            "git worktree add {commit}: {}",
            added.stderr.trim()
        )));
    }
    let result = f(worktree);
    remove_worktree(workspace_path, worktree);
    Ok(result)
}

/// Best-effort removal of every checkout an older Orbit left under the Git
/// common directory, then of the directory itself.
fn remove_legacy_checkouts(workspace_path: &Path, legacy: &Path) {
    let Ok(entries) = std::fs::read_dir(legacy) else {
        return;
    };
    for entry in entries.flatten() {
        remove_worktree(workspace_path, &entry.path());
    }
    if let Err(error) = std::fs::remove_dir(legacy) {
        tracing::warn!(
            path = %legacy.display(),
            "could not remove the legacy base checkout directory: {error}"
        );
    }
}

/// Best-effort removal of a base worktree and its registration.
fn remove_worktree(workspace_path: &Path, worktree: &Path) {
    if worktree.exists() {
        let path = worktree.to_string_lossy();
        let _ = git_run(
            workspace_path,
            &["worktree", "remove", "--force", "--end-of-options", &path],
        );
        if worktree.exists()
            && let Err(error) = std::fs::remove_dir_all(worktree)
        {
            tracing::warn!(
                path = %worktree.display(),
                "could not remove the required validation base worktree: {error}"
            );
        }
    }
    let _ = git_run(workspace_path, &["worktree", "prune"]);
}

/// The typed failure for a command the base fails exactly as the candidate.
pub(super) fn baseline_red_failure(
    hold: &BaselineRedHold,
    run: &RequiredCommandRun,
    candidate: &str,
    logs: &str,
) -> OrbitError {
    let base_ref = if hold.base_ref.is_empty() {
        "the base".to_string()
    } else {
        format!("`{}`", hold.base_ref)
    };
    OrbitError::Execution(hold.text(&format!(
        "required validation '{}' fails on base {} exactly as on candidate {candidate} (exit \
         {}{}). The candidate did not introduce the failure, so no repair runs: the candidate is \
         kept and the task is held until {base_ref} moves to a base where the command passes. \
         {logs}\n{}",
        run.command,
        hold.base_sha,
        run.exit_code,
        if run.timed_out { ", timed out" } else { "" },
        run.output
    )))
}

/// A candidate failure the base does not share, with the base's result.
pub(super) fn candidate_failure(
    run: &RequiredCommandRun,
    candidate: &str,
    check: Option<&BaselineCheck>,
    logs: &str,
) -> OrbitError {
    let OrbitError::Execution(message) = run.failure(candidate) else {
        return run.failure(candidate);
    };
    let baseline = match check {
        None => "The base was not checked: the step names no synchronized base.".to_string(),
        Some(check) => match &check.result {
            Ok(base) if base.passed => format!(
                "Base {} passes this command, so the candidate introduced the failure.",
                check.base_sha
            ),
            Ok(base) => format!(
                "Base {} also fails this command, but differently (exit {}{}), so the failure \
                 is judged the candidate's.",
                check.base_sha,
                base.exit_code,
                if base.timed_out { ", timed out" } else { "" }
            ),
            Err(reason) => format!(
                "Base {} could not be judged ({reason}), so the failure is judged the \
                 candidate's.",
                check.base_sha
            ),
        },
    };
    OrbitError::Execution(format!("{message}\n\n{baseline} {logs}"))
}

/// Whether a held task's base has moved to a commit where its command may
/// pass, observed in `repo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineHoldStatus {
    /// The base still fails the command; why.
    Holding(String),
    /// The hold no longer applies; why.
    Lifted(String),
}

/// Decide whether a [`BaselineRedHold`] still stands in `repo`.
///
/// The hold stands while its base ref still points at the red commit. When
/// that ref advances, run the command on the new tip (or use the shared
/// result cache) and lift the hold only after it passes. A failed or
/// inconclusive check keeps the task held. A remote-tracking ref is refreshed
/// from `origin` at most every `HOLD_FETCH_INTERVAL` per repository and
/// branch, so a held backlog does not wait on some other delivery to fetch.
///
/// A cache miss runs the whole command, for up to the validation timeout, so
/// only a background run may ask: the owner's clock tick reads
/// [`recorded_baseline_hold_status`] and leaves a miss to a detached refresh
/// run [ORB-14739, ORB-14823].
pub fn baseline_hold_status<H: RuntimeHost + ?Sized>(
    host: &H,
    repo: &Path,
    hold: &BaselineRedHold,
) -> BaselineHoldStatus {
    match moved_base_tip(repo, hold) {
        Ok(tip) => {
            let check = compare_with_base(host, repo, &tip, &hold.command);
            judge_moved_base(hold, &tip, check.result)
        }
        Err(status) => status,
    }
}

/// [`baseline_hold_status`] from what is already recorded, never running the
/// command: `None` when the base moved to a tip with no recorded result for
/// the hold's command, so deciding needs a full run.
///
/// It still reads the base ref, refreshing it from `origin` as
/// [`baseline_hold_status`] does, so it fits a short caller such as the clock
/// tick, which holds the host's sweep lock.
pub fn recorded_baseline_hold_status(
    repo: &Path,
    hold: &BaselineRedHold,
) -> Option<BaselineHoldStatus> {
    let tip = match moved_base_tip(repo, hold) {
        Ok(tip) => tip,
        Err(status) => return Some(status),
    };
    let result = match cache_paths(repo, &tip, &hold.command) {
        Ok(cache) => Ok(read_cached(&cache.result)?),
        // A full run could not locate the cache either.
        Err(error) => Err(format!("locate the base result cache: {error}")),
    };
    Some(judge_moved_base(hold, &tip, result))
}

/// The commit the hold's base ref now names, when it moved off the red
/// commit; otherwise the status that keeps the hold.
fn moved_base_tip(repo: &Path, hold: &BaselineRedHold) -> Result<String, BaselineHoldStatus> {
    let base_ref = hold.base_ref.trim();
    if base_ref.is_empty() {
        return Err(BaselineHoldStatus::Holding(
            "the hold names no base ref".to_string(),
        ));
    }
    if let Some(branch) = base_ref.strip_prefix("origin/")
        && fetch_due(repo, branch)
        && let Err(error) = fetch_remote_base(repo, branch)
    {
        tracing::debug!(branch, "baseline hold could not refresh its base: {error}");
    }
    let tip = git_output(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base_ref}^{{commit}}"),
        ],
    )
    .map_err(|error| {
        BaselineHoldStatus::Holding(format!(
            "`{base_ref}` cannot be read, so the new base cannot be checked: {error}"
        ))
    })?;
    if tip == hold.base_sha {
        return Err(BaselineHoldStatus::Holding(format!(
            "`{base_ref}` is still at {tip}, where required validation `{}` fails",
            hold.command
        )));
    }
    Ok(tip)
}

/// The hold's status once its base moved to `tip`, from the command's result
/// there or why there is none.
fn judge_moved_base(
    hold: &BaselineRedHold,
    tip: &str,
    result: Result<BaseCommandResult, String>,
) -> BaselineHoldStatus {
    let base_ref = hold.base_ref.trim();
    match result {
        Ok(result) if result.passed => BaselineHoldStatus::Lifted(format!(
            "`{base_ref}` moved from {} to {tip}, where required validation `{}` passes",
            hold.base_sha, hold.command
        )),
        Ok(_) => BaselineHoldStatus::Holding(format!(
            "`{base_ref}` moved to {tip}, where required validation `{}` still fails",
            hold.command
        )),
        Err(reason) => BaselineHoldStatus::Holding(format!(
            "`{base_ref}` moved to {tip}, but required validation `{}` could not be checked: {reason}",
            hold.command
        )),
    }
}

/// Whether `branch` in `repo` may be fetched again for a hold check.
fn fetch_due(repo: &Path, branch: &str) -> bool {
    static LAST: OnceLock<Mutex<HashMap<(PathBuf, String), Instant>>> = OnceLock::new();
    let Ok(mut last) = LAST.get_or_init(Default::default).lock() else {
        return false;
    };
    let key = (repo.to_path_buf(), branch.to_string());
    let now = Instant::now();
    if last
        .get(&key)
        .is_some_and(|at| now.duration_since(*at) < HOLD_FETCH_INTERVAL)
    {
        return false;
    }
    last.insert(key, now);
    true
}

/// What rerunning a reviewer's failed required check showed about its
/// claim that the pinned base fails it the same way [ORB-14434].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseFailureVerdict {
    /// The base fails exactly as the candidate: same exit status, same
    /// timeout outcome, and no failure the base does not also name. Carries
    /// the failures both name.
    Reproduced { failures: Vec<String> },
    /// The base fails too, but the candidate names failures it does not:
    /// those still belong to the candidate.
    CandidateAdds { failures: Vec<String> },
    /// The host's runs contradict the claim, and how.
    Contradicted(String),
    /// The host could not judge the claim, and why.
    Inconclusive(String),
}

/// A base-failure claim as the host checked it: the verdict and the logs of
/// both runs, for the review's evidence.
#[derive(Debug, Clone)]
pub struct BaseFailureCheck {
    pub command: String,
    pub base_sha: String,
    pub verdict: BaseFailureVerdict,
    /// The candidate run: exit status, timeout and captured output.
    pub candidate_log: Value,
    /// The base run as `BaselineCheck::log` records it.
    pub base_log: Value,
}

/// Check a reviewer's claim that `command` fails on `base_sha` exactly as on
/// the candidate checked out in `workspace_path` [ORB-14434].
///
/// Settlement never takes the claim on trust. The command runs again on the
/// candidate, then on the base through `compare_with_base`, whose
/// `(base, command)` result cache is the same one delivery validation fills,
/// so a gate-step `baseline_red` run of the same command on the same base is
/// reused rather than repeated. Beyond the exit status and timeout outcome
/// `BaselineCheck::reproduces` compares, the failures each output names
/// (`failure_identities`) must not grow on the candidate, and every failure
/// the reviewer named must appear in the base's output.
///
/// Only call this with a command the host itself trusts: it runs on the host,
/// outside any agent sandbox.
pub fn verify_base_failure<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    base_sha: &str,
    command: &str,
    claimed_failures: &[String],
    run_id: &str,
) -> Result<BaseFailureCheck, OrbitError> {
    let run = run_validation_command(host, workspace_path, command)?;
    let candidate_log = json!({
        "schema_version": 1,
        "role": "candidate",
        "run_id": run_id,
        "command": run.command,
        "exit_code": run.exit_code,
        "timed_out": run.timed_out,
        "passed": run.passed,
        "network_retries": run.network_retries,
        "output": run.output,
        "validation_env": run.environment_record(),
    });
    let checked = |verdict: BaseFailureVerdict, base_log: Value| BaseFailureCheck {
        command: run.command.clone(),
        base_sha: base_sha.to_string(),
        verdict,
        candidate_log: candidate_log.clone(),
        base_log,
    };
    if run.passed {
        return Ok(checked(
            BaseFailureVerdict::Contradicted(format!(
                "`{}` passes on the final candidate when the host runs it",
                run.command
            )),
            Value::Null,
        ));
    }
    if run.missing_tool.is_some() || run.network_evidence.is_some() {
        return Ok(checked(
            BaseFailureVerdict::Inconclusive(format!(
                "the host's run of `{}` on the candidate {}",
                run.command,
                if run.missing_tool.is_some() {
                    "lacked a tool in the validation environment"
                } else {
                    "could not reach the network"
                }
            )),
            Value::Null,
        ));
    }
    let check = compare_with_base(host, workspace_path, base_sha, command);
    let base_log = check.log(run_id);
    let base = match &check.result {
        Err(reason) => {
            return Ok(checked(
                BaseFailureVerdict::Inconclusive(format!(
                    "base {base_sha} could not be judged: {reason}"
                )),
                base_log,
            ));
        }
        Ok(base) if base.passed => {
            return Ok(checked(
                BaseFailureVerdict::Contradicted(format!(
                    "base {base_sha} passes `{}`",
                    run.command
                )),
                base_log,
            ));
        }
        Ok(base) => base,
    };
    if !check.reproduces(&run) {
        return Ok(checked(
            BaseFailureVerdict::Contradicted(format!(
                "base {base_sha} fails `{}` differently: exit {}{} there, exit {}{} on the \
                 candidate",
                run.command,
                base.exit_code,
                if base.timed_out { " (timed out)" } else { "" },
                run.exit_code,
                if run.timed_out { " (timed out)" } else { "" },
            )),
            base_log,
        ));
    }
    let base_output = normalize_output(&base.output);
    if let Some(missing) = claimed_failures
        .iter()
        .map(|failure| normalize_output(failure))
        .find(|failure| !failure.is_empty() && !base_output.contains(failure.as_str()))
    {
        return Ok(checked(
            BaseFailureVerdict::Contradicted(format!(
                "the claimed failure `{missing}` is not in base {base_sha}'s output"
            )),
            base_log,
        ));
    }
    let base_root = cache_paths(workspace_path, base_sha, command)
        .map(|paths| paths.worktree)
        .unwrap_or_default();
    let on_base = failure_identities(&base.output, &base_root);
    let on_candidate = failure_identities(&run.output, workspace_path);
    let added = on_candidate
        .difference(&on_base)
        .cloned()
        .collect::<Vec<_>>();
    let verdict = if added.is_empty() {
        BaseFailureVerdict::Reproduced {
            failures: on_candidate.into_iter().collect(),
        }
    } else {
        BaseFailureVerdict::CandidateAdds { failures: added }
    };
    Ok(checked(verdict, base_log))
}

/// Whitespace runs collapsed and color codes dropped, so two captures of one
/// failure compare equal.
fn normalize_output(text: &str) -> String {
    static ANSI: OnceLock<Option<Regex>> = OnceLock::new();
    let text = match ANSI
        .get_or_init(|| Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").ok())
        .as_ref()
    {
        Some(ansi) => ansi.replace_all(text, ""),
        None => std::borrow::Cow::Borrowed(text),
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The failures a check's output names [ORB-14434]: each failed test
/// (`cargo test`'s `test <name> ... FAILED`, nextest's `FAIL [..] <name>`)
/// and each compiler or lint error at its location (`<path>:<line>:<col>`
/// with its header line). Paths under `root`, where the run happened, are
/// made relative so a base worktree and the candidate compare equal.
///
/// Identities come from the captured text, which keeps a head and a tail of
/// each stream; anything a long output drops is not compared.
pub(super) fn failure_identities(output: &str, root: &Path) -> std::collections::BTreeSet<String> {
    static PATTERNS: OnceLock<Option<[Regex; 4]>> = OnceLock::new();
    let Some([libtest, nextest, header, location]) = PATTERNS
        .get_or_init(|| {
            Some([
                Regex::new(r"^test (\S+) \.\.\. FAILED$").ok()?,
                Regex::new(
                    r"^(?:FAIL|TIMEOUT|SIGSEGV|SIGABRT|SIGKILL|SIGBUS|LEAK-FAIL)\s*\[[^\]]*\]\s*(?:\(\s*\d+/\d+\)\s*)?(.+)$",
                )
                .ok()?,
                Regex::new(r"^(error(?:\[\w+\])?: .+)$").ok()?,
                Regex::new(r"^--> (\S+:\d+:\d+)$").ok()?,
            ])
        })
        .as_ref()
    else {
        return Default::default();
    };
    let root = format!("{}/", root.display());
    let mut identities = std::collections::BTreeSet::new();
    let mut last_header: Option<String> = None;
    for line in output.lines() {
        let line = normalize_output(line);
        if let Some(found) = libtest.captures(&line) {
            identities.insert(format!("test {}", &found[1]));
        } else if let Some(found) = nextest.captures(&line) {
            identities.insert(format!("test {}", found[1].trim()));
        } else if let Some(found) = header.captures(&line) {
            last_header = Some(found[1].to_string());
        } else if let Some(found) = location.captures(&line)
            && let Some(header) = last_header.take()
        {
            let at = found[1].strip_prefix(root.as_str()).unwrap_or(&found[1]);
            identities.insert(format!("{at}: {header}"));
        }
    }
    identities
}
