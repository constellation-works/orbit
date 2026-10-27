//! One simulated drain as the drain admits it [ORB-18686].
//!
//! An event loop over simulated hours: at t=0 the in-progress tasks hold
//! their slots and locks; at every completion (or a pilot's footprint
//! deadline, or an arrival) the pending queue is walked in dispatch order
//! and [`ForecastAdmission::pass`] admits what the drain would, up to the
//! free slots. A task starts only once its in-forecast dependencies have
//! finished in this iteration. Pure computation: no store, clock or
//! filesystem access.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};

use crate::application::admission::forecast::{
    ForecastAdmission, ForecastPassScratch, ForecastTaskSet, ForecastWait,
};

use super::model::{Arrivals, Draw, Rng};

/// Every scenario stops at this many simulated hours (90 days); work still
/// pending then has not drained.
pub(super) const HORIZON_HOURS: f64 = 90.0 * 24.0;
/// Events one simulated drain may process before it counts as truncated.
const MAX_EVENTS: usize = 100_000;
/// The heap key of a running arrival, which is no forecast task.
const ARRIVAL: usize = usize::MAX;

/// One scenario's fixed scheduling inputs.
pub(super) struct Scenario<'a> {
    pub(super) admission: &'a ForecastAdmission,
    pub(super) edges: &'a [Vec<usize>],
    dependents: Vec<Vec<usize>>,
    /// The tasks this scenario schedules: its members less unforecastable
    /// ones.
    pub(super) included: Vec<bool>,
    pub(super) in_progress: &'a [bool],
    pub(super) slots: usize,
    /// `included` in dependency order.
    topological: Vec<usize>,
}

impl<'a> Scenario<'a> {
    pub(super) fn new(
        admission: &'a ForecastAdmission,
        edges: &'a [Vec<usize>],
        included: Vec<bool>,
        in_progress: &'a [bool],
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
            slots: slots.max(1),
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
    pub(super) waits: Option<[f64; 5]>,
    /// Scheduled initial tasks that did not finish by the horizon.
    pub(super) unfinished: usize,
    pub(super) truncated: bool,
}

/// A heap entry: the earliest end first, ties by key.
#[derive(PartialEq)]
struct End(f64, usize);

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
    heap: BinaryHeap<End>,
    indegree: Vec<usize>,
    pending: Vec<usize>,
    ready: Vec<usize>,
    selected: Vec<usize>,
    reason: Vec<ForecastWait>,
    queued: VecDeque<f64>,
    scratch: ForecastPassScratch,
    /// Per task, this iteration: first start hour and start rank, and
    /// whether it finished by the horizon.
    pub(super) start: Vec<Option<f64>>,
    pub(super) rank: Vec<Option<f64>>,
    pub(super) finished: Vec<bool>,
}

impl ScheduleState {
    pub(super) fn new(admission: &ForecastAdmission) -> Self {
        let len = admission.len();
        Self {
            running: ForecastTaskSet::with_len(len),
            heap: BinaryHeap::new(),
            indegree: vec![0; len],
            pending: Vec::with_capacity(len),
            ready: Vec::with_capacity(len),
            selected: Vec::new(),
            reason: vec![ForecastWait::Capacity; len],
            queued: VecDeque::new(),
            scratch: admission.scratch(),
            start: vec![None; len],
            rank: vec![None; len],
            finished: vec![false; len],
        }
    }
}

fn wait_slot(wait: ForecastWait) -> usize {
    ForecastWait::ALL
        .iter()
        .position(|one| *one == wait)
        .unwrap_or(0)
}

/// The longest dependency path through `hours`, ignoring contention.
fn critical_path(scenario: &Scenario<'_>, hours: &[Option<f64>]) -> Option<f64> {
    let mut longest = vec![0.0f64; hours.len()];
    let mut overall = 0.0f64;
    for &index in &scenario.topological {
        let before = scenario.edges[index]
            .iter()
            .map(|dep| longest[*dep])
            .fold(0.0, f64::max);
        longest[index] = before + hours[index]?;
        overall = overall.max(longest[index]);
    }
    Some(overall)
}

