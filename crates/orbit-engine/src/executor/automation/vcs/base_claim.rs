//! Checking a reviewer's claim that the pinned base fails a required check
//! the same way [ORB-14434], through the base comparison delivery validation
//! uses ([`super::baseline`]).
//!
//! The claim's check need not replay the whole selection on the base
//! [ORB-15328]. When the candidate's failures are all named tests, the base
//! run is handed the candidate's selection with [`FAILING_TESTS_KEY`] added
//! and runs only those tests; a full-selection result already cached for that
//! exact base is read instead. The check's host runs share a
//! [`ReplayBudget`] whose last quarter is reserved for the base: the
//! candidate rerun is stopped before the reserve, and a stopped run is
//! reported as cut, with the failures it printed still replayed on the base.
//!
//! A reproduced claim whose failures are all tests the base fails too is
//! known red [ORB-15546], by the rule delivery validation applies to the
//! owner's required commands ([`known_red`]): the candidate introduced none
//! of them, so settlement lets it continue instead of holding it.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_types::workflow::BaselineRedFailures;
use regex::Regex;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::baseline::{
    BaseCommandResult, FAILING_TESTS_KEY, cached_base_check, compare_claimed_with_base_until,
    compare_with_base_until, hold_failures, known_red, run_validation_command_until,
};
use super::git::{BaseSyncMode, git_command_success, resolve_worktree_start_point};
use super::required_command::{VALIDATION_TIMEOUT_MS, ValidationSummary};

/// The share of a [`ReplayBudget`] reserved for the base replay: one part in
/// this many.
const BASE_REPLAY_RESERVE_DIVISOR: u32 = 4;

/// What rerunning a reviewer's failed required check showed about its
/// claim that the pinned base fails it the same way [ORB-14434].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseFailureVerdict {
    /// The base fails exactly as the candidate: same exit status, same
    /// timeout outcome, and no failure the base does not also name. Carries
    /// the failures both name.
    Reproduced { failures: Vec<String> },
    /// The base fails as the candidate does, and the candidate's failures
    /// are known red [ORB-15546]: at least one, every one a test the base
    /// fails too, with no timeout and no deferred sandbox path on the
    /// candidate, and no supervisor stop. The candidate introduced none of
    /// them. Carries them.
    KnownRed { failures: Vec<String> },
    /// The base fails too, but the candidate names failures it does not:
    /// those still belong to the candidate.
    CandidateAdds { failures: Vec<String> },
    /// The final candidate passes when the host runs it [ORB-15122], so the
    /// reviewer's failure was its own environment's. A run that reports a
    /// summary executed at least one counted test; carries that count.
    CandidatePasses { tests_run: Option<u64> },
    /// The candidate fails on the host and the base passes a comparable run
    /// [ORB-15122]: the failure is the candidate's own. Carries how.
    BasePasses(String),
    /// The host's candidate run was stopped at the base-replay reserve
    /// [ORB-15328], so it may not have reached every failure. Every failure
    /// it printed fails on the base too, so those are attributed to the base,
    /// but the claim is not confirmed. Carries the cut and those failures;
    /// none when the run named no failure the base could replay.
    Cut {
        reason: String,
        failures: Vec<String>,
    },
    /// The host's runs contradict the claim otherwise, and how.
    Contradicted(String),
    /// The base ran, but its run does not show what the candidate's would:
    /// it tested another selection, or passed without executing a counted
    /// test [ORB-15131]. The claim is neither refuted nor confirmed.
    NotComparable(String),
    /// The host could not judge the claim, and why.
    Inconclusive(String),
}

/// How a base-failure check judged the base [ORB-15328].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseReplay {
    /// A full-selection result another run already recorded for this exact
    /// base and command, read without running anything.
    CachedFailureSet,
    /// Only the tests the candidate run failed, replayed on the base (or that
    /// replay's cached result).
    FailingTests,
    /// The whole selection, because the candidate's failures are not all
    /// tests a run can be handed by name.
    FullSelection,
}

impl BaseReplay {
    /// The name settlement records.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CachedFailureSet => "cached_failure_set",
            Self::FailingTests => "failing_tests",
            Self::FullSelection => "full_selection",
        }
    }
}

