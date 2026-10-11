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
//! - unless every failure the candidate names is a test that also fails on
//!   the base ([`known_red`]) [ORB-15375]: the candidate introduced none of
//!   them, so it is not held, and the step records the known-red tests and
//!   proceeds. Review settlement applies the same rule to a reviewer's
//!   red-base claim [ORB-15546];
//! - a base that passes leaves the failure the candidate's, rejected as before.
//!
//! A base run that cannot be set up, or whose own result is a missing tool or
//! a network failure, is inconclusive and also leaves the failure the
//! candidate's. A base stopped by the output capture limit or by a signal is
//! never cached as a conclusive result and reports an infrastructure failure with the validation
//! environment marker, rather than attributing a verdict to either revision.
//! Owner claim settlement retains its inconclusive attempted runs separately,
//! so retries converge without restarting a deadline-cut build. These records
//! never establish known red and ordinary base comparisons ignore them.
//!
//! A command that selects what to test from the candidate's diff
//! (`make ci-test-affected`) selects nothing on the base itself, where that
//! diff is empty [ORB-15131]. When the candidate run reported its selection
//! (see [`super::required_command`]), the base run is handed that selection,
//! which also enters the cache key. A base result that ran another selection,
//! reported none, or passed without executing a counted test is *not
//! comparable* ([`not_comparable`]): it neither makes the failure
//! `baseline_red` nor refutes a reviewer's claim, and never lifts a hold that
//! recorded the selection.
//!
//! A reviewer's red-base claim is checked through the same machinery in
//! [`super::base_claim`], which may hand a base run only the candidate's
//! failing tests ([`FAILING_TESTS_KEY`]) [ORB-15328].

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::file_lock::{FileLockOptions, acquire_exclusive_file_lock};
use orbit_common::fs::git::git_common_dir;
use orbit_common::fs::io::atomic_write_bytes;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::{BaselineRedFailures, BaselineRedHold, VALIDATION_ENVIRONMENT_MARKER};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::git::{fetch_remote_base, git_output, git_run};
use super::required_command::{
    RequiredCommandRun, VALIDATION_PATH_LINE, VALIDATION_TIMEOUT_MS, ValidationSummary,
    run_required_command,
};
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
/// An inconclusive hold check may retry after this delay, or immediately
/// when its base moves. This marker is separate from conclusive results so
/// candidate validation never treats it as a cached command result.
const HOLD_INCONCLUSIVE_BACKOFF: chrono::Duration = chrono::Duration::minutes(15);
/// Directory under the Git common directory holding base results.
const CACHE_DIR: &str = "orbit-baseline";
/// Version 2 moved the base checkout out of the common directory, so a
/// version 1 result's output names paths under the old checkout root.
/// Version 3 records the command's summary [ORB-15131]; an older passing
/// result cannot show that it executed any test.
/// Version 4 ignores deadline-shortened timeouts [ORB-15350]; a shortened
/// timeout is treated as inconclusive and never cached.
/// Version 5 excludes capture-limit and signal stops: they are infrastructure
/// failures, never conclusive base results.
/// Version 6 records the failures the complete output names [ORB-15375].
const CACHE_SCHEMA_VERSION: u32 = 6;
/// Where version 1 checked bases out, under [`CACHE_DIR`]. Any checkout left
/// there trips the Linux Git protection scan until it is removed.
const LEGACY_WORKTREES_DIR: &str = "worktrees";
/// The key Orbit adds to a candidate run's reported selection to hand a base
/// run only the tests the candidate failed [ORB-15328]. A command that honours
/// it runs just those tests within the selection and reports the selection it
/// was handed, key included.
pub(crate) const FAILING_TESTS_KEY: &str = "failing_tests";

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
/// backoff. The returned run records how many reruns it took. `selection`,
/// from a candidate run's summary, is what a base run must test.
pub(super) fn run_validation_command<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    command: &str,
    selection: Option<&Value>,
) -> Result<RequiredCommandRun, OrbitError> {
    run_validation_command_until(host, workspace_path, command, selection, None)
}

