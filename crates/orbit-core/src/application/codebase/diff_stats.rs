//! Lines added and deleted per landed delivery [ORB-18383], split into code,
//! tests and other by `workflow.test_paths` and `workflow.non_code_paths`.
//!
//! The owner writes one row when a delivery lands, from one of two points:
//!
//! - a local fast-forward (local ship mode, or an owner landing a local
//!   handoff), which knows the base the branch moved from and diffs
//!   `before..after`;
//! - a task's transition into done with a landing receipt in its history (a
//!   pull request merged by the pipeline, an owner handoff landing, or a pull
//!   request merged outside Orbit), which diffs the landed commit against its
//!   first parent. Pull requests land by squash or merge commit, so the first
//!   parent is the base they landed on and refreshes merged into the branch
//!   are not counted. A rebase merge of several commits counts only its last.
//!
//! Rows are keyed on workspace, task and landed commit, so a retried landing
//! or a second recording point keeps the first row. Recording never fails the
//! landing it describes: a missing commit or a store error is logged.
//!
//! The backfill reads the same receipts (and, for a local landing without
//! one, the delivery run's observed landing) for done tasks that have no row,
//! diffs only commits the local checkout already holds, and counts the rest.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::path_filter::PathFilter;
use orbit_store::contracts::{
    DiffClassCounts, LandedDiffSource, LandedDiffStats, LandedDiffStoreBackend as _,
};
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::automation::DirectLandingRequest;
use serde::Serialize;

use crate::OrbitRuntime;

/// Done tasks one backfill diffs and records at most.
pub const DIFF_STATS_BACKFILL_DEFAULT_LIMIT: usize = 500;
/// The most rows one backfill may be asked to record.
pub const DIFF_STATS_BACKFILL_MAX_LIMIT: usize = 5_000;

/// What one backfill pass did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DiffStatsBackfill {
    /// Done tasks without a row that the pass looked at.
    pub examined: usize,
    /// Rows written.
    pub recorded: usize,
    /// Done tasks with no landing receipt or observed landing: nothing to diff.
    pub no_landing: usize,
    /// Done tasks whose landed commit is not in the local checkout.
    pub unresolvable: usize,
    /// Done tasks without a row left for a later pass once `limit` was reached.
    pub remaining: usize,
    pub limit: usize,
}

/// The landed change for one task: what to diff and what to label it with.
struct Landing<'a> {
    task_id: &'a str,
    landed_commit: &'a str,
    /// `None` diffs the landed commit against its first parent.
    base_commit: Option<&'a str>,
    landed_at: DateTime<Utc>,
    job_run_id: Option<&'a str>,
    source: LandedDiffSource,
}

/// The class counts of one diff.
#[derive(Default)]
struct Counts {
    code: DiffClassCounts,
    tests: DiffClassCounts,
    other: DiffClassCounts,
}

impl Counts {
    fn add(&mut self, other: &Self) {
        for (into, from) in [
            (&mut self.code, &other.code),
            (&mut self.tests, &other.tests),
            (&mut self.other, &other.other),
        ] {
            into.added += from.added;
            into.deleted += from.deleted;
            into.files += from.files;
        }
    }
}

