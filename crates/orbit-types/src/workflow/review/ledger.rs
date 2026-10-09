//! Review attempts and the per-lineage ledger [ORB-11333].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::super::automation::SourceRevision;
use super::{ReviewBudget, ReviewConsumption, ReviewVerdict};

/// The state of one reviewer attempt in a lineage ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ReviewAttemptState {
    /// Admitted; the reviewer may be running or the run was interrupted.
    Open,
    /// Settled with a verdict.
    Settled { verdict: ReviewVerdict },
}

/// One reviewer start recorded against a lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAttempt {
    pub attempt_id: String,
    /// One-based index within the lineage.
    pub index: u32,
    pub run_id: String,
    pub task_meaning_digest: String,
    pub candidate: SourceRevision,
    pub started_at: DateTime<Utc>,
    pub state: ReviewAttemptState,
    /// Reviewer runtime charged for this attempt at release or settlement.
    /// Absent until then. A released attempt may record more runtime
    /// afterwards; [`Self::elapsed_at`] counts that too, so this value is a
    /// floor rather than a freeze.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
    /// Set when the attempt was closed without a reviewer verdict — its
    /// reviewer step failed or its run ended first — and settled
    /// `incomplete` with the reviewer runtime spent so far. A resumed run of
    /// the same lineage may still settle it with a verdict; the charge is
    /// then replaced by the attempt's total reviewer runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<DateTime<Utc>>,
    /// Runtime of the reviewer invocations that finished for this attempt,
    /// summed across retries and resumed runs. Retry backoff, recovery
    /// activities and time no reviewer process ran are never part of it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reviewer_seconds: u64,
    /// The reviewer invocation running for this attempt, if one started and
    /// has not reported its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_running: Option<ReviewerInvocation>,
}

/// A reviewer invocation that started for an attempt and has not finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerInvocation {
    /// The run executing the reviewer.
    pub run_id: String,
    pub started_at: DateTime<Utc>,
    /// The invocation's own wall-clock bound; no reviewer process outlives it.
    pub deadline: DateTime<Utc>,
}

/// A reviewer invocation starting or ending for an attempt, as the engine
/// observes it around the reviewer step's dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerInvocationEvent {
    /// The reviewer process is about to start; the ledger answers with the
    /// wall-clock deadline it may run to ([`ReviewLedger::invocation_seconds_for`]).
    Started,
    /// The reviewer process ended, successfully or not, after running this
    /// long.
    Finished { runtime_seconds: u64 },
    /// The reviewer exceeded its invocation deadline; retain its partial
    /// report and release this attempt as incomplete for continuation.
    TimedOut { runtime_seconds: u64 },
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl ReviewAttempt {
    /// Charge counted against the review budget at `now`.
    ///
    /// A recorded [`Self::elapsed_seconds`] is a floor. Release stores the
    /// runtime spent so far and still accepts later invocations; once
    /// [`Self::reviewer_runtime_at`] exceeds that floor, the greater value
    /// counts, so a resumed run cannot spend the budget again. An attempt
    /// with no recorded charge reports its runtime only. Settlement sets the
    /// recorded charge to the runtime and clears any running invocation, so
    /// the two agree.
    pub fn elapsed_at(&self, now: DateTime<Utc>) -> u64 {
        self.elapsed_seconds
            .unwrap_or(0)
            .max(self.reviewer_runtime_at(now))
    }

    /// Reviewer process runtime spent on this attempt, counting a running
    /// invocation up to `bound` — the latest instant it can still have been
    /// running, such as the end of a run that died with it — and never past
    /// its own deadline.
    pub fn reviewer_runtime_at(&self, bound: DateTime<Utc>) -> u64 {
        let running = self.reviewer_running.as_ref().map_or(0, |running| {
            seconds_between(running.started_at, bound.min(running.deadline))
        });
        self.reviewer_seconds.saturating_add(running)
    }

    /// The run that still holds this attempt: the one running its reviewer,
    /// else the admitting run while the attempt is open.
    pub fn holder_run_id(&self) -> Option<&str> {
        match (&self.reviewer_running, &self.state) {
            (Some(running), _) => Some(running.run_id.as_str()),
            (None, ReviewAttemptState::Open) => Some(self.run_id.as_str()),
            (None, ReviewAttemptState::Settled { .. }) => None,
        }
    }
}