/// [`run_validation_command`], ending by `deadline` when one is set: each
/// attempt's timeout is shortened to what is left, and no rerun starts after
/// it.
pub(super) fn run_validation_command_until<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    command: &str,
    selection: Option<&Value>,
    deadline: Option<Instant>,
) -> Result<RequiredCommandRun, OrbitError> {
    let mut run = run_required_command(
        host,
        workspace_path,
        command,
        selection,
        attempt_timeout_ms(deadline),
    )?;
    for backoff in NETWORK_RETRY_BACKOFF {
        if run.passed
            || run.timed_out
            || run.missing_tool.is_some()
            || run.infrastructure_failure.is_some()
        {
            break;
        }
        let Some(evidence) = network_inconclusive(&run.output) else {
            break;
        };
        if deadline.is_some_and(|deadline| Instant::now() + backoff >= deadline) {
            break;
        }
        tracing::info!(
            command = run.command,
            evidence,
            retry = run.network_retries + 1,
            "required validation was network-inconclusive; retrying"
        );
        std::thread::sleep(backoff);
        let retries = run.network_retries + 1;
        run = run_required_command(
            host,
            workspace_path,
            command,
            selection,
            attempt_timeout_ms(deadline),
        )?;
        run.network_retries = retries;
    }
    if !run.passed
        && !run.timed_out
        && run.missing_tool.is_none()
        && run.infrastructure_failure.is_none()
    {
        run.network_evidence = network_inconclusive(&run.output);
    }
    Ok(run)
}