impl OrbitRuntime {
    /// The workspace's landed-delivery rows that landed at or after `since`
    /// (every row when `None`), oldest first.
    pub fn landed_diff_stats(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<LandedDiffStats>, OrbitError> {
        self.sqlite_store()?
            .landed_diff_stats_since(&self.workspace_id()?, since)
    }

    /// Record the diff stats of a local fast-forward that moved its branch
    /// from `before_commit` to `after_commit`. One task takes the whole
    /// range; several share it by the commits whose message names exactly
    /// one of them.
    pub(crate) fn record_direct_landing_diff_stats(&self, request: &DirectLandingRequest) {
        if let Err(error) = self.try_record_direct_landing(request) {
            tracing::warn!(
                run_id = %request.run_id,
                %error,
                "recording a local landing's diff stats failed"
            );
        }
    }

    fn try_record_direct_landing(&self, request: &DirectLandingRequest) -> Result<(), OrbitError> {
        let task_ids: Vec<String> = if request.task_ids.is_empty() {
            self.show_job_run(&request.run_id)?
                .input
                .as_ref()
                .map(crate::runtime::run_input::recorded_task_ids)
                .unwrap_or_default()
                .into_iter()
                .map(str::to_owned)
                .collect()
        } else {
            request.task_ids.clone()
        };
        let repo = git2::Repository::open(&self.paths().repo_root)
            .map_err(|error| OrbitError::Execution(format!("open repository: {error}")))?;
        let classes = self.diff_classes();
        let now = Utc::now();
        let per_task: BTreeMap<String, Counts> = match task_ids.as_slice() {
            [] => return Ok(()),
            [only] => {
                let counts = diff_counts(
                    &repo,
                    Some(&request.before_commit),
                    &request.after_commit,
                    &classes,
                )
                .ok_or_else(|| {
                    OrbitError::Execution(format!(
                        "landed range {}..{} is not readable",
                        request.before_commit, request.after_commit
                    ))
                })?
                .1;
                BTreeMap::from([(only.clone(), counts)])
            }
            many => split_by_commit_message(&repo, request, many, &classes)?,
        };
        for (task_id, counts) in per_task {
            let task = self.get_task(&task_id)?;
            self.write_row(
                &task,
                &Landing {
                    task_id: &task_id,
                    landed_commit: &request.after_commit,
                    base_commit: Some(&request.before_commit),
                    landed_at: now,
                    job_run_id: Some(&request.run_id),
                    source: LandedDiffSource::Landing,
                },
                &request.before_commit,
                counts,
            )?;
        }
        Ok(())
    }

    /// On a task's transition into done, record the landing its history's
    /// receipt names. The merge commit of a pull request lives on the remote,
    /// so it is fetched when the checkout lacks it.
    pub(crate) fn record_done_landing_diff_stats(&self, previous_status: TaskStatus, task: &Task) {
        if task.status != TaskStatus::Done || previous_status == TaskStatus::Done {
            return;
        }
        let result = (|| -> Result<(), OrbitError> {
            let history = self.get_task_history(&task.id)?;
            let Some((commit, landed_at)) = super::landed::receipt(&history) else {
                return Ok(());
            };
            let repo_root = &self.paths().repo_root;
            if let Err(error) = orbit_engine::review_gate::fetch_landed_commit(repo_root, &commit) {
                tracing::warn!(task_id = %task.id, %commit, %error, "landed commit could not be fetched");
            }
            let repo = git2::Repository::open(repo_root)
                .map_err(|error| OrbitError::Execution(format!("open repository: {error}")))?;
            let written = self.record_landing(
                &repo,
                task,
                &Landing {
                    task_id: &task.id,
                    landed_commit: &commit,
                    base_commit: None,
                    landed_at,
                    job_run_id: task.job_run_id.as_deref(),
                    source: LandedDiffSource::Landing,
                },
            )?;
            if !written {
                tracing::warn!(
                    task_id = %task.id,
                    %commit,
                    "landed commit is not in the local checkout; no diff stats recorded"
                );
            }
            Ok(())
        })();
        if let Err(error) = result {
            tracing::warn!(task_id = %task.id, %error, "recording a landing's diff stats failed");
        }
    }

    /// Fill rows for up to `limit` done tasks that landed before diff stats
    /// were recorded, newest first. `limit` bounds the diffs; tasks without
    /// a landing or a local commit cost one history read each and are only
    /// counted. Only commits the local checkout already holds are diffed;
    /// nothing is fetched. Running it again skips every task that has a row.
    pub fn backfill_landed_diff_stats(
        &self,
        limit: usize,
    ) -> Result<DiffStatsBackfill, OrbitError> {
        if limit == 0 || limit > DIFF_STATS_BACKFILL_MAX_LIMIT {
            return Err(OrbitError::InvalidInput(format!(
                "backfill limit must be between 1 and {DIFF_STATS_BACKFILL_MAX_LIMIT}"
            )));
        }
        let recorded: BTreeSet<String> = self
            .sqlite_store()?
            .landed_diff_task_ids(&self.workspace_id()?)?;
        let mut done: Vec<Task> = self
            .list_task_metadata()?
            .into_iter()
            .filter(|task| task.status == TaskStatus::Done && !recorded.contains(task.id.as_str()))
            .collect();
        done.sort_by_key(|task| std::cmp::Reverse(task.updated_at));
        let mut report = DiffStatsBackfill {
            limit,
            ..DiffStatsBackfill::default()
        };
        let repo = git2::Repository::open(&self.paths().repo_root)
            .map_err(|error| OrbitError::Execution(format!("open repository: {error}")))?;
        let mut scanner = super::landed::Scanner::new(self);
        for (index, task) in done.iter().enumerate() {
            if report.recorded == limit {
                report.remaining = done.len() - index;
                break;
            }
            report.examined += 1;
            let history = match self.get_task_history(&task.id) {
                Ok(history) => history,
                Err(error) => {
                    tracing::warn!(task_id = %task.id, %error, "backfill: task history unreadable");
                    report.unresolvable += 1;
                    continue;
                }
            };
            let landed = super::landed::receipt(&history).or_else(|| {
                scanner
                    .observe(self, &task.id)
                    .map(|landed| (landed.commit, landed.at))
            });
            let Some((commit, landed_at)) = landed else {
                report.no_landing += 1;
                continue;
            };
            let written = self.record_landing(
                &repo,
                task,
                &Landing {
                    task_id: &task.id,
                    landed_commit: &commit,
                    base_commit: None,
                    landed_at,
                    job_run_id: task.job_run_id.as_deref(),
                    source: LandedDiffSource::Backfill,
                },
            )?;
            if written {
                report.recorded += 1;
            } else {
                report.unresolvable += 1;
            }
        }
        Ok(report)
    }

    /// Diff `landing` and write its row. `false` when its commits are not
    /// in the checkout.
    fn record_landing(
        &self,
        repo: &git2::Repository,
        task: &Task,
        landing: &Landing<'_>,
    ) -> Result<bool, OrbitError> {
        let Some((base, counts)) = diff_counts(
            repo,
            landing.base_commit,
            landing.landed_commit,
            &self.diff_classes(),
        ) else {
            return Ok(false);
        };
        self.write_row(task, landing, &base, counts)?;
        Ok(true)
    }

    fn write_row(
        &self,
        task: &Task,
        landing: &Landing<'_>,
        base_commit: &str,
        counts: Counts,
    ) -> Result<(), OrbitError> {
        // The implementing crew and model as the delivery run resolved them;
        // the task's own crew when that run is not on this host.
        let run = landing
            .job_run_id
            .and_then(|run_id| self.show_job_run(run_id).ok());
        let crew = run
            .as_ref()
            .and_then(|run| run.resolved_crew.clone())
            .or_else(|| task.crew.clone());
        let model = run.and_then(|run| run.crew_model);
        self.sqlite_store()?
            .record_landed_diff_stats(&LandedDiffStats {
                workspace_id: self.workspace_id()?,
                task_id: landing.task_id.to_string(),
                landed_commit: landing.landed_commit.to_string(),
                base_commit: base_commit.to_string(),
                job_run_id: landing.job_run_id.map(str::to_owned),
                landed_at: landing.landed_at,
                recorded_at: Utc::now(),
                source: landing.source,
                crew,
                model,
                complexity: task
                    .complexity
                    .map(|complexity| complexity.as_str().to_string()),
                task_type: Some(task.task_type.to_string()),
                code: counts.code,
                tests: counts.tests,
                other: counts.other,
            })?;
        Ok(())
    }

    fn diff_classes(&self) -> DiffClasses<'_> {
        DiffClasses {
            tests: self.diff_test_paths(),
            non_code: self.diff_non_code_paths(),
        }
    }
}

