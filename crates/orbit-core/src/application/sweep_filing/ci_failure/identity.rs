//! Creation identities read once per filing, with one pre-decision reader.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use orbit_common::OrbitError;
use orbit_types::task::{CiFailureIdentity, CiFailureSource, Task, TaskComment, TaskStatus};

use super::cluster::{COMPILER_CAUSE_LINE, FailureCluster, OPEN_COMPILER_SET_LINE, SIGNATURE_LINE};
use super::filing::CI_FAILURE_KEY_TAG_PREFIX;
use super::log_signature::{compiler_cause, compiler_diagnostic_set, error_signature};
use crate::OrbitRuntime;
use crate::application::contracts::CI_FAILURE_TAG;
use crate::application::sweep_filing::digest;
use crate::application::sweep_filing::duplicate_tasks::DuplicateTaskLookup;

pub(super) struct TaskCiIdentity {
    pub(super) identity: CiFailureIdentity,
    // A shipped broad key is not a cause. Old fallback records can still
    // retain a concrete diagnostic in their bounded log section.
    pub(super) archive_signature: Option<String>,
    // Old-key migration checked the retained log, whereas completed repairs
    // checked the rendered cause line. Keep those distinct for old records
    // whose description excerpt was truncated.
    pub(super) prior_compiler_cause_digest: Option<String>,
    // Old provenance matching tested membership across the whole description,
    // without requiring all three labels to occur in the same run block.
    legacy_source_ids: Option<[BTreeSet<String>; 3]>,
}

impl TaskCiIdentity {
    pub(super) fn matches_source(&self, source: &CiFailureSource) -> bool {
        if source.run_id.is_empty() || source.job_id.is_empty() || source.checkout.is_empty() {
            return false;
        }
        match &self.legacy_source_ids {
            Some([runs, jobs, checkouts]) => {
                runs.contains(&source.run_id)
                    && jobs.contains(&source.job_id)
                    && checkouts.contains(&source.checkout)
            }
            None => self.identity.sources.contains(source),
        }
    }
}

pub(super) struct CiTaskIdentities<'a> {
    runtime: &'a OrbitRuntime,
    tasks: RefCell<BTreeMap<String, Option<Rc<TaskCiIdentity>>>>,
}

impl<'a> CiTaskIdentities<'a> {
    pub(super) fn new(runtime: &'a OrbitRuntime) -> Self {
        Self {
            runtime,
            tasks: RefCell::new(BTreeMap::new()),
        }
    }

    pub(super) fn for_task(&self, task: &Task) -> Result<Option<Rc<TaskCiIdentity>>, OrbitError> {
        if let Some(identity) = self.tasks.borrow().get(&task.id) {
            return Ok(identity.clone());
        }
        let history = self.runtime.get_task_history(&task.id)?;
        let identity = match CiFailureIdentity::from_history(&history) {
            Ok(Some(identity)) => Some(TaskCiIdentity {
                archive_signature: identity
                    .signature
                    .clone()
                    .or_else(|| retained_archive_signature(task)),
                prior_compiler_cause_digest: identity.compiler_cause_digest.clone(),
                identity,
                legacy_source_ids: None,
            }),
            Ok(None) => Some(ci_identity_from_description_legacy(task)),
            // A foreign decision is authoritative; never reinterpret its prose.
            Err(_) => None,
        }
        .map(Rc::new);
        self.tasks
            .borrow_mut()
            .insert(task.id.clone(), identity.clone());
        Ok(identity)
    }
}