/// One attempt's timeout: the validation ceiling, shortened to what is left
/// before `deadline`, and never zero.
fn attempt_timeout_ms(deadline: Option<Instant>) -> u64 {
    let left = deadline.map_or(u64::MAX, |deadline| {
        u64::try_from(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    });
    VALIDATION_TIMEOUT_MS.min(left).max(1)
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
    /// What the base run reported it tested [ORB-15131].
    #[serde(default)]
    pub summary: Option<ValidationSummary>,
    /// The failures the base run's complete output names [ORB-15375].
    #[serde(default)]
    pub failures: BTreeSet<String>,
    pub recorded_at: chrono::DateTime<Utc>,
}

/// Why `base` cannot stand in for the candidate's run of the same command,
/// which reported `selection`, or `None` when it can [ORB-15131].
///
/// A command that reports no summary on either side is judged by its exit
/// status as before. Otherwise the base must report the candidate's selection,
/// and a pass counts only when it executed at least one counted test. A
/// replay of the candidate's failing tests ([`FAILING_TESTS_KEY`]) must have
/// executed one whether it passed or failed: a filter that matched nothing
/// shows nothing about those tests [ORB-15328].
fn not_comparable(selection: Option<&Value>, base: &BaseCommandResult) -> Option<String> {
    let summary = base.summary.as_ref();
    let replays_failures =
        selection.is_some_and(|selection| selection.get(FAILING_TESTS_KEY).is_some());
    if let Some(selection) = selection {
        let Some(summary) = summary else {
            return Some(format!(
                "the base run reported no selection, so it cannot be shown to have tested the \
                 candidate's ({selection})"
            ));
        };
        if summary.selection != *selection {
            return Some(format!(
                "the base run tested {} where the candidate tested {selection}",
                summary.selection
            ));
        }
    }
    match summary.map(|summary| summary.tests_run) {
        Some(Some(0)) if base.passed => {
            Some("the base run passed without executing a test".to_string())
        }
        Some(None) if base.passed => {
            Some("the base run passed without reporting how many tests it executed".to_string())
        }
        Some(Some(0)) if replays_failures => {
            Some("the base replay of the failing tests executed none of them".to_string())
        }
        Some(None) if replays_failures => Some(
            "the base replay of the failing tests did not report how many it executed".to_string(),
        ),
        _ => None,
    }
}

/// What rerunning a failed command on the candidate's base showed.
pub(super) struct BaselineCheck {
    pub(super) base_sha: String,
    /// The base result, or why there is none.
    pub(super) result: Result<BaseCommandResult, String>,
    /// Whether the result came from another candidate's run of this base.
    pub(super) cached: bool,
    /// Why the base result does not show what the candidate's would
    /// ([`not_comparable`]), when it does not.
    pub(super) not_comparable: Option<String>,
}

impl BaselineCheck {
    /// Whether the base fails exactly as the candidate did: same exit status
    /// and the same timeout outcome, on a comparable run. A base result that
    /// is itself a missing tool or a network failure was never cached and
    /// never gets here.
    pub(super) fn reproduces(&self, run: &RequiredCommandRun) -> bool {
        run.infrastructure_failure.is_none()
            && self.not_comparable.is_none()
            && self.result.as_ref().is_ok_and(|base| {
                !base.passed && base.exit_code == run.exit_code && base.timed_out == run.timed_out
            })
    }

    /// A supervisor-stopped base is an environment failure, not a red base.
    pub(super) fn infrastructure_failure(&self) -> Option<&str> {
        self.result
            .as_ref()
            .err()
            .filter(|reason| reason.starts_with(VALIDATION_ENVIRONMENT_MARKER))
            .map(String::as_str)
    }

    /// [ORB-15375] The candidate's failing tests when the base fails them
    /// all: the run fails exactly as the base does ([`Self::reproduces`]),
    /// and its failures are known red against the base's ([`known_red`]).
    /// The candidate introduced none of them, so it is not held.
    ///
    /// `None` holds or fails the candidate as before.
    pub(super) fn known_red<'run>(
        &self,
        run: &'run RequiredCommandRun,
    ) -> Option<&'run BTreeSet<String>> {
        let base = self.result.as_ref().ok()?;
        (self.reproduces(run) && known_red(run, &base.failures)).then_some(&run.failures)
    }

    /// The check a claimed leaf's known-red report gets from the owner
    /// [ORB-15427]: a comparable base run that failed without timing out,
    /// with `exit_code` when one is given, and names every one of `failures`,
    /// which must be failed tests ([`names_only_failed_tests`]), the rule
    /// [`known_red`] applies to the owner's own run. `exit_code` is `None` for
    /// a base run handed only the failing tests, which ran part of the
    /// selection.
    pub(super) fn fails_all(&self, exit_code: Option<i32>, failures: &BTreeSet<String>) -> bool {
        self.not_comparable.is_none()
            && names_only_failed_tests(failures)
            && self.result.as_ref().is_ok_and(|base| {
                !base.passed
                    && !base.timed_out
                    && exit_code.is_none_or(|code| code == base.exit_code)
                    && failures.is_subset(&base.failures)
            })
    }

    /// `passed`, `failed`, `not_comparable` or `inconclusive`.
    pub(super) fn decision(&self) -> &'static str {
        match &self.result {
            Ok(_) if self.not_comparable.is_some() => "not_comparable",
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
            "reason": self.result.as_ref().err().or(self.not_comparable.as_ref()),
            "log": log,
        })
    }

    /// What the base run named as failing, for the hold to record
    /// [ORB-15374]: the failures its output names, or its last lines when it
    /// names none.
    pub(super) fn failures(&self) -> BaselineRedFailures {
        let Ok(base) = &self.result else {
            return BaselineRedFailures::default();
        };
        hold_failures(base.failures.iter().cloned(), &base.output)
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
                "summary": base.summary,
                "not_comparable": self.not_comparable,
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

/// The known-red rule delivery validation and review settlement share
/// [ORB-15375] [ORB-15546]: whether a failed candidate `run`'s failures are
/// all the base's to answer for, given the failures the base's run of the
/// same check names. The run must name at least one failure, every one a
/// failed test (`test …`, as [`super::base_claim::failure_identities`] reads
/// it) that `base_failures` names too, and must neither have timed out,
/// or been stopped by the supervisor, which could have cut its failures
/// short, nor deferred a sandbox-confined
/// path, which then never ran, or printed any other host coverage refusal
/// (including `SKIP:` and `skipping` notices) [ORB-15548].
/// A run that names no failure cannot be
/// compared; a compiler or lint error says nothing about which tests ran.
pub(super) fn known_red(run: &RequiredCommandRun, base_failures: &BTreeSet<String>) -> bool {
    !run.timed_out
        && run.infrastructure_failure.is_none()
        && run.deferred_notice.is_none()
        && run.host_output_refusal.is_none()
        && names_only_failed_tests(&run.failures)
        && run.failures.is_subset(base_failures)
}

/// Whether `failures` is a nonempty set of failed tests, every one a `test …`
/// identity as `failure_identities` reads it [ORB-15546]: the only failures a
/// base run can answer for. A compiler or lint error names no test, so says
/// nothing about which tests ran.
fn names_only_failed_tests(failures: &BTreeSet<String>) -> bool {
    !failures.is_empty()
        && failures
            .iter()
            .all(|failure| failure.starts_with(TEST_FAILURE_PREFIX))
}

/// The prefix `failure_identities` gives a failed test.
const TEST_FAILURE_PREFIX: &str = "test ";

/// What a hold records as failing [ORB-15374]: `identities`, as
/// `failure_identities` parsed them from `output`, or the tail of `output`
/// without the `PATH` line Orbit appends to a failed run, which names the
/// host rather than the failure.
pub(super) fn hold_failures(
    identities: impl IntoIterator<Item = String>,
    output: &str,
) -> BaselineRedFailures {
    let output = output
        .lines()
        .filter(|line| !line.starts_with(VALIDATION_PATH_LINE))
        .collect::<Vec<_>>()
        .join("\n");
    BaselineRedFailures::new(identities, &output)
}

/// Rerun `command` on `base_sha`, or read another candidate's run of it.
/// `selection` is what the candidate run reported testing, which the base
/// run is handed and must report back.
///
/// Never fails the step: anything that keeps the base from being judged is an
/// inconclusive check, which leaves the candidate's failure standing.
pub(super) fn compare_with_base<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    base_sha: &str,
    command: &str,
    selection: Option<&Value>,
) -> BaselineCheck {
    compare_with_base_until(host, workspace_path, base_sha, command, selection, None)
}