/// How a changed path is classed: tests first, then other, else code.
struct DiffClasses<'a> {
    tests: &'a PathFilter,
    non_code: &'a PathFilter,
}

/// Split a multi-task landing by the first-parent commits in its range whose
/// message names exactly one of `task_ids`. A commit naming none or several
/// is left out, so no line is counted twice.
fn split_by_commit_message(
    repo: &git2::Repository,
    request: &DirectLandingRequest,
    task_ids: &[String],
    classes: &DiffClasses<'_>,
) -> Result<BTreeMap<String, Counts>, OrbitError> {
    let git = |error: git2::Error| OrbitError::Execution(format!("walk landed range: {error}"));
    let mut walk = repo.revwalk().map_err(git)?;
    walk.simplify_first_parent().map_err(git)?;
    walk.push(git2::Oid::from_str(&request.after_commit).map_err(git)?)
        .map_err(git)?;
    walk.hide(git2::Oid::from_str(&request.before_commit).map_err(git)?)
        .map_err(git)?;
    let mut per_task: BTreeMap<String, Counts> = BTreeMap::new();
    for oid in walk {
        let oid = oid.map_err(git)?;
        let commit = repo.find_commit(oid).map_err(git)?;
        let message = commit.message().unwrap_or_default();
        let mut named = task_ids.iter().filter(|id| names_task(message, id));
        let (Some(task_id), None) = (named.next(), named.next()) else {
            tracing::debug!(commit = %oid, "landed commit names no single task; not attributed");
            continue;
        };
        if let Some((_, counts)) = diff_counts(repo, None, &oid.to_string(), classes) {
            per_task.entry(task_id.clone()).or_default().add(&counts);
        }
    }
    Ok(per_task)
}

