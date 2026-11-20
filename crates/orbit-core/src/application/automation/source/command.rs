//! Bounded child processes and git plumbing for one source pass.

use super::super::source_cache::SourceCache;
use chrono::{DateTime, Utc};
use orbit_automation::{AutomationError, DeferredReason};
use orbit_common::process::supervisor::{
    Outcome, Periodic, ProcessStopCause, Stdin, Stream, SupervisedChild, Supervision, Termination,
};
use std::{
    io::{BufRead, Read, Seek, SeekFrom, Write},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

#[cfg(test)]
use super::revision::source_deadline_forced;
use super::{BudgetClock, SOURCE_DEADLINE, Source};

/// Budget for one local git or provider command. A fetch uses the time left
/// on [`SOURCE_DEADLINE`] instead: two seconds is not a network fetch.
const COMMAND_BUDGET: Duration = Duration::from_secs(2);

/// Largest stdout one command's evidence may carry.
pub(in crate::application::automation) const OUTPUT_LIMIT: u64 = 1_048_576;
/// How often supervision checks that stdout file. The previous poll slept this
/// long between size and deadline checks.
const OUTPUT_LIMIT_POLL: Duration = Duration::from_millis(10);
/// Largest listing [`Source::git_matching_paths`] streams through a filter.
/// Only the kept paths are held in memory, so this bounds disk, not evidence.
pub(in crate::application::automation) const LISTING_LIMIT: u64 = 256 * 1_048_576;

impl<'a> Source<'a> {
    pub(crate) fn new(root: &'a Path) -> Self {
        Self::at(root, Utc::now())
    }

    pub(crate) fn at(root: &'a Path, now: DateTime<Utc>) -> Self {
        Self {
            root,
            started: BudgetClock::start(),
            cache: None,
            now,
            fetch_origin: true,
        }
    }

    /// Inspect evidence against existing refs without fetching origin. Missing
    /// tracking refs stay deferred; the local branch is not a substitute.
    pub(in crate::application::automation) fn read_only(root: &'a Path) -> Self {
        Self {
            fetch_origin: false,
            ..Self::new(root)
        }
    }

    pub(in crate::application::automation) fn read_only_with_cache(
        root: &'a Path,
        cache: &'a SourceCache,
    ) -> Self {
        Self {
            cache: Some(cache),
            ..Self::read_only(root)
        }
    }

    pub(crate) fn with_cache(root: &'a Path, cache: &'a SourceCache, now: DateTime<Utc>) -> Self {
        Self {
            cache: Some(cache),
            now,
            ..Self::new(root)
        }
    }

    /// Run one bounded child process, capturing stdout under a size and time budget.
    ///
    /// A failing command defers with its command line and the first line of
    /// its stderr, so an operator reading a sweep row or `auto-task show`
    /// learns which ref git could not resolve instead of a bare token.
    pub(super) fn command(&self, program: &str, args: &[&str]) -> Result<String, AutomationError> {
        self.command_with(program, args, &[], None, COMMAND_BUDGET)
    }

    pub(super) fn command_with(
        &self,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        input: Option<&[u8]>,
        budget: Duration,
    ) -> Result<String, AutomationError> {
        self.command_with_output(program, args, env, input, budget, true)
    }

    /// `command_with`, optionally preserving stdout exactly. Trimming is right
    /// for a single revision or ref, and wrong for a NUL-delimited path list
    /// or instruction bytes whose leading or trailing whitespace is content.
    fn command_with_output(
        &self,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        input: Option<&[u8]>,
        budget: Duration,
        trim_output: bool,
    ) -> Result<String, AutomationError> {
        let mut file = self.capture(program, args, env, input, budget, OUTPUT_LIMIT)?;

        file.seek(SeekFrom::Start(0))
            .map_err(|e| AutomationError::Deferred(DeferredReason::SourceIo(e.into())))?;

        let mut result = String::new();
        file.take(OUTPUT_LIMIT + 1)
            .read_to_string(&mut result)
            .map_err(|e| AutomationError::Deferred(DeferredReason::SourceIo(e.into())))?;

        if result.len() as u64 > OUTPUT_LIMIT {
            return Err(AutomationError::Deferred(DeferredReason::SourceBudget));
        }

        Ok(if trim_output {
            result.trim().into()
        } else {
            result
        })
    }

    /// Run one child process to completion with stdout in an unlinked temp
    /// file. The supervisor waits, applies `budget` and the time left on
    /// [`SOURCE_DEADLINE`], and kills the child at once. Crossing `limit`
    /// bytes is a periodic check. The caller still checks the file after exit.
    fn capture(
        &self,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        input: Option<&[u8]>,
        budget: Duration,
        limit: u64,
    ) -> Result<std::fs::File, AutomationError> {
        // Test seam for ORB-14356: the next `diff-tree` fails as a deadline
        // once, so a swallowed canonical signature is distinguishable from a
        // propagated one. Production builds do not include the seam.
        #[cfg(test)]
        if canonical_signature_deadline_armed(args) {
            return Err(AutomationError::Deferred(DeferredReason::SourceDeadline));
        }

        #[cfg(test)]
        if source_deadline_forced(self) {
            return Err(AutomationError::Deferred(DeferredReason::SourceDeadline));
        }

        let remaining_source_before = SOURCE_DEADLINE
            .checked_sub(self.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(AutomationError::Deferred(DeferredReason::SourceDeadline))?;

        #[cfg(feature = "test-support")]
        let frozen_clock = self.started.is_frozen();
        #[cfg(not(feature = "test-support"))]
        let frozen_clock = false;

        // A provider read spends the host's GitHub quota as background work:
        // a read the guard refuses defers this pass and runs nothing. Bound any
        // rate-limit probe to what remains of this command and source budget,
        // and re-check both before spawning the provider command.
        let mut quota_request = gh_quota_request(program, args, env, self.root);
        // The instant the request's finite budget runs out, so a throttle the
        // child reports after the wait is recorded inside that budget.
        let mut quota_deadline = None;
        let effective_budget = if let Some(request) = quota_request.as_mut() {
            if !frozen_clock {
                let probe_budget = budget.min(remaining_source_before);
                let probe_ms = u64::try_from(probe_budget.as_millis())
                    .unwrap_or(u64::MAX)
                    .max(1);
                request.timeout_ms = Some(probe_ms);
            }
            let command_started = super::budget_now();
            let wall_started = std::time::Instant::now();
            quota_deadline = orbit_tools::github_quota::gh_request_deadline(request, wall_started);
            let admitted = orbit_tools::github_quota::admit_gh_request(
                orbit_tools::github_quota::GithubCaller::Background,
                request,
            );
            let wall_elapsed = wall_started.elapsed();
            let command_elapsed = super::budget_now()
                .saturating_duration_since(command_started)
                .max(wall_elapsed);
            if self.elapsed() >= SOURCE_DEADLINE
                || (!frozen_clock && wall_elapsed >= remaining_source_before)
            {
                return Err(AutomationError::Deferred(DeferredReason::SourceDeadline));
            }
            admitted.map_err(|error| {
                AutomationError::Deferred(DeferredReason::EvidenceUnavailable(format!(
                    "{}: {error}",
                    command_line(program, args)
                )))
            })?;
            let remaining_command_budget = budget.saturating_sub(command_elapsed);
            if !frozen_clock && remaining_command_budget.is_zero() {
                return Err(AutomationError::Deferred(DeferredReason::SourceBudget));
            }
            if frozen_clock {
                budget
            } else {
                let remaining_source_after = SOURCE_DEADLINE
                    .saturating_sub(self.elapsed())
                    .min(remaining_source_before.saturating_sub(wall_elapsed));
                if remaining_source_after.is_zero() {
                    return Err(AutomationError::Deferred(DeferredReason::SourceDeadline));
                }
                remaining_command_budget.min(remaining_source_after)
            }
        } else {
            budget
        };

        let stdin = match input {
            None => Stdio::null(),
            Some(bytes) => {
                let mut stdin = tempfile::tempfile()
                    .map_err(|e| AutomationError::Deferred(DeferredReason::SourceIo(e.into())))?;
                stdin
                    .write_all(bytes)
                    .and_then(|()| stdin.seek(SeekFrom::Start(0)).map(drop))
                    .map_err(|e| AutomationError::Deferred(DeferredReason::SourceIo(e.into())))?;
                Stdio::from(stdin)
            }
        };

        let file = tempfile::tempfile()
            .map_err(|e| AutomationError::Deferred(DeferredReason::SourceIo(e.into())))?;
        let output = file
            .try_clone()
            .map_err(|e| AutomationError::Deferred(DeferredReason::SourceIo(e.into())))?;
        let mut diagnostics = tempfile::tempfile()
            .map_err(|e| AutomationError::Deferred(DeferredReason::SourceIo(e.into())))?;
        let errors = diagnostics
            .try_clone()
            .map_err(|e| AutomationError::Deferred(DeferredReason::SourceIo(e.into())))?;
        // Keep each value as a separate OS argument. This avoids treating the
        // collected values as a command-line string while retaining Git's
        // normal argument semantics.
        let mut command = Command::new(program);
        for arg in args {
            command.arg(arg);
        }
        for (key, value) in env {
            command.env(key, value);
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command
            .current_dir(self.root)
            .stdin(stdin)
            .stdout(output)
            .stderr(errors)
            .spawn()
            .map_err(|e| {
                AutomationError::Deferred(DeferredReason::SourceSpawnFailed(format!(
                    "{}: {e}",
                    command_line(program, args)
                )))
            })?;

        let mut over_limit = || {
            let over = file
                .metadata()
                .map(|meta| meta.len() > limit)
                .unwrap_or(true);
            over.then_some(ProcessStopCause::Periodic)
        };
        let outcome = SupervisedChild::adopt(child)
            .supervise(Supervision {
                stdin: Stdin::Null,
                stdout: Stream::Redirected,
                stderr: Stream::Redirected,
                timeout: self.supervision_timeout(effective_budget),
                // The source clock, so a frozen or injected test clock does
                // not start spending the wall-clock budget.
                clock: Some(&super::budget_now),
                every: Some(Periodic {
                    interval: OUTPUT_LIMIT_POLL,
                    run: &mut over_limit,
                }),
                termination: Termination::KillNow,
                cancelled: None,
                intercept_signals: false,
                drain_budget: Duration::ZERO,
                cap_drain_at_deadline: false,
                require_eof: false,
                stopped_descendants: None,
                on_spawn: None,
                on_stopped_descendant: None,
                extendable_deadline: None,
            })
            .map_err(|error| {
                AutomationError::Deferred(DeferredReason::SourceSupervision(error.into()))
            })?;

        if outcome.timed_out || stopped_periodically(&outcome) {
            return Err(AutomationError::Deferred(DeferredReason::SourceBudget));
        }
        if !outcome.success {
            let failure = failure_text(&mut diagnostics, &failure_fallback(&outcome));
            if let Some(request) = &quota_request {
                orbit_tools::github_quota::observe_gh_failure_by_deadline(
                    request,
                    &failure,
                    quota_deadline,
                );
            }
            return Err(AutomationError::Deferred(
                DeferredReason::EvidenceUnavailable(format!(
                    "{}: {failure}",
                    command_line(program, args),
                )),
            ));
        }

        // A child that exits between the last check and here may have written
        // past the limit.
        if file
            .metadata()
            .map(|meta| meta.len() > limit)
            .unwrap_or(true)
        {
            return Err(AutomationError::Deferred(DeferredReason::SourceBudget));
        }

        Ok(file)
    }

    /// Deadline passed to the supervisor for one command.
    ///
    /// A frozen test clock does not advance, so the wall clock must not spend
    /// the budget: the timeout is already exhausted, or there is none. A live
    /// clock gets the shorter of the command budget and the time left on
    /// [`SOURCE_DEADLINE`], measured from the start of supervision.
    fn supervision_timeout(&self, budget: Duration) -> Option<Duration> {
        #[cfg(feature = "test-support")]
        if self.started.is_frozen() {
            let frozen = self.started.restart().elapsed();
            if frozen > budget || self.elapsed() > SOURCE_DEADLINE {
                return Some(Duration::ZERO);
            }
            return None;
        }
        Some(budget.min(SOURCE_DEADLINE.saturating_sub(self.elapsed())))
    }

    /// The NUL-delimited paths of a git listing for which `keep` is true, in
    /// listing order. The listing is streamed from disk and filtered, so only
    /// the matches are held in memory and the evidence cap on one command's
    /// output does not apply to the whole listing — a large tree lists far
    /// more paths than any caller keeps. The listing runs on the time left on
    /// this source's deadline, like a replay.
    pub(crate) fn git_matching_paths(
        &self,
        args: &[&str],
        keep: impl Fn(&str) -> bool,
    ) -> Result<Vec<String>, AutomationError> {
        let budget = match SOURCE_DEADLINE.checked_sub(self.elapsed()) {
            Some(budget) if !budget.is_zero() => budget,
            _ => return Err(AutomationError::Deferred(DeferredReason::SourceDeadline)),
        };
        let mut file = self.capture("git", args, &[], None, budget, LISTING_LIMIT)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| AutomationError::Deferred(DeferredReason::SourceIo(e.into())))?;

        let mut paths = Vec::new();
        let mut record = Vec::new();
        let mut reader = std::io::BufReader::new(file);
        loop {
            record.clear();
            let read = reader
                .read_until(0, &mut record)
                .map_err(|e| AutomationError::Deferred(DeferredReason::SourceIo(e.into())))?;
            if read == 0 {
                break;
            }
            if record.last() == Some(&0) {
                record.pop();
            }
            let path = std::str::from_utf8(&record)
                .map_err(|e| AutomationError::Evidence(e.to_string()))?;
            if !path.is_empty() && keep(path) {
                paths.push(path.to_string());
            }
        }
        Ok(paths)
    }

    /// Read an operator-named cover through the bounded provider transport.
    pub(crate) fn ci_cover_pull_request(
        &self,
        reference: &str,
    ) -> Result<serde_json::Value, AutomationError> {
        let output = self.command(
            "gh",
            &["pr", "view", reference, "--json", "state,mergeCommit,url"],
        )?;
        serde_json::from_str(&output).map_err(|error| {
            AutomationError::Deferred(DeferredReason::ProviderResponseDecode(error.into()))
        })
    }

    pub(crate) fn git(&self, args: &[&str]) -> Result<String, AutomationError> {
        self.command("git", args)
    }

    /// [`Self::git`] without trimming stdout. Use when leading or trailing
    /// bytes are data: a NUL-delimited path list, or instruction file content.
    pub(in crate::application::automation) fn git_preserving_output(
        &self,
        args: &[&str],
    ) -> Result<String, AutomationError> {
        self.command_with_output("git", args, &[], None, COMMAND_BUDGET, false)
    }

    /// [`Self::git`] with `input` on standard input, under the same budget.
    pub(crate) fn git_with_input(
        &self,
        args: &[&str],
        input: &[u8],
    ) -> Result<String, AutomationError> {
        self.command_with("git", args, &[], Some(input), COMMAND_BUDGET)
    }

    /// One git command for the replay proof. A batched range can outlive the
    /// two-second local-command budget, so this uses the time left on this
    /// source's deadline — the same rule as a fetch.
    pub(super) fn git_replay(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
    ) -> Result<String, AutomationError> {
        let budget = match SOURCE_DEADLINE.checked_sub(self.elapsed()) {
            Some(budget) if !budget.is_zero() => budget,
            _ => return Err(AutomationError::Deferred(DeferredReason::SourceDeadline)),
        };
        self.command_with("git", args, &[], input, budget)
    }
}

/// Bytes of a child's stderr kept for a deferral reason.
const FAILURE_TEXT_LIMIT: u64 = 4096;

/// The request the host's GitHub quota guard reads for a `gh` command: the
/// child inherits this process's environment plus `env`, in `root`. `None`
/// for any other program.
fn gh_quota_request(
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
    root: &Path,
) -> Option<orbit_exec::ExecRequest> {
    if program != "gh" {
        return None;
    }
    let environment_mode = if env.is_empty() {
        orbit_exec::EnvironmentMode::Inherit
    } else {
        let mut merged = std::env::vars().collect::<Vec<_>>();
        for (key, value) in env {
            merged.retain(|(name, _)| name != key);
            merged.push(((*key).to_string(), (*value).to_string()));
        }
        orbit_exec::EnvironmentMode::ClearAndSet(merged)
    };
    Some(orbit_exec::ExecRequest {
        program: program.to_string(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        current_dir: Some(root.to_string_lossy().into_owned()),
        timeout_ms: None,
        stdin_mode: orbit_exec::StdinMode::Null,
        environment_mode,
        debug: false,
    })
}

fn command_line(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

fn stopped_periodically(outcome: &Outcome) -> bool {
    outcome
        .stop_causes
        .iter()
        .any(|cause| matches!(cause, ProcessStopCause::Periodic))
}

/// Exit status text for a child that wrote no stderr. Matches `ExitStatus`'s
/// display for a normal code, and keeps the signal status the supervisor
/// recorded when there is no code.
fn failure_fallback(outcome: &Outcome) -> String {
    outcome
        .stop_causes
        .iter()
        .find_map(|cause| match cause {
            ProcessStopCause::TerminationStatus { status } => Some(status.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| match outcome.exit_code {
            Some(code) => format!("exit status: {code}"),
            None => "exit status: unknown".to_string(),
        })
}

/// The first stderr line of a failed child, or its exit status when it said
/// nothing. Bounded so a chatty tool cannot turn a reason into a log.
fn failure_text(diagnostics: &mut std::fs::File, fallback: &str) -> String {
    let mut text = String::new();
    if diagnostics.seek(SeekFrom::Start(0)).is_ok() {
        let _ = diagnostics
            .take(FAILURE_TEXT_LIMIT)
            .read_to_string(&mut text);
    }

    match text.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => line.to_string(),
        None => fallback.to_string(),
    }
}

#[cfg(test)]
thread_local! {
    static CANONICAL_SIGNATURE_DEADLINE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Fail the next `diff-tree` once with `source_deadline`.
///
/// The canonical pass runs that command first. Swallowing the failure lets
/// the orphan pass succeed and report an ambiguous mapping instead.
#[cfg(test)]
pub(in crate::application::automation) fn arm_canonical_signature_deadline() {
    CANONICAL_SIGNATURE_DEADLINE.with(|armed| armed.set(true));
}

#[cfg(test)]
pub(in crate::application::automation) fn clear_canonical_signature_deadline() {
    CANONICAL_SIGNATURE_DEADLINE.with(|armed| armed.set(false));
}

#[cfg(test)]
fn canonical_signature_deadline_armed(args: &[&str]) -> bool {
    if !args.contains(&"diff-tree") {
        return false;
    }
    CANONICAL_SIGNATURE_DEADLINE.with(|armed| armed.replace(false))
}
