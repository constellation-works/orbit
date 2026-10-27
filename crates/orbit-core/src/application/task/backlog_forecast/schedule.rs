//! One simulated drain as the drain admits it [ORB-18686].
//!
//! An event loop over simulated hours: at t=0 the in-progress tasks hold
//! their slots and locks; at every completion (or a pilot's footprint
//! deadline, or an arrival) the pending queue is walked in dispatch order
//! and [`ForecastAdmission::pass`] admits what the drain would, up to the
//! free slots. A task starts only once its in-forecast dependencies have
//! finished in this iteration.
//!
//! Each task follows its iteration's [`Script`] [ORB-18688]: an attempt that
//! does not deliver frees its slot and locks when its sampled hold ends,
//! then the task requeues at once (failed), returns after its sampled wait
//! (blocked), finishes outside the drain, or waits unresolved. A censored
//! block holds no slot and keeps waiting until the horizon. Absorption
//! waits only until that event; abandonment and an attempt cap stop with
//! no return wait. The cap is a script that is still retrying after
//! [`MAX_ATTEMPTS`]: it counts only when that attempt starts, not when the
//! script was generated. A blocked task starts out waiting. A scenario
//! drains only once every scheduled task has finished.
//!
//! Under resource fit [ORB-18689] the pass also asks gap fit, which prices
//! each running task's profile by the hours since its admission (an
//! in-progress task's age at t=0 included); a pass it held while a slot was
//! free is retried as the running profiles age. Pure computation: no store,
//! clock or filesystem access.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::application::admission::forecast::{
    ForecastAdmission, ForecastPassScratch, ForecastTaskSet, ForecastWait,
};

use super::model::{Draw, Script, Step};
use super::outcomes::{EpisodeOutcome, Follow, MAX_ATTEMPTS};
use super::rework::ForecastGeneratedNode;

/// Every scenario stops at this many simulated hours (90 days); work still
/// pending then has not drained.
pub(super) const HORIZON_HOURS: f64 = 90.0 * 24.0;
/// Events one simulated drain may process before it counts as truncated.
const MAX_EVENTS: usize = 100_000;
/// One summed wait per [`ForecastWait::ALL`] reason.
pub(super) const WAITS: usize = ForecastWait::ALL.len();

/// One scenario's fixed scheduling inputs.
pub(super) struct Scenario<'a> {
    pub(super) admission: &'a ForecastAdmission,
    pub(super) edges: &'a [Vec<usize>],
    dependents: Vec<Vec<usize>>,
    /// The tasks this scenario schedules: its members less unforecastable
    /// ones.
    pub(super) included: Vec<bool>,
    pub(super) in_progress: &'a [bool],
    /// Hours since each in-progress task's running attempt was admitted.
    age_hours: &'a [f64],
    pub(super) slots: usize,
    pub(super) generated: &'a [ForecastGeneratedNode],
    pub(super) initial: usize,
    /// `included` in dependency order.
    topological: Vec<usize>,
}

impl<'a> Scenario<'a> {
    pub(super) fn new(
        admission: &'a ForecastAdmission,
        edges: &'a [Vec<usize>],
        included: Vec<bool>,
        in_progress: &'a [bool],
        age_hours: &'a [f64],
        slots: usize,
    ) -> Self {
        let len = edges.len();
        let mut dependents = vec![Vec::new(); len];
        let mut indegree = vec![0usize; len];
        for (index, deps) in edges.iter().enumerate() {
            if !included[index] {
                continue;
            }
            for dep in deps {
                dependents[*dep].push(index);
                indegree[index] += 1;
            }
        }
        let mut queue: Vec<usize> = (0..len)
            .filter(|index| included[*index] && indegree[*index] == 0)
            .collect();
        let mut topological = Vec::with_capacity(len);
        while let Some(index) = queue.pop() {
            topological.push(index);
            for dependent in &dependents[index] {
                indegree[*dependent] -= 1;
                if indegree[*dependent] == 0 {
                    queue.push(*dependent);
                }
            }
        }
        Self {
            admission,
            edges,
            dependents,
            included,
            in_progress,
            age_hours,
            slots: slots.max(1),
            generated: &[],
            initial: len,
            topological,
        }
    }
}