/// The wall clock the host's runs for one base-failure check may spend
/// [ORB-15328]. A quarter of it is reserved for the base replay: the
/// candidate rerun is stopped before it reaches the reserve, and the base
/// replay always gets at least the reserve. Each run is still capped at the
/// validation timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayBudget {
    window: Duration,
}

impl ReplayBudget {
    /// A budget of `window` for one check.
    pub fn new(window: Duration) -> Self {
        Self { window }
    }

    /// The time held back for the base replay.
    pub fn reserve(&self) -> Duration {
        self.window / BASE_REPLAY_RESERVE_DIVISOR
    }

    /// The longest the candidate rerun may take.
    fn candidate_limit(&self) -> Duration {
        self.window
            .saturating_sub(self.reserve())
            .min(Duration::from_millis(VALIDATION_TIMEOUT_MS))
    }
}

/// A base-failure claim as the host checked it: the verdict and the logs of
/// both runs, for the review's evidence.
#[derive(Debug, Clone)]
pub struct BaseFailureCheck {
    pub command: String,
    pub base_sha: String,
    pub verdict: BaseFailureVerdict,
    /// The selection the candidate run reported testing, which the base run
    /// was handed and a hold records [ORB-15131].
    pub selection: Option<Value>,
    /// How the base was judged, when it was.
    pub base_replay: Option<BaseReplay>,
    /// How the candidate rerun was stopped at the base-replay reserve, when
    /// it was.
    pub cut: Option<String>,
    /// The candidate run: exit status, timeout and captured output.
    pub candidate_log: Value,
    /// The base run as `BaselineCheck::log` records it.
    pub base_log: Value,
}

impl BaseFailureCheck {
    /// What a hold for a reproduced claim, or the record of a known-red one
    /// [ORB-15546], names as failing on the base [ORB-15374]: the failures
    /// both runs name, or the tail of the base run's output when they name
    /// none.
    pub fn base_failures(&self) -> BaselineRedFailures {
        let identities = match &self.verdict {
            BaseFailureVerdict::Reproduced { failures }
            | BaseFailureVerdict::KnownRed { failures } => failures.clone(),
            _ => Vec::new(),
        };
        hold_failures(
            identities,
            self.base_log["output"].as_str().unwrap_or_default(),
        )
    }
}