/// Whole seconds from `start` to `end`; a clock behind `start` counts as zero.
pub fn seconds_between(start: DateTime<Utc>, end: DateTime<Utc>) -> u64 {
    u64::try_from(end.signed_duration_since(start).num_seconds()).unwrap_or(0)
}

/// An operator decision starting a fresh budget while retaining attempt history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewResetDecision {
    /// All attempts through this index belong to the previous budget.
    pub after_attempt_index: u32,
    pub reason: String,
    pub actor: String,
    pub recorded_at: DateTime<Utc>,
    pub previous_budget: ReviewBudget,
    pub previous_consumption: ReviewConsumption,
    pub budget: ReviewBudget,
}

/// Review attempts for one delivery run lineage. Each candidate's attempts
/// make up its one review; `consumed_seconds` totals the lineage's settled
/// reviewer runtime since the last reset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewLedger {
    pub lineage_key: String,
    pub task_ids: Vec<String>,
    pub budget: ReviewBudget,
    pub attempts: Vec<ReviewAttempt>,
    pub consumed_seconds: u64,
    /// Audited budget resets; absent in ledgers written before reset support.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<ReviewResetDecision>,
    /// Compare-and-set handle.
    pub revision: u32,
    pub updated_at: DateTime<Utc>,
}

