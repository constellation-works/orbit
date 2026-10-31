//! Consecutive-failure streaks per job, for the dashboard's failing-job warning.
//!
//! A streak is the newest finished runs of one job whose state is `failed` or
//! `interrupted`, read newest first until the first `success`. Operator cancels,
//! designed holds, timeouts and skips are neither failures nor successes: they
//! are not in the query, so they neither extend nor break a streak. That is the
//! failure set the daily run-failure-patterns pass counts, so the two views
//! agree. A streak only describes; nothing here pauses or changes a job.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{JobRunOrder, JobRunQuery};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::root_cause::RunFailureContextProfile;

/// Newest finished runs read per job. A streak that fills it is reported as
/// "at least" that long, because an older success could still be unseen.
const STREAK_SCAN_LIMIT: usize = 100;
/// Failed runs of one streak whose errors are read to decide whether they
/// share a signature; reading a run's error opens its step records.
const SIGNATURE_RUN_LIMIT: usize = 25;
/// Longest latest-error line returned.
const LATEST_ERROR_CHARS: usize = 200;

/// `jobs.failure_streak_warn`: the streak length at which the dashboard warns.
const WARN_KEY: &str = "jobs.failure_streak_warn";
/// The key's built-in default, used only when the config cannot be loaded.
const DEFAULT_WARN: u32 = 5;

/// A job's current run of consecutive failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobFailureStreak {
    pub job_id: String,
    /// Failed runs in a row, newest first. At least this many when `truncated`.
    pub count: usize,
    /// When the oldest run of the streak was created.
    pub since: DateTime<Utc>,
    /// The newest failed run.
    pub latest_run_id: String,
    /// The newest run's error cut to one line, when it recorded one.
    pub latest_error: Option<String>,
    /// Whether every inspected run in the streak shares one error signature.
    pub same_error: bool,
    /// The scan ended inside the streak, so it may be longer.
    pub truncated: bool,
}

impl JobFailureStreak {
    /// The dashboard's `failure_streak` object.
    pub fn to_json(&self) -> Value {
        json!({
            "job_id": self.job_id,
            "count": self.count,
            "since": self.since.to_rfc3339(),
            "latest_run_id": self.latest_run_id,
            "latest_error": self.latest_error,
            "same_error": self.same_error,
            "truncated": self.truncated,
        })
    }
}

impl OrbitRuntime {
    /// `jobs.failure_streak_warn` from the effective config: the streak
    /// length that raises the warning, `0` when the warning is off. An
    /// unreadable config reads as the default so a bad file cannot hide it.
    pub fn job_failure_streak_warn(&self) -> u32 {
        orbit_config::load_effective_config(&orbit_config::ConfigRoots::new(
            self.global_root(),
            self.shared_root(),
        ))
        .ok()
        .and_then(|config| {
            config
                .values()
                .iter()
                .find(|entry| entry.key == WARN_KEY)
                .and_then(|entry| entry.value.as_u64())
        })
        .map_or(DEFAULT_WARN, |value| {
            u32::try_from(value).unwrap_or(u32::MAX)
        })
    }

    /// The current failure streak of every job in `job_ids` whose newest
    /// finished run failed, in `job_ids` order. Reads recorded runs without
    /// reconciling them.
    pub fn job_failure_streaks(
        &self,
        job_ids: &[String],
    ) -> Result<Vec<JobFailureStreak>, OrbitError> {
        let mut streaks = Vec::new();
        for job_id in job_ids {
            let runs = self.list_job_runs_filtered_backend(&JobRunQuery {
                job_id: Some(job_id.clone()),
                states: vec![
                    JobRunState::Success,
                    JobRunState::Failed,
                    JobRunState::Interrupted,
                ],
                limit: Some(STREAK_SCAN_LIMIT),
                order_by: JobRunOrder::Recency,
                include_steps: false,
                ..JobRunQuery::default()
            })?;
            let streak: Vec<&JobRun> = runs
                .iter()
                .take_while(|run| run.state != JobRunState::Success)
                .collect();
            let (Some(newest), Some(oldest)) = (streak.first(), streak.last()) else {
                continue;
            };
            let (latest_error, same_error) = self.streak_errors(&streak);
            streaks.push(JobFailureStreak {
                job_id: job_id.clone(),
                count: streak.len(),
                since: oldest.created_at,
                latest_run_id: newest.run_id.to_string(),
                latest_error,
                same_error,
                truncated: streak.len() == STREAK_SCAN_LIMIT,
            });
        }
        Ok(streaks)
    }

    /// The newest run's one-line error and whether the inspected runs agree.
    /// A run whose error cannot be read has no signature, so it counts as a
    /// different one: "same error" is only claimed on evidence.
    fn streak_errors(&self, streak: &[&JobRun]) -> (Option<String>, bool) {
        let mut errors = streak
            .iter()
            .take(SIGNATURE_RUN_LIMIT)
            .map(|run| self.failed_run_error(run));
        let latest = errors.next().flatten();
        let Some(latest) = latest else {
            return (None, streak.len() == 1);
        };
        let signature = error_signature(&latest);
        let same = errors.all(|error| {
            error
                .as_deref()
                .is_some_and(|error| error_signature(error) == signature)
        });
        (Some(one_line(&latest)), same)
    }

    /// The deepest recorded cause of one failed run as "step: message".
    fn failed_run_error(&self, run: &JobRun) -> Option<String> {
        let run = self.get_job_run_backend(&run.run_id).ok().flatten()?;
        let state = self.read_run_state(&run.run_id).ok().flatten();
        let mut ignore = |_: &_| {};
        let cause = self
            .run_failure_context(
                &run,
                state.as_ref(),
                RunFailureContextProfile::DeepestObserved,
                &mut ignore,
            )
            .ok()
            .and_then(|context| context.causes.into_iter().next())
            .map(Ok)
            .unwrap_or_else(|| {
                self.run_failure_cause(&run, RunFailureContextProfile::DeepestObserved, &mut ignore)
            })
            .ok()?;
        let step = cause
            .step
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let message = cause
            .message
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match (step, message) {
            (Some(step), Some(message)) => Some(format!("{step}: {message}")),
            (None, Some(message)) => Some(message.to_string()),
            (Some(step), None) => Some(step.to_string()),
            (None, None) => None,
        }
    }
}

/// First non-empty line of `text`, cut to [`LATEST_ERROR_CHARS`].
fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.chars().count() <= LATEST_ERROR_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(LATEST_ERROR_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// An error with the parts that differ between occurrences removed: the first
/// line only, with any word holding a digit (ids, hashes, counts, timestamps)
/// or a path separator replaced by a placeholder. Bracketed error codes such as
/// `[provider_capacity]` and the wording stay.
fn error_signature(error: &str) -> String {
    one_line(error)
        .split_whitespace()
        .map(|word| {
            if word.contains('/') {
                "<path>"
            } else if word.chars().any(|c| c.is_ascii_digit()) {
                "<n>"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
