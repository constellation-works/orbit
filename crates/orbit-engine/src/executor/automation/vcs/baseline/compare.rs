use super::super::required_command::{
    MAX_VALIDATION_MESSAGE_BYTES, RequiredCommandRun, VALIDATION_PATH_LINE, VALIDATION_TIMEOUT_MS,
    ValidationSummary, bounded_validation_text,
};
use super::cache::{
    CACHE_SCHEMA_VERSION, cache_paths, read_cached, remove_legacy_checkouts, run_on_base,
};
use crate::context::RuntimeHost;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::file_lock::{FileLockOptions, acquire_exclusive_file_lock};
use orbit_common::fs::io::atomic_write_bytes;
use orbit_types::workflow::{BaselineRedFailures, BaselineRedHold, VALIDATION_ENVIRONMENT_MARKER};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

/// Longest a candidate waits for another candidate's run of the same base
/// command: one validation timeout, plus slack for worktree setup.
const BASE_LOCK_TIMEOUT: Duration = Duration::from_secs(50 * 60);

/// The key Orbit adds to a candidate run's reported selection to hand a base
/// run only the tests the candidate failed [ORB-15328]. A command that honours
/// it runs just those tests within the selection and reports the selection it
/// was handed, key included.
pub(crate) const FAILING_TESTS_KEY: &str = "failing_tests";

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
    /// Whether complete base output refused any host coverage.
    #[serde(default)]
    pub host_coverage_refused: bool,
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
pub(super) fn not_comparable(
    selection: Option<&Value>,
    base: &BaseCommandResult,
) -> Option<String> {
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
pub(in super::super) struct BaselineCheck {
    pub(in super::super) base_sha: String,
    /// The base result, or why there is none.
    pub(in super::super) result: Result<BaseCommandResult, String>,
    /// Whether the result came from another candidate's run of this base.
    pub(in super::super) cached: bool,
    /// Why the base result does not show what the candidate's would
    /// ([`not_comparable`]), when it does not.
    pub(in super::super) not_comparable: Option<String>,
}

impl BaselineCheck {
    /// Whether the base fails exactly as the candidate did: same exit status
    /// and the same timeout outcome, on a comparable run. A base result that
    /// is itself a missing tool or a network failure was never cached and
    /// never gets here.
    pub(in super::super) fn reproduces(&self, run: &RequiredCommandRun) -> bool {
        run.infrastructure_failure.is_none()
            && self.not_comparable.is_none()
            && self.result.as_ref().is_ok_and(|base| {
                !base.passed && base.exit_code == run.exit_code && base.timed_out == run.timed_out
            })
    }

    /// A supervisor-stopped base is an environment failure, not a red base.
    pub(in super::super) fn infrastructure_failure(&self) -> Option<&str> {
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
    pub(in super::super) fn known_red<'run>(
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
    pub(in super::super) fn fails_all(
        &self,
        exit_code: Option<i32>,
        failures: &BTreeSet<String>,
    ) -> bool {
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
    pub(in super::super) fn decision(&self) -> &'static str {
        match &self.result {
            Ok(_) if self.not_comparable.is_some() => "not_comparable",
            Ok(base) if base.passed => "passed",
            Ok(_) => "failed",
            Err(_) => "inconclusive",
        }
    }

    /// The summary recorded beside the candidate's own result; `log` names
    /// the attached base log.
    pub(in super::super) fn record(&self, log: Option<&str>) -> Value {
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
    pub(in super::super) fn failures(&self) -> BaselineRedFailures {
        let Ok(base) = &self.result else {
            return BaselineRedFailures::default();
        };
        hold_failures(base.failures.iter().cloned(), &base.output)
    }

    /// The captured base log: identity, result and output.
    pub(in super::super) fn log(&self, run_id: &str) -> Value {
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
                "host_coverage_refused": base.host_coverage_refused,
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
/// failed test (`test …`, as [`super::super::base_claim::failure_identities`] reads
/// it) that `base_failures` names too, and must neither have timed out,
/// or been stopped by the supervisor, which could have cut its failures
/// short, nor deferred a sandbox-confined
/// path, which then never ran, or printed any other host coverage refusal
/// (including `SKIP:` and `skipping` notices) [ORB-15548].
/// A run that names no failure cannot be
/// compared; a compiler or lint error says nothing about which tests ran.
pub(in super::super) fn known_red(
    run: &RequiredCommandRun,
    base_failures: &BTreeSet<String>,
) -> bool {
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
pub(in super::super) fn hold_failures(
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
pub(in super::super) fn compare_with_base<H: RuntimeHost + ?Sized>(
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
pub(in super::super) fn compare_with_base_until<H: RuntimeHost + ?Sized>(
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
pub(in super::super) fn compare_claimed_with_base_until<H: RuntimeHost + ?Sized>(
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
                host_coverage_refused: run.host_output_refusal.is_some()
                    || run.deferred_notice.is_some(),
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

/// The typed failure for a command the base fails exactly as the candidate.
pub(in super::super) fn baseline_red_failure(
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
    let summary = run.failure_summary(candidate, "failed identically", logs);
    let message = hold.text(&format!(
        "{summary} The command fails on base {} exactly as on the candidate{}. \
         The candidate did not introduce the failure, so no repair runs: the candidate \
         is kept and the task is held until {base_ref} moves to a base where the command passes.",
        hold.base_sha,
        hold.failures_clause(),
    ));
    if message.len() > MAX_VALIDATION_MESSAGE_BYTES - 64 {
        // Never truncate the persisted hold JSON: admission needs its exact
        // command and selection to replay it. Refuse oversized metadata safely.
        return OrbitError::PolicyDenied(format!(
            "{summary} The base hold metadata exceeds the diagnostic limit; inspect the logs."
        ));
    }
    OrbitError::Execution(message)
}

/// A candidate failure the base does not share, with the base's result.
pub(in super::super) fn candidate_failure(
    run: &RequiredCommandRun,
    candidate: &str,
    check: Option<&BaselineCheck>,
    logs: &str,
) -> OrbitError {
    if let Some(reason) = check.and_then(BaselineCheck::infrastructure_failure) {
        return OrbitError::Execution(format!(
            "{VALIDATION_ENVIRONMENT_MARKER} {} {}",
            run.failure_summary(candidate, "inconclusive: base infrastructure stopped", logs),
            bounded_validation_text(reason, 2048),
        ));
    }
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
    run.failure(candidate, &baseline, logs)
}