/// What one simulated drain did.
pub(super) struct Run {
    /// Hours until everything finished; `None` past the horizon, when
    /// truncated, or without hours to draw.
    pub(super) drained: Option<f64>,
    pub(super) critical_path: Option<f64>,
    /// Occupied leaf slots integrated over the run, divided by its length.
    pub(super) parallelism: Option<f64>,
    /// Summed task-hours spent waiting, per [`ForecastWait::ALL`] reason.
    pub(super) waits: Option<[f64; WAITS]>,
    /// Summed task-hours out of the queue: finite returns and resolutions,
    /// the wait until absorption, and censored waits through the horizon.
    pub(super) return_wait: Option<f64>,
    /// Attempts started that were blocked, failed or abandoned.
    pub(super) setbacks: [u64; 3],
    /// Tasks whose [`MAX_ATTEMPTS`]-th attempt started on a script that
    /// was still retrying. Steps the horizon never starts do not count.
    pub(super) attempt_limited: u64,
    /// Scheduled tasks left without a return.
    pub(super) no_return: usize,
    /// Scheduled initial tasks that did not finish by the horizon.
    pub(super) unfinished: usize,
    pub(super) truncated: bool,
    pub(super) followups: u64,
    pub(super) outside_horizon: u64,
    pub(super) rework: Draw,
    pub(super) arrivals: Draw,
}

/// A heap entry: the earliest end first, ties by key.
#[derive(PartialEq)]
struct End(f64, usize);

/// How the outstanding wait in [`ScheduleState::returns`] ends.
#[derive(Clone, Copy)]
enum ReturnKind {
    Requeue,
    Resolve,
    Absorb,
}

impl Eq for End {}

impl PartialOrd for End {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for End {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.total_cmp(&self.0).then(other.1.cmp(&self.1))
    }
}

/// Buffers reused across iterations.
pub(super) struct ScheduleState {
    running: ForecastTaskSet,
    /// Per running task: the simulated hour its attempt was admitted.
    admitted_at: Vec<f64>,
    heap: BinaryHeap<End>,
    indegree: Vec<usize>,
    pending: Vec<usize>,
    ready: Vec<usize>,
    selected: Vec<usize>,
    reason: Vec<ForecastWait>,
    filings: BinaryHeap<End>,
    followups: Vec<Vec<usize>>,
    outside_horizon: u64,
    followup_count: u64,
    scratch: ForecastPassScratch,
    /// Waits out of the queue, earliest first. [`ScheduleState::return_kind`]
    /// says whether each ends in a requeue, a resolution or absorption.
    returns: BinaryHeap<End>,
    return_kind: Vec<ReturnKind>,
    /// Per task: its current attempt, and its position in dispatch order.
    attempt: Vec<usize>,
    dispatch_rank: Vec<usize>,
    /// Per task, this iteration: what its started attempts used.
    pub(super) charged: Vec<Draw>,
    /// Per task, this iteration: first start hour and start rank, and
    /// whether it finished by the horizon.
    pub(super) start: Vec<Option<f64>>,
    pub(super) rank: Vec<Option<f64>>,
    pub(super) finished: Vec<bool>,
    filed: Vec<bool>,
}

impl ScheduleState {
    /// Put task `index` back in the pending queue at its dispatch position.
    fn requeue(&mut self, index: usize) {
        let rank = &self.dispatch_rank;
        let position = self
            .pending
            .partition_point(|other| rank[*other] < rank[index]);
        self.pending.insert(position, index);
    }

    pub(super) fn new(admission: &ForecastAdmission) -> Self {
        let len = admission.len();
        Self {
            running: ForecastTaskSet::with_len(len),
            admitted_at: vec![0.0; len],
            heap: BinaryHeap::new(),
            indegree: vec![0; len],
            pending: Vec::with_capacity(len),
            ready: Vec::with_capacity(len),
            selected: Vec::new(),
            reason: vec![ForecastWait::Capacity; len],
            filings: BinaryHeap::new(),
            followups: vec![Vec::new(); len],
            outside_horizon: 0,
            followup_count: 0,
            scratch: admission.scratch(),
            returns: BinaryHeap::new(),
            return_kind: vec![ReturnKind::Requeue; len],
            attempt: vec![0; len],
            dispatch_rank: {
                let mut rank = vec![0; len];
                for (position, index) in admission.dispatch_order().iter().enumerate() {
                    rank[*index] = position;
                }
                rank
            },
            charged: vec![Draw::MISSING; len],
            start: vec![None; len],
            rank: vec![None; len],
            finished: vec![false; len],
            filed: vec![false; len],
        }
    }
}