/// Check a reviewer's claim that `command` fails on `base_sha` exactly as on
/// the candidate checked out in `workspace_path` [ORB-14434].
///
/// Settlement never takes the claim on trust. The command runs again on the
/// candidate first: a pass there is `CandidatePasses` unless its output shows
/// deferred, skipped, or unavailable-sandbox evidence, or its summary shows
/// it executed no counted test; those leave the claim inconclusive [ORB-15122].
/// A failed candidate run stopped by a signal or output-capture limit is
/// inconclusive without replay: its printed failures may be incomplete.
///
/// A failure there is checked on the base [ORB-15328]. A full-selection
/// result the `(base, command, selection)` cache already holds, as delivery
/// validation fills it, is read without running anything. Otherwise, when
/// the run reported a selection and every failure it names is a test, the
/// base runs only those tests (and any the reviewer named): the selection is
/// handed over with the `failing_tests` selection key. Failures that name no test, or a
/// command that reports no selection, replay the whole selection through
/// `compare_with_base`. A base run that is not comparable with what it was
/// handed settles nothing.
///
/// A full replay must reproduce the exit status and timeout outcome
/// `BaselineCheck::reproduces` compares; a replay of the failing tests ran
/// part of the selection, so only its own timeout disqualifies it. Either
/// way the failures each output names (`failure_identities`) must not grow
/// on the candidate, and every failure the reviewer named must appear in the
/// base's output. A reproduced claim is `KnownRed` when the candidate's
/// failures also meet the known-red rule delivery validation applies
/// (`known_red`) [ORB-15546], and `Reproduced` otherwise.
///
/// `budget` bounds the runs. A candidate rerun stopped at its reserve is
/// `Cut` when the base shares every failure it printed: the cut is named and
/// those failures stay attributed to the base, but nothing is confirmed.
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
    budget: ReplayBudget,
) -> Result<BaseFailureCheck, OrbitError> {
    let started = Instant::now();
    let candidate_limit = budget.candidate_limit();
    let run = run_validation_command_until(
        host,
        workspace_path,
        command,
        None,
        Some(started + candidate_limit),
    )?;
    // Stopped by the reserve rather than by the validation timeout.
    let cut = (run.timed_out && candidate_limit < Duration::from_millis(VALIDATION_TIMEOUT_MS))
        .then(|| {
            format!(
                "the host's run of `{}` on the candidate was stopped after {} s to keep {} s of \
                 its {} s budget for the base replay",
                run.command,
                candidate_limit.as_secs(),
                budget.reserve().as_secs(),
                budget.window.as_secs()
            )
        });
    let base_deadline = (started + budget.window).max(Instant::now() + budget.reserve());
    let host_output_refusal = run.host_output_refusal.as_ref().map(|refusal| {
        json!({
            "reason": refusal.reason.as_str(),
            "detail": refusal.detail,
        })
    });
    let candidate_log = json!({
        "schema_version": 1,
        "role": "candidate",
        "run_id": run_id,
        "command": run.command,
        "exit_code": run.exit_code,
        "timed_out": run.timed_out,
        "cut": cut,
        "passed": run.passed,
        "network_retries": run.network_retries,
        "output": run.output,
        "host_output_refusal": host_output_refusal,
        "validation_env": run.environment_record(),
        "summary": run.summary,
    });
    let checked =
        |verdict: BaseFailureVerdict, base_log: Value, base_replay: Option<BaseReplay>| {
            BaseFailureCheck {
                command: run.command.clone(),
                base_sha: base_sha.to_string(),
                verdict,
                selection: run.selection().cloned(),
                base_replay,
                cut: cut.clone(),
                candidate_log: candidate_log.clone(),
                base_log,
            }
        };
    if run.passed {
        let verdict = match &run.host_output_refusal {
            Some(refusal) => BaseFailureVerdict::Inconclusive(format!(
                "the host's run of `{}` on the candidate was refused as {}: {}",
                run.command,
                refusal.reason.as_str(),
                refusal.detail
            )),
            None => match executed_no_tests(run.summary.as_ref()) {
                Some(reason) => BaseFailureVerdict::Inconclusive(format!(
                    "the host's run of `{}` on the candidate {reason}",
                    run.command
                )),
                None => BaseFailureVerdict::CandidatePasses {
                    tests_run: run.summary.as_ref().and_then(|summary| summary.tests_run),
                },
            },
        };
        return Ok(checked(verdict, Value::Null, None));
    }
    if let Some(cause) = &run.infrastructure_failure {
        return Ok(checked(
            BaseFailureVerdict::Inconclusive(format!(
                "the host's run of `{}` on the candidate was stopped: {cause}",
                run.command
            )),
            Value::Null,
            None,
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
            None,
        ));
    }
    let on_candidate = run.failures.clone();
    let full = run.selection().cloned();
    // Only a comparable, conclusive record stands in for a run.
    let cached = cut
        .is_none()
        .then(|| cached_base_check(workspace_path, base_sha, command, full.as_ref()))
        .flatten()
        .filter(|check| check.not_comparable.is_none() && check.result.is_ok());
    // A run the validation timeout stopped is compared whole, timeout and all.
    let narrowed = (!run.timed_out || cut.is_some())
        .then(|| failing_tests_selection(full.as_ref(), &on_candidate, claimed_failures))
        .flatten();
    let (replay, check) = match (cached, narrowed) {
        (Some(check), _) => (BaseReplay::CachedFailureSet, check),
        (None, Some(narrowed)) => {
            let check = compare_with_base_until(
                host,
                workspace_path,
                base_sha,
                command,
                Some(&narrowed),
                Some(base_deadline),
            );
            (BaseReplay::FailingTests, check)
        }
        (None, None) => {
            if let Some(reason) = &cut {
                return Ok(checked(
                    BaseFailureVerdict::Cut {
                        reason: format!(
                            "{reason}; its failures cannot be replayed alone on the base, so \
                             none is attributed"
                        ),
                        failures: Vec::new(),
                    },
                    Value::Null,
                    None,
                ));
            }
            let check = compare_with_base_until(
                host,
                workspace_path,
                base_sha,
                command,
                full.as_ref(),
                Some(base_deadline),
            );
            (BaseReplay::FullSelection, check)
        }
    };
    let base_log = check.log(run_id);
    let checked = |verdict: BaseFailureVerdict| checked(verdict, base_log.clone(), Some(replay));
    if let Some(reason) = &check.not_comparable {
        return Ok(checked(BaseFailureVerdict::NotComparable(format!(
            "base {base_sha}'s run of `{}` is not comparable with the candidate's: {reason}",
            run.command
        ))));
    }
    let base = match &check.result {
        Err(reason) => {
            return Ok(checked(BaseFailureVerdict::Inconclusive(format!(
                "base {base_sha} could not be judged: {reason}"
            ))));
        }
        Ok(base) if base.passed => {
            return Ok(checked(BaseFailureVerdict::BasePasses(format!(
                "`{}` fails on the final candidate when the host runs it, and base {base_sha} \
                 passes {}",
                run.command,
                if replay == BaseReplay::FailingTests {
                    "the failing tests"
                } else {
                    "it"
                }
            ))));
        }
        Ok(base) => base,
    };
    let reproduces = match replay {
        BaseReplay::FailingTests => !base.timed_out,
        BaseReplay::CachedFailureSet | BaseReplay::FullSelection => check.reproduces(&run),
    };
    if !reproduces {
        return Ok(checked(BaseFailureVerdict::Contradicted(format!(
            "base {base_sha} fails `{}` differently: exit {}{} there, exit {}{} on the candidate",
            run.command,
            base.exit_code,
            if base.timed_out { " (timed out)" } else { "" },
            run.exit_code,
            if run.timed_out { " (timed out)" } else { "" },
        ))));
    }
    let base_output = normalize_output(&base.output);
    if let Some(missing) = claimed_failures
        .iter()
        .map(|failure| normalize_output(failure))
        .find(|failure| !failure.is_empty() && !base_output.contains(failure.as_str()))
    {
        return Ok(checked(BaseFailureVerdict::Contradicted(format!(
            "the claimed failure `{missing}` is not in base {base_sha}'s output"
        ))));
    }
    let added = on_candidate
        .difference(&base.failures)
        .cloned()
        .collect::<Vec<_>>();
    Ok(checked(match (added.is_empty(), &cut) {
        (false, _) => BaseFailureVerdict::CandidateAdds { failures: added },
        (true, Some(reason)) => BaseFailureVerdict::Cut {
            reason: reason.clone(),
            failures: on_candidate.into_iter().collect(),
        },
        (true, None) if known_red(&run, &base.failures) => BaseFailureVerdict::KnownRed {
            failures: on_candidate.into_iter().collect(),
        },
        (true, None) => BaseFailureVerdict::Reproduced {
            failures: on_candidate.into_iter().collect(),
        },
    }))
}

