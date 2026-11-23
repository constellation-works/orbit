use orbit_types::task::Task;

use crate::OrbitRuntime;
use crate::application::review::gate::context::GateContext;
use crate::application::review::gate::judgement::ExternalMerge;

/// [ORB-26599] The merge of the PR a before-landing review judged, when it
/// merged outside Orbit before the review settled. `None` when the task has
/// no PR, the PR is not merged, or the forge cannot say: a settlement never
/// fails on the lookup, it only loses the merge note.
pub(super) fn merged_outside_orbit(
    runtime: &OrbitRuntime,
    context: &GateContext,
    task: &Task,
) -> Option<ExternalMerge> {
    let number = task.github_pr_number()?.parse::<u64>().ok()?;
    match runtime
        .task_pr_forge()
        .merged_pull_request(&runtime.paths().repo_root, number)
    {
        Ok(merged) => merged.map(|merged| ExternalMerge {
            merge_commit: merged.merge_commit,
            worktree: context.workspace_path.display().to_string(),
        }),
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.review",
                run_id = %context.run_id,
                task_id = %task.id,
                pr_number = number,
                "could not read whether the reviewed PR merged: {error}"
            );
            None
        }
    }
}
