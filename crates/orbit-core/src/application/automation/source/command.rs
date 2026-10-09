//! Bounded child processes and git plumbing for one source pass.

use super::super::source_cache::SourceCache;
use chrono::{DateTime, Utc};
use orbit_automation::AutomationError;
use std::{
    io::{BufRead, Read, Seek, SeekFrom, Write},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(test)]
use super::revision::source_deadline_forced;
use super::{SOURCE_DEADLINE, Source};

/// Budget for one local git or provider command. A fetch uses the time left
/// on [`SOURCE_DEADLINE`] instead: two seconds is not a network fetch.
const COMMAND_BUDGET: Duration = Duration::from_secs(2);

/// Largest stdout one command's evidence may carry.
const OUTPUT_LIMIT: u64 = 1_048_576;
/// Largest listing [`Source::git_matching_paths`] streams through a filter.
/// Only the kept paths are held in memory, so this bounds disk, not evidence.
const LISTING_LIMIT: u64 = 256 * 1_048_576;

/// A source command owns its descendants as well as its direct child. Drop
/// cleans up on timeout, I/O failure and a leader exiting before its helpers.
struct SourceChild(Child);

impl Drop for SourceChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(group) = i32::try_from(self.0.id()) {
            // SAFETY: this child was made a group leader with process_group(0).
            // No caller memory is accessed; never signal our own group or a
            // reused PID that now belongs to a different process group.
            unsafe {
                let current = libc::getpgid(group);
                if group > 1 && group != libc::getpgrp() && (current < 0 || current == group) {
                    libc::killpg(group, libc::SIGKILL);
                }
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl<'a> Source<'a> {
    pub(crate) fn new(root: &'a Path) -> Self {
        Self::at(root, Utc::now())
    }

    pub(crate) fn at(root: &'a Path, now: DateTime<Utc>) -> Self {
        Self {
            root,
            started: Instant::now(),
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
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;

        let mut result = String::new();
        file.take(OUTPUT_LIMIT + 1)
            .read_to_string(&mut result)
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;

        if result.len() as u64 > OUTPUT_LIMIT {
            return Err(AutomationError::Deferred("source_budget".into()));
        }

        Ok(if trim_output {
            result.trim().into()
        } else {
            result
        })
    }

    /// Run one child process to completion with stdout in an unlinked temp
    /// file, killing it once the file passes `limit` bytes or `budget` /
    /// [`SOURCE_DEADLINE`] elapses.
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
            return Err(AutomationError::Deferred("source_deadline".into()));
        }

        #[cfg(test)]
        if source_deadline_forced(self) {
            return Err(AutomationError::Deferred("source_deadline".into()));
        }

        if self.started.elapsed() > SOURCE_DEADLINE {
            return Err(AutomationError::Deferred("source_deadline".into()));
        }

        let stdin = match input {
            None => Stdio::null(),
            Some(bytes) => {
                let mut stdin =
                    tempfile::tempfile().map_err(|e| AutomationError::Deferred(e.to_string()))?;
                stdin
                    .write_all(bytes)
                    .and_then(|()| stdin.seek(SeekFrom::Start(0)).map(drop))
                    .map_err(|e| AutomationError::Deferred(e.to_string()))?;
                Stdio::from(stdin)
            }
        };

        let file = tempfile::tempfile().map_err(|e| AutomationError::Deferred(e.to_string()))?;
        let output = file
            .try_clone()
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;
        let mut diagnostics =
            tempfile::tempfile().map_err(|e| AutomationError::Deferred(e.to_string()))?;
        let errors = diagnostics
            .try_clone()
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;
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
        let mut child = SourceChild(
            command
                .current_dir(self.root)
                .stdin(stdin)
                .stdout(output)
                .stderr(errors)
                .spawn()
                .map_err(|e| {
                    AutomationError::Deferred(format!(
                        "source_spawn_failed: {}: {e}",
                        command_line(program, args)
                    ))
                })?,
        );

        let start = Instant::now();

        loop {
            if let Some(status) = child
                .0
                .try_wait()
                .map_err(|e| AutomationError::Deferred(e.to_string()))?
            {
                if !status.success() {
                    return Err(AutomationError::Deferred(format!(
                        "evidence_unavailable: {}: {}",
                        command_line(program, args),
                        failure_text(&mut diagnostics, status)
                    )));
                }
                break;
            }

            let over_budget = start.elapsed() > budget
                || self.started.elapsed() > SOURCE_DEADLINE
                || file
                    .metadata()
                    .map(|meta| meta.len() > limit)
                    .unwrap_or(true);

            if over_budget {
                return Err(AutomationError::Deferred("source_budget".into()));
            }

            std::thread::sleep(Duration::from_millis(10));
        }

        // A child that exits between the last poll and here may have written
        // past the limit.
        if file
            .metadata()
            .map(|meta| meta.len() > limit)
            .unwrap_or(true)
        {
            return Err(AutomationError::Deferred("source_budget".into()));
        }

        Ok(file)
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
        let budget = match SOURCE_DEADLINE.checked_sub(self.started.elapsed()) {
            Some(budget) if !budget.is_zero() => budget,
            _ => return Err(AutomationError::Deferred("source_deadline".into())),
        };
        let mut file = self.capture("git", args, &[], None, budget, LISTING_LIMIT)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;

        let mut paths = Vec::new();
        let mut record = Vec::new();
        let mut reader = std::io::BufReader::new(file);
        loop {
            record.clear();
            let read = reader
                .read_until(0, &mut record)
                .map_err(|e| AutomationError::Deferred(e.to_string()))?;
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

    pub(crate) fn git(&self, args: &[&str]) -> Result<String, AutomationError> {
        self.command("git", args)
    }

    /// [`Self::git`] without trimming stdout. Use when leading or trailing
    /// bytes are data: a NUL-delimited path list, or instruction file content.
    pub(crate) fn git_preserving_output(&self, args: &[&str]) -> Result<String, AutomationError> {
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
        let budget = match SOURCE_DEADLINE.checked_sub(self.started.elapsed()) {
            Some(budget) if !budget.is_zero() => budget,
            _ => return Err(AutomationError::Deferred("source_deadline".into())),
        };
        self.command_with("git", args, &[], input, budget)
    }
}

/// Bytes of a child's stderr kept for a deferral reason.
const FAILURE_TEXT_LIMIT: u64 = 4096;

fn command_line(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first stderr line of a failed child, or its exit status when it said
/// nothing. Bounded so a chatty tool cannot turn a reason into a log.
fn failure_text(diagnostics: &mut std::fs::File, status: std::process::ExitStatus) -> String {
    let mut text = String::new();
    if diagnostics.seek(SeekFrom::Start(0)).is_ok() {
        let _ = diagnostics
            .take(FAILURE_TEXT_LIMIT)
            .read_to_string(&mut text);
    }

    match text.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => line.to_string(),
        None => status.to_string(),
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
