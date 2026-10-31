//! Run outcomes [ORB-18688]: admission episodes and returns from blocked
//! work, read from the task status history Orbit already records.
//!
//! An episode is one admission: from the task's entry into `in-progress`
//! to the next status it settles in. Settling in `review` or `done` is a
//! delivery; `blocked`, `someday` or `proposed` leave the dispatch queue
//! (blocked); `backlog` puts the task straight back in the queue (a failed,
//! cancelled or interrupted run, or a provider hold); `rejected` or
//! `archived` abandon it. A task that left the queue may return: its next
//! entry into `backlog` or `in-progress` is a return, `review` or `done` a
//! resolution outside the drain, and `rejected` or `archived` an absorbing
//! competing event; still waiting, it is right-censored. Returns are
//! estimated with a competing-risk cumulative incidence (Aalen–Johansen)
//! over [`RETURN_HORIZON_HOURS`].

use chrono::{DateTime, Utc};
use orbit_types::task::{TaskHistoryEntry, TaskStatus};

use super::model::{CellRow, Rng};
use super::review_modes::ReviewMode;

/// Returns are observed, and drawn, for at most 90 days after a setback.
pub(super) const RETURN_HORIZON_HOURS: f64 = super::schedule::HORIZON_HOURS;
/// Days of return evidence read before the lookback starts, so a recent
/// lookback need not prove a 90-day tail on its own.
pub(super) const RETURN_WINDOW_DAYS: i64 = 90;
/// Attempts one task may make in one simulated drain; a task still
/// retrying then counts as not draining.
pub(super) const MAX_ATTEMPTS: usize = 25;

/// How one admission episode settled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum EpisodeOutcome {
    /// Handed off to review, or done.
    Delivered,
    /// Left the dispatch queue: blocked, someday or proposed.
    Blocked,
    /// Back in the backlog at once.
    Failed,
    /// Rejected or archived.
    Abandoned,
}

impl EpisodeOutcome {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
        }
    }

    fn of(settled: TaskStatus) -> Option<Self> {
        match settled {
            TaskStatus::Review | TaskStatus::Done => Some(Self::Delivered),
            TaskStatus::Blocked | TaskStatus::Someday | TaskStatus::Proposed => Some(Self::Blocked),
            TaskStatus::Backlog => Some(Self::Failed),
            TaskStatus::Rejected | TaskStatus::Archived => Some(Self::Abandoned),
            TaskStatus::InProgress => None,
        }
    }
}

/// What ended a wait out of the queue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ReturnEvent {
    /// Back in `backlog` or `in-progress`.
    Returned,
    /// `review` or `done` without another admission.
    Resolved,
    /// `rejected` or `archived`.
    Absorbed,
}

/// When a task left the queue, and what ended that wait (if anything has).
pub(super) type Away = (DateTime<Utc>, Option<(DateTime<Utc>, ReturnEvent)>);

/// One task's attempts and waits, from its status history.
#[derive(Default)]
pub(super) struct TaskEpisodes {
    /// Settled episodes: admitted, settled, outcome.
    pub(super) settled: Vec<(DateTime<Utc>, DateTime<Utc>, EpisodeOutcome)>,
    /// The admission of an episode still running.
    pub(super) live: Option<DateTime<Utc>>,
    /// Each time it left the queue, and what ended that wait.
    pub(super) away: Vec<Away>,
    /// When its current stretch in `blocked` began.
    pub(super) blocked_since: Option<DateTime<Utc>>,
}