/// [ORB-15427] A claimed leaf's report that `command` failed on its
/// candidate, with `exit_code`, only with `failures`, every one of which
/// also fails on `base_sha`, and the owner's own base it must stand on.
#[derive(Debug, Clone, Copy)]
pub struct ClaimedKnownRed<'a> {
    /// The base branch this owner captured for the claim's admission.
    pub base_branch: &'a str,
    /// `remote` to read that branch as `origin/<base_branch>` after a fetch,
    /// `local` for this checkout's own branch.
    pub base_sync: BaseSyncMode,
    /// The base the leaf compared with: the handed-off candidate's base.
    pub base_sha: &'a str,
    /// The owner-required command the log reports.
    pub command: &'a str,
    /// What the candidate run reported testing.
    pub selection: Option<&'a Value>,
    /// The candidate run's exit status.
    pub exit_code: i32,
    /// Every failure the candidate run named.
    pub failures: &'a BTreeSet<String>,
}

/// [ORB-15566] Why the owner did not confirm a claimed leaf's known-red
/// report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnownRedUnconfirmed {
    /// The owner's own comparable, completed base run contradicts the report:
    /// it passes the reported tests, fails without one of them, or fails a
    /// whole run with another exit status. Asking again reads the same base
    /// result, so the report can never be confirmed.
    Contradicted(String),
    /// The owner could not establish it either way: no base run in time, a
    /// run it could not compare, or a report it does not judge.
    Inconclusive(String),
}

impl KnownRedUnconfirmed {
    /// Why, in prose.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Contradicted(reason) | Self::Inconclusive(reason) => reason,
        }
    }
}