fn wait_slot(wait: ForecastWait) -> usize {
    ForecastWait::ALL
        .iter()
        .position(|one| *one == wait)
        .unwrap_or(0)
}

/// The longest dependency path through each task's attempts and return
/// waits, ignoring contention. A task that never finishes ends its path
/// unresolved, and so does everything behind it.
fn critical_path(
    scenario: &Scenario<'_>,
    scripts: &[Script],
    state: &ScheduleState,
) -> Option<f64> {
    let mut longest: Vec<Option<f64>> = vec![Some(0.0); scripts.len()];
    let mut overall = 0.0f64;
    for &index in &scenario.topological {
        if index >= scenario.initial && !state.filed[index] {
            continue;
        }
        let before = scenario.edges[index]
            .iter()
            .try_fold(0.0f64, |most, dep| Some(most.max(longest[*dep]?)));
        longest[index] = before.zip(scripts[index].finish_hours()).map(|(a, b)| {
            a + b
                + if index >= scenario.initial {
                    scenario.generated[index - scenario.initial].lag
                } else {
                    0.0
                }
        });
        overall = overall.max(longest[index]?);
    }
    Some(overall)
}

fn setback_slot(outcome: EpisodeOutcome) -> Option<usize> {
    match outcome {
        EpisodeOutcome::Blocked => Some(0),
        EpisodeOutcome::Failed => Some(1),
        EpisodeOutcome::Abandoned => Some(2),
        EpisodeOutcome::Delivered => None,
    }
}

