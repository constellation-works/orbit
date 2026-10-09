//! Committing and adopting reviewer repairs, widening selectors they reach.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_common::fs::selector::overlaps;
use orbit_engine::review_gate::{
    REVIEW_ATTEMPT_TRAILER, commit_reviewer_repairs, uncommitted_paths,
};
use orbit_types::task::{ContextWideningStep, Task};
use orbit_types::workflow::{CommitIdentity, FindingDisposition, ReviewAttempt, ReviewerIdentity};

use crate::OrbitRuntime;

use super::super::context::GateContext;

use super::Judgement;

impl Judgement {
    /// Commit whatever the reviewer changed as its own attributed work: the
    /// candidate's one reviewer commit, `review: <summary>`, on top of the
    /// untouched implementation commits [ORB-13989].
    ///
    /// A reviewer may change any path a fix requires: a changed path no
    /// task selector covers widens the reviewed task's `context_files`, with
    /// review provenance in its history, and does not abandon the review.
    pub(in crate::application::review::gate) fn commit_repairs(
        &mut self,
        runtime: &OrbitRuntime,
        context: &mut GateContext,
        reviewer: &ReviewerIdentity,
        attempt: &ReviewAttempt,
    ) -> Result<Option<CommitIdentity>, OrbitError> {
        let changed = uncommitted_paths(&context.workspace_path)?;
        if changed.is_empty() {
            return Ok(None);
        }
        let out_of_scope = out_of_scope_paths(&changed, &context.tasks);
        if !out_of_scope.is_empty() {
            self.widen_reviewer_selectors(runtime, context, &out_of_scope)?;
        }
        let finding_ids = self
            .findings
            .iter()
            .filter(|finding| finding.disposition == FindingDisposition::Repaired)
            .map(|finding| finding.id.clone())
            .collect::<Vec<_>>();
        let message = format!(
            "review: {} [{}]\n\nFindings: {}\nPaths: {}\n{REVIEW_ATTEMPT_TRAILER}: {}\nOrbit-Review-Crew: {}",
            if self.summary.trim().is_empty() {
                "reviewer repairs".to_string()
            } else {
                self.summary
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string()
            },
            context.task_ids.join(", "),
            if finding_ids.is_empty() {
                "none named".to_string()
            } else {
                finding_ids.join(", ")
            },
            changed.join(", "),
            attempt.attempt_id,
            reviewer.crew,
        );
        // The provider names the agent family the repair commit is attributed
        // to; the model alone may carry no family hint.
        let commit = commit_reviewer_repairs(
            &context.workspace_path,
            &repair_author_label(reviewer),
            &message,
        )?;
        Ok(commit)
    }

    /// Adopt the repair commit an interrupted settlement of this attempt
    /// already made, widening its paths exactly as [`Self::commit_repairs`]
    /// did before committing. The interrupted run widened selectors before
    /// committing, so a repair path outside the admitted selectors is
    /// reported as widened even though it is in scope by now.
    pub(in crate::application::review::gate) fn adopt_repairs(
        &mut self,
        runtime: &OrbitRuntime,
        context: &mut GateContext,
        committed: &[String],
        admitted_selectors: &BTreeMap<String, Vec<String>>,
    ) -> Result<(), OrbitError> {
        let out_of_scope = out_of_scope_paths(committed, &context.tasks);
        if !out_of_scope.is_empty() {
            self.widen_reviewer_selectors(runtime, context, &out_of_scope)?;
        }
        let admitted_tasks = context
            .tasks
            .iter()
            .map(|task| {
                let mut admitted = task.clone();
                if let Some(selectors) = admitted_selectors.get(task.id.as_str()) {
                    admitted.context_files = selectors.clone();
                }
                admitted
            })
            .collect::<Vec<_>>();
        for path in out_of_scope_paths(committed, &admitted_tasks) {
            let selector = format!("file:{}", normalize_git_path(&path));
            if !self.selectors_widened.contains(&selector) {
                self.selectors_widened.push(selector);
            }
        }
        Ok(())
    }

    /// Append `file:<path>` selectors for reviewer-changed paths no task
    /// covers to the reviewed (first) task — the task whose agent changed
    /// them — with review provenance in its history, then bind the
    /// certificate to the post-widening task-meaning digest.
    fn widen_reviewer_selectors(
        &mut self,
        runtime: &OrbitRuntime,
        context: &mut GateContext,
        paths: &[String],
    ) -> Result<(), OrbitError> {
        let Some(task_id) = context.tasks.first().map(|task| task.id.clone()) else {
            return Ok(());
        };
        let paths = paths
            .iter()
            .map(|path| normalize_git_path(path))
            .collect::<Vec<_>>();
        if context.claimed {
            // A claim's footprint is fixed until its handoff: the owner
            // widens it then, for every path the candidate changed outside
            // it, and the certificate reports what that will add.
            for path in &paths {
                let selector = format!("file:{path}");
                if !self.selectors_widened.contains(&selector) {
                    self.selectors_widened.push(selector);
                }
            }
            return Ok(());
        }
        let widened = runtime.widen_context_files_for_paths(
            &task_id,
            &context.run_id,
            ContextWideningStep::Review,
            "review_gate_settle",
            &paths,
        )?;
        if widened.is_empty() {
            return Ok(());
        }
        context.tasks[0] = runtime.get_task(&task_id)?;
        context.refresh_task_digests()?;
        self.task_meaning_digest = context.task_digests.1.clone();
        self.selectors_widened = widened;
        Ok(())
    }
}

/// A repair path is in scope when a task selector's filesystem anchor names
/// it or a directory/legacy selector contains it. Matching uses the shared
/// selector grammar, so `symbol:<path>#<symbol>:<kind>` authorizes the
/// backing file even when `<symbol>` contains `::`.
fn path_in_scope(path: &str, tasks: &[Task]) -> bool {
    let path = path.trim_start_matches("./");
    let changed = format!("file:{path}");
    tasks.iter().any(|task| {
        task.context_files
            .iter()
            .any(|selector| overlaps(selector, &changed))
    })
}

/// Repair paths no task's selectors cover.
fn out_of_scope_paths(paths: &[String], tasks: &[Task]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| !path_in_scope(path, tasks))
        .cloned()
        .collect()
}

fn normalize_git_path(path: &str) -> String {
    path.trim().trim_start_matches("./").to_string()
}

/// The author label a reviewer's repair commit is attributed to.
pub(in crate::application::review::gate) fn repair_author_label(
    reviewer: &ReviewerIdentity,
) -> String {
    format!("{} / {}", reviewer.provider, reviewer.model)
}