impl TaskEpisodes {
    pub(super) fn read(history: &[TaskHistoryEntry]) -> Self {
        let transitions: Vec<(DateTime<Utc>, TaskStatus)> = history
            .iter()
            .filter_map(|entry| Some((entry.at, entry.to_status?)))
            .collect();
        let away_status = |status: Option<TaskStatus>| {
            matches!(
                status,
                Some(TaskStatus::Blocked | TaskStatus::Someday | TaskStatus::Proposed)
            )
        };
        let mut episodes = Self::default();
        let mut status: Option<TaskStatus> = None;
        for (index, (at, to)) in transitions.iter().copied().enumerate() {
            if status == Some(to) {
                continue;
            }
            let was = status.replace(to);
            if to == TaskStatus::InProgress {
                episodes.live = Some(at);
                continue;
            }
            if let (Some(admitted), Some(outcome)) = (episodes.live.take(), EpisodeOutcome::of(to))
            {
                episodes.settled.push((admitted, at, outcome));
            }
            let leaves_queue = (to == TaskStatus::Blocked && !away_status(was))
                || (was == Some(TaskStatus::InProgress)
                    && matches!(to, TaskStatus::Someday | TaskStatus::Proposed));
            if leaves_queue {
                let follow = transitions[index + 1..].iter().find_map(|(later, status)| {
                    let event = match status {
                        TaskStatus::Backlog | TaskStatus::InProgress => ReturnEvent::Returned,
                        TaskStatus::Review | TaskStatus::Done => ReturnEvent::Resolved,
                        TaskStatus::Rejected | TaskStatus::Archived => ReturnEvent::Absorbed,
                        _ => return None,
                    };
                    Some((*later, event))
                });
                episodes.away.push((at, follow));
            }
            if to == TaskStatus::Blocked && was != Some(TaskStatus::Blocked) {
                episodes.blocked_since = Some(at);
            }
        }
        episodes
    }

    /// The settlement of the last attempt that did not deliver, at or
    /// before `until`: where a delivery's own measure starts.
    pub(super) fn last_setback(&self, until: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
        self.settled
            .iter()
            .filter(|(_, settled, outcome)| {
                *outcome != EpisodeOutcome::Delivered && until.is_none_or(|until| *settled <= until)
            })
            .map(|(_, settled, _)| *settled)
            .max()
    }
}

/// One settled admission episode, as a sample.
pub(super) struct Episode {
    pub(super) complexity: String,
    pub(super) crew: Option<String>,
    pub(super) review_mode: ReviewMode,
    pub(super) outcome: EpisodeOutcome,
    /// Wall hours from admission to settlement: the slot it held.
    pub(super) hold_hours: f64,
    pub(super) tokens: Option<f64>,
    pub(super) usd: Option<f64>,
}

/// One wait out of the queue, as a return observation.
pub(super) struct AwayWait {
    pub(super) complexity: String,
    pub(super) crew: Option<String>,
    pub(super) review_mode: ReviewMode,
    /// Hours to its event, or until it was censored.
    pub(super) hours: f64,
    /// `None` when still waiting (or past the horizon): right-censored.
    pub(super) event: Option<ReturnEvent>,
}

impl CellRow for Episode {
    fn complexity(&self) -> &str {
        &self.complexity
    }
    fn crew(&self) -> Option<&str> {
        self.crew.as_deref()
    }
    fn review_mode(&self) -> ReviewMode {
        self.review_mode
    }
}

impl CellRow for AwayWait {
    fn complexity(&self) -> &str {
        &self.complexity
    }
    fn crew(&self) -> Option<&str> {
        self.crew.as_deref()
    }
    fn review_mode(&self) -> ReviewMode {
        self.review_mode
    }
}

/// What follows an attempt that did not deliver, or a blocked task's wait.
///
/// `Censored` is still waiting when the horizon arrives. `Absorb` and
/// `Never` have stopped: absorption after its own wait, abandonment or an
/// attempt cap with none. Collapsing those into one variant either drops
/// the censored wait or keeps a finished task blocked for the rest of the
/// horizon.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Follow {
    /// Back in the dispatch queue after this many hours.
    Requeue(f64),
    /// Finished outside the drain after this many hours.
    Resolve(f64),
    /// Rejected or archived after this many hours out of the queue.
    Absorb(f64),
    /// No event drawn: unresolved through the horizon, holding no slot.
    Censored,
    /// Stopped with no return wait: abandoned, or attempts ran out.
    Never,
}

/// A return pool's cumulative incidence: probability mass per event and
/// time, the rest still waiting.
#[derive(Default)]
pub(super) struct ReturnCurve {
    atoms: Vec<(f64, ReturnEvent, f64)>,
    /// Waits in the pool, and how many of them were censored.
    pub(super) support: usize,
    pub(super) censored: usize,
    /// The risk set ran out before the horizon with mass still waiting:
    /// the tail past the last observation is unknown.
    pub(super) tail_unsupported: bool,
    last_observed: f64,
}