/// [`compare_with_base`], ending by `deadline` when one is set: the wait for
/// another candidate's run and the base run itself both stop there.
pub(super) fn compare_with_base_until<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    base_sha: &str,
    command: &str,
    selection: Option<&Value>,
    deadline: Option<Instant>,
) -> BaselineCheck {
    compare_base(
        host,
        workspace_path,
        base_sha,
        command,
        selection,
        BaseComparison::Ordinary(deadline),
    )
}

/// Owner claim comparisons retain inconclusive runs separately. They cannot
/// confirm known red, but settlement retries must not restart a cold base run.
pub(super) fn compare_claimed_with_base_until<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    base_sha: &str,
    command: &str,
    selection: Option<&Value>,
    deadline: Instant,
) -> BaselineCheck {
    compare_base(
        host,
        workspace_path,
        base_sha,
        command,
        selection,
        BaseComparison::OwnerClaim(deadline),
    )
}

enum BaseComparison {
    Ordinary(Option<Instant>),
    OwnerClaim(Instant),
}

fn compare_base<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    base_sha: &str,
    command: &str,
    selection: Option<&Value>,
    comparison: BaseComparison,
) -> BaselineCheck {
    let (deadline, retain_inconclusive) = match comparison {
        BaseComparison::Ordinary(deadline) => (deadline, false),
        BaseComparison::OwnerClaim(deadline) => (Some(deadline), true),
    };
    let inconclusive = |reason: String| BaselineCheck {
        base_sha: base_sha.to_string(),
        result: Err(reason),
        cached: false,
        not_comparable: None,
    };
    let judged = |result: BaseCommandResult, cached: bool| BaselineCheck {
        base_sha: base_sha.to_string(),
        not_comparable: not_comparable(selection, &result),
        result: Ok(result),
        cached,
    };
    let cache = match cache_paths(workspace_path, base_sha, command, selection) {
        Ok(cache) => cache,
        Err(error) => return inconclusive(format!("locate the base result cache: {error}")),
    };
    if let Some(result) = read_cached(&cache.result) {
        return judged(result, true);
    }
    if retain_inconclusive && let Some(reason) = read_claim_inconclusive(&cache.claim_inconclusive)
    {
        return BaselineCheck {
            cached: true,
            ..inconclusive(reason)
        };
    }
    let _guard = match acquire_exclusive_file_lock(
        &cache.lock,
        "required validation base run",
        FileLockOptions {
            timeout: deadline.map_or(BASE_LOCK_TIMEOUT, |deadline| {
                BASE_LOCK_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()))
            }),
            warn_after: Duration::from_secs(30),
            ..FileLockOptions::default()
        },
    ) {
        Ok(guard) => guard,
        Err(error) => return inconclusive(format!("lock the base result cache: {error}")),
    };
    // Another candidate may have finished the same base run while this one
    // waited for the lock.
    if let Some(result) = read_cached(&cache.result) {
        return judged(result, true);
    }
    if retain_inconclusive && let Some(reason) = read_claim_inconclusive(&cache.claim_inconclusive)
    {
        return BaselineCheck {
            cached: true,
            ..inconclusive(reason)
        };
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return inconclusive(
            "the base comparison deadline expired before a run could start".into(),
        );
    }
    remove_legacy_checkouts(workspace_path, &cache.legacy_worktrees);
    let check = (|| match run_on_base(
        host,
        workspace_path,
        &cache.worktree,
        base_sha,
        command,
        selection,
        deadline,
    ) {
        Ok(run) => {
            if let Some(cause) = &run.infrastructure_failure {
                return inconclusive(format!(
                    "{VALIDATION_ENVIRONMENT_MARKER} the base run of '{}' was stopped: {cause}",
                    run.command
                ));
            }
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
            if run.timed_out && run.timeout_ms < VALIDATION_TIMEOUT_MS {
                return inconclusive(format!(
                    "the base run of '{}' timed out before the comparison deadline",
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
                summary: run.summary,
                failures: run.failures,
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
            judged(result, false)
        }
        Err(error) => inconclusive(format!("run the command on base {base_sha}: {error}")),
    })();
    if retain_inconclusive && let Err(reason) = &check.result {
        let record = ClaimInconclusive {
            schema_version: 1,
            reason: reason.clone(),
        };
        if let Err(error) = serde_json::to_vec(&record)
            .map_err(std::io::Error::other)
            .and_then(|bytes| atomic_write_bytes(&cache.claim_inconclusive, &bytes))
        {
            tracing::warn!(path = %cache.claim_inconclusive.display(),
                "could not retain the owner claim base outcome: {error}");
        }
    }
    check
}

#[derive(Serialize, Deserialize)]
struct ClaimInconclusive {
    schema_version: u32,
    reason: String,
}

fn read_claim_inconclusive(path: &Path) -> Option<String> {
    let record: ClaimInconclusive = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (record.schema_version == 1).then_some(record.reason)
}

pub(super) struct CachePaths {
    result: PathBuf,
    lock: PathBuf,
    inconclusive_hold: PathBuf,
    claim_inconclusive: PathBuf,
    pub(super) worktree: PathBuf,
    legacy_worktrees: PathBuf,
}

pub(super) fn cache_paths(
    repo: &Path,
    base_sha: &str,
    command: &str,
    selection: Option<&Value>,
) -> Result<CachePaths, OrbitError> {
    let mut identity = format!("{}\0{}", base_sha.trim(), command.trim());
    // The selection is handed to the command outside its text, so a run of
    // another selection is another result [ORB-15131].
    if let Some(selection) = selection {
        identity.push('\0');
        identity.push_str(&selection.to_string());
    }
    let key = sha256_hex(identity.as_bytes());
    let dir = git_common_dir(repo)?.join(CACHE_DIR);
    Ok(CachePaths {
        result: dir.join(format!("{key}.json")),
        lock: dir.join(format!("{key}.lock")),
        inconclusive_hold: dir.join(format!("{key}.hold-inconclusive.json")),
        claim_inconclusive: dir.join(format!("{key}.claim-inconclusive.json")),
        worktree: scratch_checkout_path(repo, &format!("{CACHE_DIR}-{key}"))?,
        legacy_worktrees: dir.join(LEGACY_WORKTREES_DIR),
    })
}

/// The result another run already recorded for `command` on `base_sha` with
/// `selection`, judged against it, without waiting for or starting a run.
pub(super) fn cached_base_check(
    workspace_path: &Path,
    base_sha: &str,
    command: &str,
    selection: Option<&Value>,
) -> Option<BaselineCheck> {
    let cache = cache_paths(workspace_path, base_sha, command, selection).ok()?;
    let result = read_cached(&cache.result)?;
    Some(BaselineCheck {
        base_sha: base_sha.to_string(),
        not_comparable: not_comparable(selection, &result),
        result: Ok(result),
        cached: true,
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
    selection: Option<&Value>,
    deadline: Option<Instant>,
) -> Result<RequiredCommandRun, OrbitError> {
    in_detached_worktree(workspace_path, worktree, base_sha, |checkout| {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(OrbitError::Execution(
                "the base comparison deadline expired during checkout".into(),
            ));
        }
        run_validation_command_until(host, checkout, command, selection, deadline)
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
         {}{}{}). The candidate did not introduce the failure, so no repair runs: the candidate \
         is kept and the task is held until {base_ref} moves to a base where the command passes. \
         {logs}\n{}",
        run.command,
        hold.base_sha,
        run.exit_code,
        if run.timed_out { ", timed out" } else { "" },
        hold.failures_clause(),
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
    if let Some(reason) = check.and_then(BaselineCheck::infrastructure_failure) {
        return OrbitError::Execution(format!(
            "{reason}. Candidate {candidate} could not be compared with its base. {logs}"
        ));
    }
    let OrbitError::Execution(message) = run.failure(candidate) else {
        return run.failure(candidate);
    };
    let baseline = match check {
        None => "The base was not checked: the step names no synchronized base.".to_string(),
        Some(check) => match (&check.result, &check.not_comparable) {
            (Ok(_), Some(reason)) => format!(
                "Base {} ran the command, but its run is not comparable with the candidate's \
                 ({reason}), so the failure is judged the candidate's.",
                check.base_sha
            ),
            (Ok(base), None) if base.passed => format!(
                "Base {} passes this command, so the candidate introduced the failure.",
                check.base_sha
            ),
            (Ok(base), None) => format!(
                "Base {} also fails this command, but differently (exit {}{}), so the failure \
                 is judged the candidate's.",
                check.base_sha,
                base.exit_code,
                if base.timed_out { ", timed out" } else { "" }
            ),
            (Err(reason), _) => format!(
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
/// result cache), handing it the hold's recorded selection, and lift the hold
/// only after a comparable run passes. A failed, inconclusive or
/// not-comparable check keeps the task held. An inconclusive attempt is recorded
/// separately and suppresses further hold checks for 15 minutes on that tip
/// and command; a conclusive cached result always takes precedence.
/// A remote-tracking ref is refreshed
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
            if let Some(status) = recorded_moved_base_status(repo, hold, &tip) {
                return status;
            }
            let check = compare_with_base(host, repo, &tip, &hold.command, hold.selection.as_ref());
            if let Err(reason) = &check.result {
                record_inconclusive_hold(repo, &tip, hold, reason);
            }
            judge_moved_base(hold, &tip, check.result)
        }
        Err(status) => status,
    }
}

/// [`baseline_hold_status`] from what is already recorded, never running the
/// command: `None` when the base moved to a tip with neither a conclusive
/// result nor an inconclusive hold attempt within the 15-minute back-off,
/// so deciding needs a full run.
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
    recorded_moved_base_status(repo, hold, &tip)
}

/// A recent failed attempt to judge a hold, keyed by tip and command using
/// the cache path. It never represents a conclusive validation result.
#[derive(Serialize, Deserialize)]
struct InconclusiveHoldAttempt {
    recorded_at: chrono::DateTime<Utc>,
    reason: String,
}

fn record_inconclusive_hold(repo: &Path, tip: &str, hold: &BaselineRedHold, reason: &str) {
    let command = hold.command.as_str();
    let record = || -> Result<(), OrbitError> {
        let cache = cache_paths(repo, tip, command, hold.selection.as_ref())?;
        let bytes = serde_json::to_vec(&InconclusiveHoldAttempt {
            recorded_at: Utc::now(),
            reason: reason.to_string(),
        })
        .map_err(|error| OrbitError::Execution(format!("encode inconclusive hold: {error}")))?;
        atomic_write_bytes(&cache.inconclusive_hold, &bytes)?;
        Ok(())
    };
    if let Err(error) = record() {
        tracing::warn!(
            tip,
            command,
            "could not record inconclusive hold attempt: {error}"
        );
    }
}

fn recorded_moved_base_status(
    repo: &Path,
    hold: &BaselineRedHold,
    tip: &str,
) -> Option<BaselineHoldStatus> {
    let result = match cache_paths(repo, tip, &hold.command, hold.selection.as_ref()) {
        Ok(cache) => match read_cached(&cache.result) {
            Some(result) => Ok(result),
            None => {
                let bytes = std::fs::read(&cache.inconclusive_hold).ok()?;
                let attempt: InconclusiveHoldAttempt = serde_json::from_slice(&bytes).ok()?;
                let age = Utc::now().signed_duration_since(attempt.recorded_at);
                if age >= HOLD_INCONCLUSIVE_BACKOFF {
                    return None;
                }
                Err(attempt.reason)
            }
        },
        // A full run could not locate the cache either.
        Err(error) => Err(format!("locate the base result cache: {error}")),
    };
    Some(judge_moved_base(hold, tip, result))
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
            "`{base_ref}` is still at {tip}, where required validation `{}` fails{}",
            hold.command,
            hold.failures_clause()
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
    // A hold recorded before holds kept a selection names nothing to compare
    // with, so its tip is judged by exit status as before; the next delivery
    // holds again, with the selection, if the base is still red.
    let unlike = hold.selection.as_ref().and_then(|selection| {
        result
            .as_ref()
            .ok()
            .and_then(|result| not_comparable(Some(selection), result))
    });
    // [ORB-15374] A verdict that keeps the hold names what the held base
    // failed, so the readiness detail it becomes says what is red.
    let held = hold.failures_clause();
    match (result, unlike) {
        (Ok(_), Some(reason)) => BaselineHoldStatus::Holding(format!(
            "`{base_ref}` moved to {tip}, but the run of required validation `{}` there is not \
             comparable with the held one: {reason}{held}",
            hold.command
        )),
        (Ok(result), None) if result.passed => BaselineHoldStatus::Lifted(format!(
            "`{base_ref}` moved from {} to {tip}, where required validation `{}` passes",
            hold.base_sha, hold.command
        )),
        (Ok(result), None) => {
            let now = hold_failures(result.failures, &result.output);
            BaselineHoldStatus::Holding(format!(
                "`{base_ref}` moved to {tip}, where required validation `{}` still fails{}",
                hold.command,
                now.describe()
                    .map(|described| format!("; {described}"))
                    .unwrap_or(held)
            ))
        }
        (Err(reason), _) => BaselineHoldStatus::Holding(format!(
            "`{base_ref}` moved to {tip}, but required validation `{}` could not be checked: \
             {reason}{held}",
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