impl ReviewLedger {
    /// A fresh ledger.
    pub fn new(
        lineage_key: &str,
        task_ids: Vec<String>,
        budget: ReviewBudget,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            lineage_key: lineage_key.to_string(),
            task_ids,
            budget,
            attempts: Vec::new(),
            consumed_seconds: 0,
            decisions: Vec::new(),
            revision: 0,
            updated_at: now,
        }
    }

    /// Last attempt retired by an operator reset, or zero for the original budget.
    pub fn reset_through(&self) -> u32 {
        self.decisions
            .last()
            .map_or(0, |decision| decision.after_attempt_index)
    }

    /// Reviewer runtime the lineage settled under the current budget.
    pub fn consumed(&self) -> ReviewConsumption {
        ReviewConsumption {
            seconds: self.consumed_seconds,
        }
    }

    /// Attempts on `candidate` under `task_meaning_digest` since the last
    /// reset: together they are that candidate's one review.
    fn review_attempts<'a>(
        &'a self,
        candidate: &'a SourceRevision,
        task_meaning_digest: &'a str,
    ) -> impl Iterator<Item = &'a ReviewAttempt> + 'a {
        let reset_through = self.reset_through();
        self.attempts.iter().filter(move |attempt| {
            attempt.index > reset_through
                && attempt.candidate == *candidate
                && attempt.task_meaning_digest == task_meaning_digest
        })
    }

    /// Whether `candidate` already had its review: an attempt on it settled
    /// with a reviewer verdict rather than being released unfinished.
    pub fn reviewed(&self, candidate: &SourceRevision, task_meaning_digest: &str) -> bool {
        self.review_attempts(candidate, task_meaning_digest)
            .any(|attempt| {
                attempt.released_at.is_none()
                    && matches!(attempt.state, ReviewAttemptState::Settled { .. })
            })
    }

    /// Reviewer runtime `candidate`'s review has spent at `now`, counting a
    /// running reviewer and any runtime recorded after a provisional release.
    pub fn consumed_for(
        &self,
        candidate: &SourceRevision,
        task_meaning_digest: &str,
        now: DateTime<Utc>,
    ) -> ReviewConsumption {
        ReviewConsumption {
            seconds: self
                .review_attempts(candidate, task_meaning_digest)
                .map(|attempt| attempt.elapsed_at(now))
                .fold(0, u64::saturating_add),
        }
    }

    /// What `candidate`'s review may still spend at `now`.
    pub fn remaining_for(
        &self,
        candidate: &SourceRevision,
        task_meaning_digest: &str,
        now: DateTime<Utc>,
    ) -> ReviewConsumption {
        let consumed = self.consumed_for(candidate, task_meaning_digest, now);
        ReviewConsumption {
            seconds: u64::from(self.budget.minutes)
                .saturating_mul(60)
                .saturating_sub(consumed.seconds),
        }
    }

    /// The wall-clock deadline of `candidate`'s next reviewer invocation:
    /// half of what its review has left, keeping the other half for a
    /// continuation if the invocation hits the deadline. The manifest
    /// advertises it and the engine bounds the reviewer process by it.
    pub fn invocation_seconds_for(
        &self,
        candidate: &SourceRevision,
        task_meaning_digest: &str,
        now: DateTime<Utc>,
    ) -> u64 {
        self.remaining_for(candidate, task_meaning_digest, now)
            .seconds
            / 2
    }

    /// What the latest review may still spend at `now`: the candidate of the
    /// most recent attempt since the last reset, or the whole budget when
    /// none was admitted since.
    pub fn remaining_at(&self, now: DateTime<Utc>) -> ReviewConsumption {
        match self.latest_attempt() {
            Some(latest) => self.remaining_for(&latest.candidate, &latest.task_meaning_digest, now),
            None => ReviewConsumption {
                seconds: u64::from(self.budget.minutes).saturating_mul(60),
            },
        }
    }

    /// The most recent attempt admitted under the current budget.
    pub fn latest_attempt(&self) -> Option<&ReviewAttempt> {
        self.attempts
            .last()
            .filter(|attempt| attempt.index > self.reset_through())
    }

    /// The ledger as `attempt_id`'s settlement left it: attempts admitted
    /// later are dropped, so a settlement finished after a restart reports
    /// what the lineage had consumed then. `None` when the attempt is not
    /// part of this lineage.
    pub fn as_of(&self, attempt_id: &str) -> Option<ReviewLedger> {
        let position = self
            .attempts
            .iter()
            .position(|attempt| attempt.attempt_id == attempt_id)?;
        let mut ledger = self.clone();
        ledger.attempts.truncate(position.saturating_add(1));
        let index = ledger.attempts.last()?.index;
        let original_budget = self
            .decisions
            .first()
            .map_or(self.budget, |d| d.previous_budget);
        ledger.decisions.retain(|d| d.after_attempt_index < index);
        ledger.budget = ledger
            .decisions
            .last()
            .map_or(original_budget, |d| d.budget);
        let reset_through = ledger.reset_through();
        // Only settlement charges seconds, so settled consumption is the
        // sum of the kept attempts' recorded elapsed time.
        ledger.consumed_seconds = ledger
            .attempts
            .iter()
            .filter(|attempt| attempt.index > reset_through)
            .filter_map(|attempt| attempt.elapsed_seconds)
            .fold(0, u64::saturating_add);
        Some(ledger)
    }

    /// The run that still holds an attempt of this lineage, if any.
    pub fn holder_run_id(&self) -> Option<&str> {
        self.attempts.iter().find_map(ReviewAttempt::holder_run_id)
    }

    /// The still-open attempt, if any.
    pub fn open_attempt(&self) -> Option<&ReviewAttempt> {
        self.attempts
            .iter()
            .find(|attempt| attempt.state == ReviewAttemptState::Open)
    }
}

/// The outcome of asking the ledger for a reviewer start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReviewReservation {
    /// A new reviewer start was reserved.
    Reserved { attempt: ReviewAttempt },
    /// An open attempt for the same candidate and task meaning is resumed
    /// after an interruption; no new start is consumed.
    Resumed { attempt: ReviewAttempt },
    /// The candidate's one review is spent; the caller must escalate.
    Exhausted {
        /// `review_candidate_reviewed` (an attempt on the candidate already
        /// settled with a verdict) or `review_minutes_exhausted`.
        reason: &'static str,
        /// The candidate's reviewer runtime.
        consumed: ReviewConsumption,
    },
}