/// [ORB-15427] The owner's check of a claimed leaf's known-red `report`.
/// Returns the reported failures when this host's own run of the base fails
/// them all, or why it could not establish that, typed by whether its own
/// base run contradicts the report [ORB-15566].
///
/// The leaf's base run is never taken on trust, and neither is its choice of
/// base: the base must be in the history of this owner's own base branch,
/// or its failures say nothing about the integration branch, and nothing
/// runs. Then this runs on the owner, through the owner's base result cache.
/// A comparable full-selection result already cached for
/// `(base, command, selection)` is read without running anything.
/// Otherwise, when the leaf reported a selection and every failure is a
/// test, the base runs only those tests, as a reviewer's base claim does;
/// otherwise it runs the whole selection. The known-red rule the owner's own
/// validation applies judges the result; a whole run must also fail with the
/// candidate's exit status.
///
/// The caller shares one absolute `deadline` across settlement's commands.
/// Lock waits and commands stop there. An inconclusive attempted run is retained
/// separately from conclusive base results, so retries reuse the refusal without
/// treating partial output as confirmation. Ordinary validation ignores it.
///
/// Only call this with a command the owner itself requires: it runs on the
/// owner's host, outside any agent sandbox.
pub fn verify_claimed_known_red<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    report: &ClaimedKnownRed<'_>,
    deadline: Instant,
) -> Result<BTreeSet<String>, KnownRedUnconfirmed> {
    use KnownRedUnconfirmed::{Contradicted, Inconclusive};
    let ClaimedKnownRed {
        base_branch,
        base_sync,
        base_sha,
        command,
        selection,
        exit_code,
        failures,
    } = *report;
    if failures.is_empty() {
        return Err(Inconclusive(
            "the report names no failure to compare".to_string(),
        ));
    }
    if Instant::now() >= deadline {
        return Err(Inconclusive(
            "the owner settlement base comparison deadline expired".into(),
        ));
    }
    on_owner_base(workspace_path, base_branch, base_sync, base_sha).map_err(Inconclusive)?;
    let cached = cached_base_check(workspace_path, base_sha, command, selection)
        .filter(|check| check.not_comparable.is_none() && check.result.is_ok());
    let (check, whole) = match (cached, failing_tests_selection(selection, failures, &[])) {
        (Some(check), _) => (check, true),
        (None, Some(narrowed)) => (
            compare_claimed_with_base_until(
                host,
                workspace_path,
                base_sha,
                command,
                Some(&narrowed),
                deadline,
            ),
            false,
        ),
        (None, None) => (
            compare_claimed_with_base_until(
                host,
                workspace_path,
                base_sha,
                command,
                selection,
                deadline,
            ),
            true,
        ),
    };
    if check.fails_all(whole.then_some(exit_code), failures) {
        return Ok(failures.clone());
    }
    let ran = if whole { "it" } else { "the failing tests" };
    let missing = |base: &BaseCommandResult| {
        failures
            .difference(&base.failures)
            .map(|failure| format!("`{failure}`"))
            .collect::<Vec<_>>()
    };
    Err(match &check.result {
        Err(reason) => Inconclusive(format!("base {base_sha} could not be judged: {reason}")),
        Ok(_) if check.not_comparable.is_some() => Inconclusive(format!(
            "the base run of {ran} is not comparable with the candidate's: {}",
            check.not_comparable.as_deref().unwrap_or_default()
        )),
        Ok(base) if base.timed_out => Inconclusive(format!("the base run of {ran} timed out")),
        Ok(base) if base.passed => Contradicted(format!("base {base_sha} passes {ran}")),
        Ok(base) if whole && base.exit_code != exit_code => Contradicted(format!(
            "base {base_sha} fails with exit {} where the candidate failed with exit {exit_code}",
            base.exit_code
        )),
        Ok(base) if !missing(base).is_empty() => Contradicted(format!(
            "base {base_sha} does not fail {}",
            missing(base).join(", ")
        )),
        // The base fails every reported failure, so the report itself is
        // what falls short: it names something other than failed tests.
        Ok(_) => Inconclusive(format!(
            "the report names failures other than failed tests, which base {base_sha} cannot \
             be shown to share"
        )),
    })
}