/// The only compatibility reader for pre-decision CI tasks. Keep the old
/// distinction between a rendered complete cause and its retained log subset,
/// and the old whole-description provenance membership semantics.
fn ci_identity_from_description_legacy(task: &Task) -> TaskCiIdentity {
    let description = &task.description;
    let identity_line = |prefix| {
        description.lines().find_map(|line| {
            line.trim_end()
                .strip_prefix(prefix)?
                .strip_suffix('`')
                .map(str::to_string)
        })
    };
    let retained_cause = compiler_cause(description);
    let prior_compiler_cause_digest = retained_cause.as_ref().map(|cause| digest(&[cause]));
    let compiler_cause_digest = identity_line(COMPILER_CAUSE_LINE);
    let open_compiler_set = identity_line(OPEN_COMPILER_SET_LINE).or_else(|| {
        // An old excerpt can establish the coordinate-free set only if it
        // contains the entire cause named by the complete-cause line.
        if compiler_cause_digest.as_ref() != prior_compiler_cause_digest.as_ref()
            || retained_cause.is_none()
        {
            return None;
        }
        compiler_diagnostic_set(description).map(|set| digest(&[&set]))
    });
    let labels = |prefix: &str, suffix: &str| {
        description
            .match_indices(prefix)
            .filter_map(|(offset, _)| {
                description[offset + prefix.len()..]
                    .split_once(suffix)
                    .map(|(value, _)| value.to_string())
            })
            .collect()
    };
    TaskCiIdentity {
        archive_signature: identity_line(SIGNATURE_LINE)
            .or_else(|| retained_archive_signature(task)),
        identity: CiFailureIdentity {
            failure_key: task
                .tags
                .iter()
                .find_map(|tag| tag.strip_prefix(CI_FAILURE_KEY_TAG_PREFIX))
                .unwrap_or_default()
                .to_string(),
            compiler_cause_digest,
            open_compiler_set,
            signature: identity_line(SIGNATURE_LINE),
            tested_commit: identity_line("- Commit the runner actually checked out: `")
                .unwrap_or_default(),
            sources: Vec::new(),
        },
        prior_compiler_cause_digest,
        legacy_source_ids: Some([
            labels("run `", "`"),
            labels("(id `", "`)"),
            labels("commit actually checked out: `", "`"),
        ]),
    }
}

fn retained_archive_signature(task: &Task) -> Option<String> {
    let section = task
        .description
        .split_once("\n## Failed-step log excerpt\n")?
        .1;
    let section = section
        .split_once("\n## ")
        .map_or(section, |(excerpt, _)| excerpt);
    // An incomplete excerpt cannot establish a different cause. Do not scan
    // other prose, recommendations or comments for diagnostic-looking text.
    if section.contains("truncated") || section.contains("omitted") || section.contains("[...") {
        return None;
    }
    let log = section
        .trim_start()
        .strip_prefix("```\n")?
        .split_once("\n```")?
        .0;
    let signature = error_signature(log, "");
    (!signature.step_fallback).then_some(signature.text)
}

/// Exclude a rejected broad-key collision before shared duplicate lookup can
/// follow its superseding owner. Open exact-key idempotency stays unchanged.
pub(super) struct CauseCheckedLookup<'a, L: ?Sized> {
    pub(super) inner: &'a L,
    pub(super) cluster: &'a FailureCluster,
    pub(super) identities: &'a CiTaskIdentities<'a>,
}

impl<L: DuplicateTaskLookup + ?Sized> DuplicateTaskLookup for CauseCheckedLookup<'_, L> {
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        let mut tasks = Vec::new();
        for task in self.inner.list_tasks_by_tags(tags)? {
            if self.cluster.compiler_cause.is_none()
                && matches!(task.status, TaskStatus::Archived | TaskStatus::Rejected)
                && task.tags.iter().any(|tag| tag == CI_FAILURE_TAG)
                && (self.cluster.signature_is_step_fallback
                    || !self.identities.for_task(&task)?.is_some_and(|record| {
                        record.archive_signature.as_ref() == Some(&self.cluster.signature)
                    }))
            {
                continue;
            }
            tasks.push(task);
        }
        Ok(tasks)
    }

    fn list_tasks(&self) -> Result<Rc<[Task]>, OrbitError> {
        self.inner.list_tasks()
    }
    fn get_task(&self, id: &str) -> Result<Task, OrbitError> {
        self.inner.get_task(id)
    }
    fn get_task_comments(&self, id: &str) -> Result<Rc<[TaskComment]>, OrbitError> {
        self.inner.get_task_comments(id)
    }
}