/// Simulate one drain of `scenario` with this iteration's `scripts` per
/// task; `hours` is each task's delivery draw, `None` without history.
/// Every started attempt and arrival adds its use to `totals`.
pub(super) fn simulate_drain(
    scenario: &Scenario<'_>,
    hours: &[Option<f64>],
    scripts: &[Script],
    totals: &mut Draw,
    state: &mut ScheduleState,
) -> Run {
    let admission = scenario.admission;
    state.start.fill(None);
    state.rank.fill(None);
    state.finished.fill(false);
    state.filed.fill(false);
    for (index, deps) in scenario.edges.iter().enumerate() {
        state.indegree[index] = deps.len();
    }
    state.attempt.fill(0);
    state.charged.fill(Draw::ZERO);
    state.returns.clear();
    state.scratch.start_drain();
    let included: &[bool] = &scenario.included;
    let scheduled = move || (0..hours.len()).filter(move |index| included[*index]);
    let unavailable = Run {
        drained: None,
        critical_path: None,
        parallelism: None,
        waits: None,
        return_wait: None,
        setbacks: [0; 3],
        attempt_limited: 0,
        no_return: 0,
        unfinished: 0,
        truncated: false,
        followups: 0,
        outside_horizon: 0,
        rework: Draw::MISSING,
        arrivals: Draw::MISSING,
    };
    if scheduled().any(|index| hours[index].is_none()) {
        return unavailable;
    }
    state.running.clear();
    state.heap.clear();
    state.filings.clear();
    state.outside_horizon = 0;
    state.followup_count = 0;
    for fixes in &mut state.followups {
        fixes.clear()
    }
    for (offset, generated) in scenario.generated.iter().enumerate() {
        let index = scenario.initial + offset;
        if !scenario.included[index] {
            continue;
        }
        if let Some(origin) = generated.origin {
            state.followups[origin].push(index);
        } else {
            state.filings.push(End(generated.lag, index));
        }
    }
    state.pending.clear();
    let mut running = 0usize;
    let mut left = 0usize;
    let mut setbacks = [0u64; 3];
    let mut no_return = 0usize;
    let mut attempt_limited = 0u64;
    // Blocked tasks still waiting at the horizon. They hold no slot.
    let mut unresolved = 0usize;
    // Start task `index`'s current attempt at `at`, charging its use.
    let mut begin = |state: &mut ScheduleState, totals: &mut Draw, index: usize, at: f64| {
        let attempt = state.attempt[index];
        // A generated script can already hold 25 steps. Only starting the
        // last one, on a script that would still retry, is a cap hit.
        if scripts[index].limited && attempt + 1 == MAX_ATTEMPTS {
            attempt_limited += 1;
        }
        let step: Step = scripts[index].steps[attempt];
        state.charged[index].add(&step.spend);
        totals.add(&step.spend);
        if let Some(slot) = setback_slot(step.outcome) {
            setbacks[slot] += 1;
        }
        state.running.insert(index);
        state.admitted_at[index] = at;
        state.heap.push(End(at + step.hours, index));
    };
    for index in scheduled().filter(|index| *index < scenario.initial) {
        state.filed[index] = true;
        left += 1;
        if scenario.in_progress[index] {
            running += 1;
            state.start[index] = Some(0.0);
            state.rank[index] = Some(0.0);
            begin(state, totals, index, 0.0);
            state.admitted_at[index] = -scenario.age_hours[index];
            continue;
        }
        match scripts[index].initial {
            Follow::Requeue(wait) if wait <= 0.0 => {}
            follow => schedule_follow(state, &mut unresolved, &mut no_return, index, 0.0, follow),
        }
    }
    state
        .pending
        .extend(admission.dispatch_order().iter().copied().filter(|index| {
            scenario.included[*index]
                && *index < scenario.initial
                && !scenario.in_progress[*index]
                && scripts[*index].initial == Follow::Requeue(0.0)
        }));
    let mut at = 0.0f64;
    let mut started = 0.0f64;
    let mut occupied = 0.0f64;
    let mut waits = [0.0f64; WAITS];
    let mut return_wait = 0.0f64;
    let mut events = 0usize;
    let mut truncated = false;
    let drained = loop {
        // The drain passes again as soon as it admits something, so a task
        // that only conflicted with this wave (a `no-diff-expected` task
        // holds no lock once running) starts at the same instant.
        loop {
            state.ready.clear();
            for &index in &state.pending {
                if state.indegree[index] == 0 {
                    state.ready.push(index);
                } else {
                    state.reason[index] = ForecastWait::Dependency;
                }
            }
            state.selected.clear();
            let reason = &mut state.reason;
            admission.pass(
                &state.ready,
                &state.running,
                &state.admitted_at,
                scenario.slots.saturating_sub(running),
                scenario.slots,
                at,
                &mut state.scratch,
                &mut state.selected,
                |index, wait| reason[index] = wait,
            );
            let selected = std::mem::take(&mut state.selected);
            for &index in &selected {
                started += 1.0;
                if state.start[index].is_none() {
                    state.start[index] = Some(at);
                    state.rank[index] = Some(started);
                }
                running += 1;
                begin(state, totals, index, at);
            }
            let admitted = !selected.is_empty();
            state.selected = selected;
            if !admitted {
                break;
            }
            let live = &state.running;
            state.pending.retain(|index| !live.contains(*index));
            if running >= scenario.slots {
                break;
            }
        }
        if left == 0 && state.returns.is_empty() && running == 0 {
            break Some(at);
        }

        let deadline = state
            .pending
            .iter()
            .filter_map(|index| admission.footprint_deadline(*index))
            .filter(|deadline| *deadline > at)
            .fold(f64::INFINITY, f64::min);
        // Gap fit held work beside a free slot: the curve falls as the
        // running profiles age, so the drain's next pass may admit it.
        let recheck = (state.scratch.resource_held() && running < scenario.slots)
            .then(|| admission.resource_recheck(&state.running, &state.admitted_at, at))
            .flatten()
            .unwrap_or(f64::INFINITY);
        let next = state
            .heap
            .peek()
            .map_or(f64::INFINITY, |end| end.0)
            .min(deadline)
            .min(recheck)
            .min(state.returns.peek().map_or(f64::INFINITY, |end| end.0))
            .min(state.filings.peek().map_or(f64::INFINITY, |end| end.0));
        let until = next.min(HORIZON_HOURS);
        let span = (until - at).max(0.0);
        occupied += running as f64 * span;
        for &index in &state.pending {
            waits[wait_slot(state.reason[index])] += span;
        }
        return_wait += (state.returns.len() + unresolved) as f64 * span;
        if !next.is_finite() || next > HORIZON_HOURS {
            // Nothing left that could start the pending work, or the
            // horizon came first.
            at = until.max(at);
            break None;
        }
        events += 1;
        if events > MAX_EVENTS {
            truncated = true;
            break None;
        }
        at = next;
        while state.heap.peek().is_some_and(|end| end.0 <= at) {
            let Some(End(_, index)) = state.heap.pop() else {
                break;
            };
            running -= 1;
            state.running.remove(index);
            let step = scripts[index].steps[state.attempt[index]];
            if step.outcome == EpisodeOutcome::Delivered {
                left -= 1;
                left += finish(state, scenario, index, at);
                continue;
            }
            state.attempt[index] += 1;
            let more = state.attempt[index] < scripts[index].steps.len();
            match step.then {
                Follow::Requeue(wait) if more && wait <= 0.0 => state.requeue(index),
                // No attempt left to run this requeue.
                Follow::Requeue(_) if !more => no_return += 1,
                follow => {
                    schedule_follow(state, &mut unresolved, &mut no_return, index, at, follow)
                }
            }
        }
        while state.returns.peek().is_some_and(|end| end.0 <= at) {
            let Some(End(_, index)) = state.returns.pop() else {
                break;
            };
            match state.return_kind[index] {
                ReturnKind::Resolve => {
                    left -= 1;
                    left += finish(state, scenario, index, at);
                }
                ReturnKind::Requeue => state.requeue(index),
                // Absorption ends the wait. The task stays unfinished and
                // does not keep accruing return time.
                ReturnKind::Absorb => {}
            }
        }
        while state.filings.peek().is_some_and(|end| end.0 <= at) {
            let Some(End(_, index)) = state.filings.pop() else {
                break;
            };
            if scenario.generated[index - scenario.initial]
                .origin
                .is_none()
            {
                left += 1;
            }
            state.indegree[index] = 0;
            state.filed[index] = true;
            state.requeue(index);
        }
    };
    let length = drained.unwrap_or(at);
    let mut rework = Draw::ZERO;
    let mut arrivals = Draw::ZERO;
    for (offset, node) in scenario.generated.iter().enumerate() {
        if node.origin.is_some() {
            rework.add(&state.charged[scenario.initial + offset]);
        } else {
            arrivals.add(&state.charged[scenario.initial + offset]);
        }
    }
    if truncated {
        return Run {
            truncated: true,
            ..unavailable
        };
    }
    Run {
        drained,
        critical_path: critical_path(scenario, scripts, state),
        parallelism: Some(if length > 0.0 { occupied / length } else { 0.0 }),
        waits: Some(waits),
        return_wait: Some(return_wait),
        setbacks,
        attempt_limited,
        no_return,
        unfinished: scheduled()
            .filter(|index| *index < scenario.initial && !state.finished[*index])
            .count(),
        truncated,
        followups: state.followup_count,
        outside_horizon: state.outside_horizon,
        rework,
        arrivals,
    }
}