/// Whether `message` names `id` as a whole token, so `ORB-1` does not match
/// `ORB-12`.
fn names_task(message: &str, id: &str) -> bool {
    message
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
        .any(|token| token == id)
}

/// The base and the class counts of `base..landed`, or of `landed` against
/// its first parent when `base` is `None`, matching `git diff --numstat`
/// with rename detection. Binary files count as changed files with no
/// lines. `None` when a commit is missing or `landed` has no parent.
fn diff_counts(
    repo: &git2::Repository,
    base: Option<&str>,
    landed: &str,
    classes: &DiffClasses<'_>,
) -> Option<(String, Counts)> {
    let landed = repo.find_commit(git2::Oid::from_str(landed).ok()?).ok()?;
    let base = match base {
        Some(base) => repo.find_commit(git2::Oid::from_str(base).ok()?).ok()?,
        None => landed.parent(0).ok()?,
    };
    let mut diff = repo
        .diff_tree_to_tree(Some(&base.tree().ok()?), Some(&landed.tree().ok()?), None)
        .ok()?;
    diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))
        .ok()?;
    let mut counts = Counts::default();
    for index in 0..diff.deltas().len() {
        let delta = diff.get_delta(index)?;
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())?
            .to_str()?
            .to_string();
        let (added, deleted) = match git2::Patch::from_diff(&diff, index).ok()? {
            Some(patch) => {
                let (_, added, deleted) = patch.line_stats().ok()?;
                (added as u64, deleted as u64)
            }
            None => (0, 0),
        };
        let class = if classes.tests.matches(&path) {
            &mut counts.tests
        } else if classes.non_code.matches(&path) {
            &mut counts.other
        } else {
            &mut counts.code
        };
        class.added += added;
        class.deleted += deleted;
        class.files += 1;
    }
    Some((base.id().to_string(), counts))
}