/// `Ok` when `base_sha` is in the history of this owner's `base_branch`, read
/// as `base_sync` names it [ORB-15427]; otherwise why not. A follower names
/// the base it synchronized onto, and the owner only checked that the
/// candidate descends from it, so a commit off the integration branch would
/// make the candidate's own regressions look like the base's.
fn on_owner_base(
    workspace_path: &Path,
    base_branch: &str,
    base_sync: BaseSyncMode,
    base_sha: &str,
) -> Result<(), String> {
    let tip = resolve_worktree_start_point(workspace_path, base_branch, base_sync)
        .map_err(|error| format!("this owner could not read its base '{base_branch}': {error}"))?;
    match git_command_success(
        workspace_path,
        &[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            base_sha,
            &tip,
        ],
    ) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "base {base_sha} is not in the history of this owner's {tip}, so its failures are \
             not the integration branch's"
        )),
        Err(error) => Err(format!(
            "this owner could not place base {base_sha} in {tip}: {error}"
        )),
    }
}

/// `selection` with [`FAILING_TESTS_KEY`] naming the tests to replay on the
/// base, or `None` when the candidate run reported no selection or a failure
/// it names is not a test a run can be handed by name.
pub(super) fn failing_tests_selection(
    selection: Option<&Value>,
    failures: &BTreeSet<String>,
    claimed: &[String],
) -> Option<Value> {
    let Some(Value::Object(selection)) = selection else {
        return None;
    };
    let mut tests = failing_tests(failures)?;
    // A test the reviewer named must be in the base's output, so it is
    // replayed too.
    let claimed = claimed
        .iter()
        .map(|failure| failure.trim())
        .map(|failure| failure.strip_prefix("test ").unwrap_or(failure))
        .filter(|name| test_id(name));
    tests.extend(claimed.map(str::to_string));
    let mut narrowed = selection.clone();
    narrowed.insert(FAILING_TESTS_KEY.to_string(), json!(tests));
    Some(Value::Object(narrowed))
}

/// The tests to replay for `failures`, in `failure_identities` form without
/// the `test ` prefix: nextest's `<binary-id> <test>` or a bare libtest name.
/// `None` when there are none, or one failure is not such a test.
fn failing_tests(failures: &BTreeSet<String>) -> Option<BTreeSet<String>> {
    let names = failures
        .iter()
        .map(|failure| failure.strip_prefix("test ").filter(|name| test_id(name)))
        .collect::<Option<Vec<_>>>()?;
    // nextest also echoes each failed test's libtest line, whose bare name
    // the qualified identity already covers.
    let qualified = names
        .iter()
        .filter_map(|name| name.split_once(' ').map(|(_, test)| test))
        .collect::<BTreeSet<_>>();
    let tests = names
        .into_iter()
        .filter(|name| name.contains(' ') || !qualified.contains(name))
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    (!tests.is_empty()).then_some(tests)
}

/// Whether `name` is a test a runner can be handed by name: an optional
/// nextest binary id (`crate`, `crate::target`, `crate::bin/name`), then a
/// Rust test path.
fn test_id(name: &str) -> bool {
    static PATTERN: OnceLock<Option<Regex>> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            Regex::new(
                r"^(?:[A-Za-z0-9_-]+(?:::[A-Za-z0-9_./-]+)? )?[A-Za-z0-9_]+(?:::[A-Za-z0-9_]+)*$",
            )
            .ok()
        })
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(name))
}

/// Why a passing run shows nothing about the disputed check, when it does
/// not [ORB-15122]: a run that reports a summary must have executed a counted
/// test, by the rule a base run's pass is held to ([`not_comparable`]). A
/// command that reports no summary is judged by its exit status.
fn executed_no_tests(summary: Option<&ValidationSummary>) -> Option<String> {
    match summary?.tests_run {
        Some(0) => Some("passed without executing a test".to_string()),
        None => Some("passed without reporting how many tests it executed".to_string()),
        Some(_) => None,
    }
}

/// Whitespace runs collapsed and color codes dropped, so two captures of one
/// failure compare equal.
pub(super) fn normalize_output(text: &str) -> String {
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
/// Required validation runs read them from the complete streams before
/// capture bounds the text ([`super::required_command`]) [ORB-15375].
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
