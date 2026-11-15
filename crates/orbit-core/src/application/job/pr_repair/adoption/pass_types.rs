use orbit_types::task::TaskStatus;
use orbit_types::workflow::{DeliveryRefusal, JobRun, PrAutoFixBlocker};
use serde_json::{Value, json};
use std::time::Duration;

/// Delivery leaves read for one candidate task, newest first, to find the
/// newest one that carries only that task. A leaf that carries several tasks
/// is no task's own, so a few are read past.
pub(in crate::application::job::pr_repair) const TASK_LEAF_SCAN: usize = 8;

/// How long a pull request found without a blocker is not read again.
pub(in crate::application::job::pr_repair) const CLEAN_RECHECK: Duration = Duration::from_secs(300);

/// A repair the pass decided to start.
pub(in crate::application::job::pr_repair) struct Repair {
    pub(super) task_id: String,
    pub(super) status: TaskStatus,
    pub(super) pr_number: String,
    pub(super) head_sha: String,
    /// What the repair resumes delivery for; `None` for a held hand-back on
    /// a pull request with no blocker.
    pub(super) blocker: Option<PrAutoFixBlocker>,
    /// The delivery hold a held hand-back resumes completion from.
    pub(super) hold: Option<DeliveryRefusal>,
    /// The run input record of the repair.
    pub(super) marker: Value,
}

impl Repair {
    /// The decided repair of `run`'s pull request, before it starts.
    pub(in crate::application::job::pr_repair) fn summary(&self, run: &JobRun) -> Value {
        json!({
            "task_id": self.task_id,
            "source_run_id": run.run_id,
            "pr_number": self.pr_number,
            "head_sha": self.head_sha,
            "blocker": self.blocker.map(PrAutoFixBlocker::as_str),
            "hold_code": self.hold.map(DeliveryRefusal::error_code),
        })
    }
}

/// What one pass does with one task's newest delivery leaf.
pub(in crate::application::job::pr_repair) enum Verdict {
    /// Not a drain pass's to report: the task has no Orbit pull request at
    /// rest, or its leaf is live, finished or someone else's. The reason is
    /// reported only when one task's repair was asked for.
    Ignore(&'static str, String),
    Skip(&'static str, String),
    Repair(Box<Repair>),
    Started(Value),
}

/// Who starts a repair, and the completion authority it holds.
pub(in crate::application::job::pr_repair) struct RepairAuthority<'a> {
    /// The drain run, recorded as the resume's actor; `None` for a start
    /// that no drain makes.
    pub(in crate::application::job::pr_repair) drain_run_id: Option<&'a str>,
    /// The resume's actor when no drain makes it.
    pub(in crate::application::job::pr_repair) actor: Option<&'a str>,
    pub(in crate::application::job::pr_repair) completion_done: bool,
}

/// Reads one pass shares across its candidates.
#[derive(Default)]
pub(in crate::application::job::pr_repair) struct PassReads {
    /// Read the forge even for a pull request found unblocked within
    /// [`CLEAN_RECHECK`]: an operator asking about one task wants a fresh
    /// answer.
    pub(super) fresh: bool,
    /// The first forge read failure of this pass.
    pub(super) forge_error: Option<String>,
    /// The delivery identities, once read.
    pub(super) authors: Option<Result<Vec<String>, String>>,
    /// The activity catalog's load result, once read.
    pub(super) catalog: Option<Result<(), String>>,
}

/// Facts learned before a candidate is refused. A policy hold can precede
/// the forge read, so a missing blocker must remain unknown.
#[derive(Default)]
pub(in crate::application::job::pr_repair) struct RepairObservation {
    pub(in crate::application::job::pr_repair) pr_number: Option<String>,
    pub(in crate::application::job::pr_repair) blocker: Option<PrAutoFixBlocker>,
    /// The delivery hold of a held leaf the pass considered.
    pub(in crate::application::job::pr_repair) hold_code: Option<&'static str>,
}

impl PassReads {
    /// Reads for one operator request, which ignore [`CLEAN_RECHECK`].
    pub(in crate::application::job::pr_repair) fn fresh() -> Self {
        Self {
            fresh: true,
            ..Self::default()
        }
    }
}

pub(in crate::application::job::pr_repair) enum BlockerRead {
    /// The forge could read the pull request's blockers (or determined it is
    /// no longer open).
    Complete(Option<crate::runtime::task_pr_forge::PullRequestBlockers>),
    /// GitHub is still calculating this pull request's mergeability. This is
    /// local to that candidate and must not defer the rest of the pass.
    MergeabilityUnknown,
}
