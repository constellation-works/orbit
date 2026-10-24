use chrono::Utc;
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{
    JobRun, PrAutoFixBlocker, PrAutoFixClaim, PrAutoFixDecision, decide_pr_auto_fix,
    pr_auto_fix_attempts,
};

use super::admission::pr_auto_fix_budget;
use super::adoption::{Verdict, out_of_run_starts};

/// When a run last moved, as the store's recency order reads it.
pub(super) fn run_recency(run: &JobRun) -> chrono::DateTime<Utc> {
    run.finished_at.or(run.started_at).unwrap_or(run.created_at)
}

/// `Err` with the skip when the pull request's ledger would not admit a
/// repair of `head_sha` for `blocker`, or an out-of-run auto-fix already
/// resumed delivery for it, or for as many heads as the budget allows.
pub(super) fn ledger_admits(
    history: &[TaskHistoryEntry],
    pr_number: &str,
    head_sha: &str,
    blocker: PrAutoFixBlocker,
) -> Result<(), Verdict> {
    let budget = pr_auto_fix_budget(history, pr_number)
        .map_err(|error| Verdict::Skip("store_unreadable", error.to_string()))?;
    let attempts = pr_auto_fix_attempts(history, pr_number);
    let claim = PrAutoFixClaim {
        pr_number,
        head_sha,
        blocker,
        run_id: "",
    };
    match decide_pr_auto_fix(&attempts, &claim, budget, "", "") {
        PrAutoFixDecision::Admit(_) | PrAutoFixDecision::Continue(_) => {}
        PrAutoFixDecision::Duplicate(previous) => {
            return Err(Verdict::Skip(
                "already_repaired",
                format!(
                    "run {} already repaired {} on {head_sha} (attempt {} of {})",
                    previous.run_id,
                    blocker.as_str(),
                    previous.attempt,
                    previous.budget
                ),
            ));
        }
        PrAutoFixDecision::Exhausted(attempts) => {
            return Err(Verdict::Skip(
                "budget_exhausted",
                format!("#{pr_number} spent its {} auto-fix repairs", attempts.len()),
            ));
        }
    }
    let starts = out_of_run_starts(history, pr_number);
    if let Some(start) = starts
        .iter()
        .find(|start| start.head_sha == head_sha && start.blocker == blocker)
    {
        return Err(Verdict::Skip(
            "already_started",
            format!(
                "run {} already resumed delivery to repair {} on {head_sha}",
                start.resumed_run_id,
                blocker.as_str()
            ),
        ));
    }
    if u32::try_from(starts.len()).unwrap_or(u32::MAX) >= budget {
        return Err(Verdict::Skip(
            "budget_exhausted",
            format!(
                "#{pr_number} was handed back to delivery {} times, its auto-fix budget",
                starts.len()
            ),
        ));
    }
    Ok(())
}