impl ReturnCurve {
    pub(super) fn estimate(waits: &[AwayWait], pool: &[usize]) -> Self {
        let mut observations: Vec<(f64, Option<ReturnEvent>)> = pool
            .iter()
            .map(|index| (waits[*index].hours, waits[*index].event))
            .collect();
        // Events at a time come before censorings at the same time.
        observations.sort_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then(left.1.is_none().cmp(&right.1.is_none()))
        });
        let mut curve = Self {
            support: observations.len(),
            censored: observations.iter().filter(|o| o.1.is_none()).count(),
            ..Self::default()
        };
        let mut at_risk = observations.len() as f64;
        let mut surviving = 1.0f64;
        let mut index = 0;
        while index < observations.len() {
            let at = observations[index].0;
            let mut counts = [0.0f64; 3];
            let mut leaving = 0.0;
            while index < observations.len() && observations[index].0 == at {
                if let Some(event) = observations[index].1 {
                    counts[event as usize] += 1.0;
                }
                leaving += 1.0;
                index += 1;
            }
            let events: f64 = counts.iter().sum();
            for (event, count) in [
                ReturnEvent::Returned,
                ReturnEvent::Resolved,
                ReturnEvent::Absorbed,
            ]
            .into_iter()
            .zip(counts)
            {
                if count > 0.0 {
                    curve.atoms.push((at, event, surviving * count / at_risk));
                }
            }
            if events > 0.0 {
                surviving *= 1.0 - events / at_risk;
            }
            at_risk -= leaving;
            curve.last_observed = at;
        }
        curve.tail_unsupported = !observations.is_empty()
            && surviving > 1e-9
            && curve.last_observed < RETURN_HORIZON_HOURS;
        curve
    }

    /// What follows a wait that has already lasted `age` hours.
    ///
    /// Age 0 is a fresh block: no atom has elapsed, so a return recorded
    /// at the same instant is still drawn. A positive age has elapsed
    /// every atom at or before it. Both choices use `return_atom_elapsed`,
    /// so the remaining mass is the complement.
    pub(super) fn draw(&self, rng: &mut Rng, age: f64) -> Follow {
        let elapsed: f64 = self
            .atoms
            .iter()
            .filter(|atom| return_atom_elapsed(age, atom.0))
            .map(|atom| atom.2)
            .sum();
        let surviving = 1.0 - elapsed;
        // Nothing left to draw is an unresolved wait, not an absorbing event.
        if surviving <= 1e-9 {
            return Follow::Censored;
        }
        let mut left = rng.unit() * surviving;
        for &(at, event, mass) in self
            .atoms
            .iter()
            .filter(|atom| !return_atom_elapsed(age, atom.0))
        {
            if left <= mass {
                let wait = at - age;
                return match event {
                    ReturnEvent::Returned => Follow::Requeue(wait),
                    ReturnEvent::Resolved => Follow::Resolve(wait),
                    ReturnEvent::Absorbed => Follow::Absorb(wait),
                };
            }
            left -= mass;
        }
        Follow::Censored
    }

    /// Probability of returning to the queue within the horizon.
    pub(super) fn return_probability(&self) -> Option<f64> {
        (self.support > 0).then(|| {
            self.atoms
                .iter()
                .filter(|atom| atom.1 == ReturnEvent::Returned)
                .map(|atom| atom.2)
                .sum()
        })
    }

    /// Whether a wait already `age` hours old has no observation left
    /// to draw its future from.
    ///
    /// An observation counts as passed only once `return_atom_elapsed`
    /// says so. A fresh block is therefore not past a pool whose last
    /// observation sits at age 0.
    pub(super) fn unsupported_at(&self, age: f64) -> bool {
        self.support == 0 || (self.tail_unsupported && return_atom_elapsed(age, self.last_observed))
    }
}

/// Whether a return atom at `at` has already elapsed for a wait of `age`
/// hours. A fresh wait has elapsed nothing, including a return recorded
/// at the instant the block began.
fn return_atom_elapsed(age: f64, at: f64) -> bool {
    age > 0.0 && at <= age
}

/// The hours from `from` to `to`, never negative.
///
/// Sub-second spans stay fractional. Whole seconds made a return a
/// fraction of a second after the block into a zero-time atom.
pub(super) fn hours_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    (to - from).as_seconds_f64().max(0.0) / 3600.0
}