/// Put `follow` on the schedule at `at`. A non-positive requeue is the
/// caller's immediate requeue. Censored work joins `unresolved` and holds
/// no slot; absorption and a bare stop do not wait past their own end.
fn schedule_follow(
    state: &mut ScheduleState,
    unresolved: &mut usize,
    no_return: &mut usize,
    index: usize,
    at: f64,
    follow: Follow,
) {
    match follow {
        Follow::Requeue(wait) if wait <= 0.0 => {}
        Follow::Requeue(wait) => {
            state.return_kind[index] = ReturnKind::Requeue;
            state.returns.push(End(at + wait, index));
        }
        Follow::Resolve(wait) => {
            state.return_kind[index] = ReturnKind::Resolve;
            state.returns.push(End(at + wait, index));
        }
        Follow::Absorb(wait) => {
            *no_return += 1;
            if wait > 0.0 {
                state.return_kind[index] = ReturnKind::Absorb;
                state.returns.push(End(at + wait, index));
            }
        }
        Follow::Censored => {
            *no_return += 1;
            *unresolved += 1;
        }
        Follow::Never => *no_return += 1,
    }
}

/// Task `index` finished: its dependents lose one unmet dependency.
fn finish(state: &mut ScheduleState, scenario: &Scenario<'_>, index: usize, at: f64) -> usize {
    state.finished[index] = true;
    for dependent in &scenario.dependents[index] {
        state.indegree[*dependent] -= 1;
    }
    let mut added = 0;
    for &fix in &state.followups[index] {
        let due = at + scenario.generated[fix - scenario.initial].lag;
        if due > HORIZON_HOURS {
            state.outside_horizon += 1;
        } else {
            state.followup_count += 1;
            state.filed[fix] = true;
            state.filings.push(End(due, fix));
            added += 1;
        }
    }
    added
}