/// Simulate one drain of `scenario` with this iteration's `hours` per task.
/// Arrived work adds its draws to `totals`.
pub(super) fn simulate_drain(
    scenario: &Scenario<'_>,
    hours: &[Option<f64>],
    arrivals: Option<&Arrivals<'_>>,
    rng: &mut Rng,
    totals: &mut Draw,
    state: &mut ScheduleState,
) -> Run {
    let admission = scenario.admission;
    state.start.fill(None);
    state.rank.fill(None);
    state.finished.fill(false);
    let included: &[bool] = &scenario.included;
    let scheduled = move || (0..hours.len()).filter(move |index| included[*index]);
    let unavailable = Run {
        drained: None,
        critical_path: None,
        parallelism: None,
        waits: None,
        unfinished: 0,
        truncated: false,
    };
    if scheduled().any(|index| hours[index].is_none()) {
        return unavailable;
    }
    let hours_of = |index: usize| hours[index].unwrap_or(0.0);
    state.running.clear();
    state.heap.clear();
    state.queued.clear();
    state.pending.clear();
    let mut running = 0usize;
    for index in scheduled() {
        state.indegree[index] = scenario.edges[index].len();
        if scenario.in_progress[index] {
            state.running.insert(index);
            running += 1;
            state.start[index] = Some(0.0);
            state.rank[index] = Some(0.0);
            state.heap.push(End(hours_of(index), index));
        }
    }
    state.pending.extend(
        admission
            .dispatch_order()
            .iter()
            .copied()
            .filter(|index| scenario.included[*index] && !scenario.in_progress[*index]),
    );
    let arrivals = arrivals.filter(|a| a.per_day > 0.0 && !a.history.is_empty());
    let rate_per_hour = arrivals.map_or(0.0, |a| a.per_day / 24.0);
    let mut next_arrival = arrivals.map(|_| -rng.unit().ln() / rate_per_hour);

    let mut at = 0.0f64;
    let mut started = 0.0f64;
    let mut occupied = 0.0f64;
    let mut waits = [0.0f64; 5];
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
                scenario.slots.saturating_sub(running),
                scenario.slots,
                at,
                &mut state.scratch,
                &mut state.selected,
                |index, wait| reason[index] = wait,
            );
            for &index in &state.selected {
                started += 1.0;
                state.start[index] = Some(at);
                state.rank[index] = Some(started);
                state.running.insert(index);
                running += 1;
                state.heap.push(End(at + hours_of(index), index));
            }
            if state.selected.is_empty() {
                break;
            }
            let start = &state.start;
            state.pending.retain(|index| start[*index].is_none());
            if running >= scenario.slots {
                break;
            }
        }
        while running < scenario.slots
            && let Some(work) = state.queued.pop_front()
        {
            running += 1;
            state.heap.push(End(at + work, ARRIVAL));
        }
        if state.pending.is_empty() && state.queued.is_empty() && running == 0 {
            break Some(at);
        }

        let deadline = state
            .pending
            .iter()
            .filter_map(|index| admission.footprint_deadline(*index))
            .filter(|deadline| *deadline > at)
            .fold(f64::INFINITY, f64::min);
        let next = state
            .heap
            .peek()
            .map_or(f64::INFINITY, |end| end.0)
            .min(deadline)
            .min(next_arrival.unwrap_or(f64::INFINITY));
        let until = next.min(HORIZON_HOURS);
        let span = (until - at).max(0.0);
        occupied += running as f64 * span;
        for &index in &state.pending {
            waits[wait_slot(state.reason[index])] += span;
        }
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
            if index == ARRIVAL {
                continue;
            }
            state.running.remove(index);
            state.finished[index] = true;
            for dependent in &scenario.dependents[index] {
                state.indegree[*dependent] -= 1;
            }
        }
        if let (Some(arrives), Some(arrivals)) = (next_arrival, arrivals)
            && arrives <= at
        {
            let delivery = &arrivals.history[rng.index(arrivals.history.len())];
            totals.add(&Draw {
                hours: Some(delivery.hours),
                tokens: delivery.tokens,
                usd: delivery.usd,
            });
            state.queued.push_back(delivery.hours);
            next_arrival = Some(at - rng.unit().ln() / rate_per_hour);
        }
    };
    let length = drained.unwrap_or(at);
    Run {
        drained,
        critical_path: critical_path(scenario, hours),
        parallelism: Some(if length > 0.0 { occupied / length } else { 0.0 }),
        waits: Some(waits),
        unfinished: scheduled().filter(|index| !state.finished[*index]).count(),
        truncated,
    }
}
