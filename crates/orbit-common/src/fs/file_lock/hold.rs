use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(doc)]
use super::FileLockOptions;

/// Most steps one held-past-threshold report lists before summarizing the rest.
const MAX_REPORTED_HOLD_STEPS: usize = 6;

/// A nested lock wait shorter than this is left in its enclosing step's own
/// time rather than recorded as a step of its own, so an uncontended
/// acquisition per bundle costs no allocation.
const MIN_RECORDED_LOCK_WAIT: Duration = Duration::from_millis(1);

/// The step a hold report names for time no step on this thread claimed.
const UNATTRIBUTED_HOLD_STEP: &str = "unattributed";

/// What a holder needs to say on release that it held the lock too long.
#[derive(Debug)]
pub(super) struct HoldReport {
    lock_path: PathBuf,
    label: String,
    exclusive: bool,
    acquired: Instant,
    warn_after: Duration,
}

impl HoldReport {
    pub(super) fn new(
        lock_path: &Path,
        label: &str,
        exclusive: bool,
        warn_after: Duration,
    ) -> Self {
        HOLD_STEPS.with(|steps| steps.borrow_mut().open_holds += 1);
        Self {
            lock_path: lock_path.to_path_buf(),
            label: label.to_string(),
            exclusive,
            acquired: Instant::now(),
            warn_after,
        }
    }

    pub(super) fn report(&self) {
        let held = self.acquired.elapsed();
        let breakdown = HOLD_STEPS.with(|steps| {
            let mut steps = steps.borrow_mut();
            let breakdown =
                (held >= self.warn_after).then(|| steps.breakdown_since(self.acquired, held));
            steps.open_holds = steps.open_holds.saturating_sub(1);
            if steps.open_holds == 0 {
                steps.done.clear();
            }
            breakdown
        });
        let Some(breakdown) = breakdown else {
            return;
        };
        crate::tracing::warn!(
            target: "orbit.common.fs.file_lock",
            lock_path = %self.lock_path.display(),
            label = %self.label,
            mode = if self.exclusive { "exclusive" } else { "shared" },
            held_ms = duration_millis(held),
            threshold_ms = duration_millis(self.warn_after),
            slowest_step = %breakdown.slowest,
            slowest_step_ms = duration_millis(breakdown.slowest_time),
            steps = %breakdown.steps,
            "advisory file lock held past its threshold",
        );
    }
}

thread_local! {
    /// Steps timed on this thread while it holds a lock that reports long
    /// holds; see [`hold_step`].
    static HOLD_STEPS: RefCell<HoldSteps> = RefCell::new(HoldSteps::default());
}

#[derive(Default)]
struct HoldSteps {
    /// Reporting holds this thread has open. Steps are recorded only while
    /// one is, and forgotten once the last closes.
    open_holds: usize,
    /// Time already claimed by nested steps, one entry per step in progress,
    /// innermost last.
    nested: Vec<Duration>,
    /// Finished steps, in completion order.
    done: Vec<FinishedHoldStep>,
}

struct FinishedHoldStep {
    name: Cow<'static, str>,
    started: Instant,
    /// The step's own time: its duration less its nested steps'.
    own: Duration,
}

/// What a long hold spent its time on: the step that cost the most, and each
/// step's total, largest first.
struct HoldBreakdown {
    slowest: Cow<'static, str>,
    slowest_time: Duration,
    steps: String,
}

impl HoldSteps {
    /// Close the innermost open step, crediting its whole duration to the
    /// step it ran inside.
    fn finish(&mut self, name: Cow<'static, str>, started: Instant, elapsed: Duration) {
        let nested = self.nested.pop().unwrap_or_default();
        if let Some(parent) = self.nested.last_mut() {
            *parent += elapsed;
        }
        if self.open_holds > 0 {
            self.done.push(FinishedHoldStep {
                name,
                started,
                own: elapsed.saturating_sub(nested),
            });
        }
    }

    /// Total own time per step name for steps that started inside a hold
    /// acquired at `acquired`, plus whatever `held` no step accounts for.
    fn breakdown_since(&self, acquired: Instant, held: Duration) -> HoldBreakdown {
        let mut totals: BTreeMap<Cow<'static, str>, Duration> = BTreeMap::new();
        for step in self.done.iter().filter(|step| step.started >= acquired) {
            *totals.entry(step.name.clone()).or_default() += step.own;
        }
        let attributed = totals.values().sum::<Duration>();
        let unattributed = held.saturating_sub(attributed);
        if !unattributed.is_zero() {
            *totals
                .entry(Cow::Borrowed(UNATTRIBUTED_HOLD_STEP))
                .or_default() += unattributed;
        }
        let mut ranked = totals.into_iter().collect::<Vec<_>>();
        ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let (slowest, slowest_time) = ranked
            .first()
            .cloned()
            .unwrap_or((Cow::Borrowed(UNATTRIBUTED_HOLD_STEP), held));
        let mut steps = ranked
            .iter()
            .take(MAX_REPORTED_HOLD_STEPS)
            .map(|(name, time)| format!("{name}={}ms", duration_millis(*time)))
            .collect::<Vec<_>>()
            .join(", ");
        if ranked.len() > MAX_REPORTED_HOLD_STEPS {
            steps.push_str(&format!(
                ", {} more",
                ranked.len() - MAX_REPORTED_HOLD_STEPS
            ));
        }
        HoldBreakdown {
            slowest,
            slowest_time,
            steps,
        }
    }
}

/// Run `op` as one named step of every lock hold on this thread that reports
/// long holds ([`FileLockOptions::warn_held_after`]), so a hold that passes
/// its threshold names the step that cost the most and its milliseconds.
///
/// A step's time excludes the steps nested inside it, and steps sharing a
/// name add up, so a step run once per task reports its total. A wait for
/// another lock inside a reporting hold is recorded as a step of its own,
/// named after that lock file. Outside any reporting hold this only runs
/// `op`.
pub fn hold_step<T>(name: &'static str, op: impl FnOnce() -> T) -> T {
    /// Closes the step on drop, so an unwinding `op` still pops its entry.
    struct OpenStep {
        name: &'static str,
        started: Instant,
    }
    impl Drop for OpenStep {
        fn drop(&mut self) {
            let elapsed = self.started.elapsed();
            HOLD_STEPS.with(|steps| {
                steps
                    .borrow_mut()
                    .finish(Cow::Borrowed(self.name), self.started, elapsed);
            });
        }
    }

    let tracked = HOLD_STEPS.with(|steps| {
        let mut steps = steps.borrow_mut();
        if steps.open_holds == 0 {
            return false;
        }
        steps.nested.push(Duration::ZERO);
        true
    });
    if !tracked {
        return op();
    }
    let _step = OpenStep {
        name,
        started: Instant::now(),
    };
    op()
}

/// Record a wait for `lock_path` as a step of the reporting holds this
/// thread is inside. Waits too short to matter stay in the enclosing step.
/// The wait precedes the lock's own hold, so only enclosing holds count it.
pub(super) fn record_lock_wait(lock_path: &Path, started: Instant) {
    let waited = started.elapsed();
    if waited < MIN_RECORDED_LOCK_WAIT {
        return;
    }
    HOLD_STEPS.with(|steps| {
        let mut steps = steps.borrow_mut();
        if steps.open_holds == 0 {
            return;
        }
        let lock = lock_path.file_name().map_or_else(
            || lock_path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        steps.nested.push(Duration::ZERO);
        steps.finish(Cow::Owned(format!("{lock} wait")), started, waited);
    });
}

pub(super) fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}
