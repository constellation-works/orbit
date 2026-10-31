//! `orbit automation explain job:<name>` [ORB-20157]: a job's auto-task
//! steps as checked at load, with each named auto-task resolved and frozen
//! for inspection (`docs/design/activity-job/specs/auto-task-step.md` §7,
//! phase 1). No such job runs yet, so there is no wait to evaluate; the
//! report reads definitions and writes nothing.

use chrono::Utc;
use orbit_automation::condition::{
    CheckedAutoTaskStep, CheckedExpression, ConditionReads, OuterBound, check_job_waits,
};
use orbit_common::OrbitError;
use orbit_types::workflow::AutoTaskSchedule;
use serde::Serialize;

use super::ExplainedDefinition;
use crate::OrbitRuntime;

/// The `orbit.automation.explain` payload for `job:<name>`.
#[derive(Debug, Serialize)]
pub struct JobStepsExplain {
    pub definition: ExplainedDefinition,
    pub state: String,
    /// The file's `schemaVersion`: always 3 for a job with auto-task steps.
    pub schema_version: u32,
    pub expire_when: ExplainedJobExpiry,
    pub pinned_revision: ExplainedPinnedRevision,
    pub steps: Vec<ExplainedAutoTaskStep>,
    pub evaluated_at: String,
    pub runtime: ExplainedJobRuntime,
}

#[derive(Debug, Serialize)]
pub struct ExplainedJobExpiry {
    pub source: String,
    /// `after_start` or `at`.
    pub bound: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_start_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainedPinnedRevision {
    /// Whether `job.source_revision` is pinned when a run is admitted.
    pub pinned: bool,
    pub note: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ExplainedAutoTaskStep {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel: Option<String>,
    /// The step `self.prev` observes; absent for a first step or a branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
    pub target: ExplainedAutoTaskTarget,
    /// The existing one-shot guard: false skips the step, and a skipped step
    /// never counts as accepted evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Absent: ready as soon as the job reaches the step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_when: Option<String>,
    /// Absent: historical evidence never discharges the step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reuse_when: Option<String>,
    /// Absent: the definition's declared completion contract applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_result: Option<String>,
    pub reads: ExplainedWaitReads,
    /// Always `not_evaluated`: no run of this job exists to wait.
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ExplainedWaitReads {
    pub steps: Vec<String>,
    pub board: Vec<&'static str>,
    pub tasks: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainedAutoTaskTarget {
    pub auto_task: String,
    /// `resolved`, `missing` (admission would hold) or `unreadable`.
    pub resolution: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// SHA-256 of the definition file as read now: the revision a run would
    /// freeze.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admission_guard: Option<ExplainedAdmissionGuard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The referenced definition's own condition, an additional admission
/// guard; its cron or interval slots do not restrict a job request.
#[derive(Debug, Serialize)]
pub struct ExplainedAdmissionGuard {
    /// `condition`, or `none` for a schedule that only sets slots.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainedJobRuntime {
    pub executable: bool,
    pub note: &'static str,
}

impl OrbitRuntime {
    /// Explain job `name`'s auto-task steps. Read-only: it resolves each
    /// named auto-task but mints, admits and writes nothing.
    pub(super) fn explain_job_auto_task_steps(
        &self,
        name: &str,
    ) -> Result<JobStepsExplain, OrbitError> {
        let entry = self.show_job_catalog_entry(name)?;
        let now = Utc::now();
        let checked = check_job_waits(&entry.spec, now).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "job '{name}' does not load ({}): {error}",
                entry.path.display()
            ))
        })?;
        let expiry = checked.expiry.ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "job '{name}' has no auto-task steps; explain covers jobs with \
                 `target: auto-task:<name>` steps"
            ))
        })?;
        let steps = checked
            .steps
            .iter()
            .map(|step| self.explained_auto_task_step(step))
            .collect();
        let (bound, after_start_seconds, at) = match expiry.bound {
            OuterBound::AfterStart(duration) => ("after_start", Some(duration.num_seconds()), None),
            OuterBound::At(at) => ("at", None, Some(at.to_rfc3339())),
        };
        Ok(JobStepsExplain {
            definition: ExplainedDefinition {
                kind: "job",
                name: entry.job_id.clone(),
                path: entry.path.display().to_string(),
                enabled: false,
            },
            state: entry.state().to_string(),
            schema_version: entry.spec.schema_version,
            expire_when: ExplainedJobExpiry {
                source: expiry.expression.source,
                bound,
                after_start_seconds,
                at,
            },
            pinned_revision: ExplainedPinnedRevision {
                pinned: checked.pinned_revision,
                note: if checked.pinned_revision {
                    "a release-readiness/v1 result requirement pins the configured integration \
                     branch's revision at admission; job.source_revision is always present"
                } else {
                    "no step requires a release-readiness/v1 result, so admission pins no \
                     revision; test has(job.source_revision) before reading it"
                },
            },
            steps,
            evaluated_at: now.to_rfc3339(),
            runtime: ExplainedJobRuntime {
                executable: false,
                note: "auto-task steps are defined and explained only: this Orbit refuses to \
                       enable or run the job, so nothing waits, no task is minted and no \
                       result is accepted",
            },
        })
    }

    fn explained_auto_task_step(&self, step: &CheckedAutoTaskStep) -> ExplainedAutoTaskStep {
        let source = |expression: &Option<CheckedExpression>| {
            expression.as_ref().map(|checked| checked.source.clone())
        };
        ExplainedAutoTaskStep {
            id: step.id.clone(),
            parallel: step.parallel.clone(),
            prev: step.prev.clone(),
            target: self.explained_auto_task_target(&step.auto_task),
            when: step.when.clone(),
            wait_until: source(&step.wait_until),
            expire_when: source(&step.expire_when),
            reuse_when: source(&step.reuse_when),
            require_result: step.require_result.clone(),
            reads: explained_reads(&step.reads),
            status: "not_evaluated",
        }
    }

    /// Resolve and freeze one named auto-task for inspection. A missing or
    /// unreadable definition is reported, not an error: at run time it would
    /// hold admission.
    fn explained_auto_task_target(&self, name: &str) -> ExplainedAutoTaskTarget {
        let mut target = ExplainedAutoTaskTarget {
            auto_task: name.to_owned(),
            resolution: "missing",
            path: None,
            enabled: None,
            definition_digest: None,
            admission_guard: None,
            error: None,
        };
        let unreadable = |mut target: ExplainedAutoTaskTarget, error: String| {
            target.resolution = "unreadable";
            target.error = Some(error);
            target
        };
        let path = match self.auto_task_read_source(name) {
            Ok(Some((_, path))) => path,
            Ok(None) => return target,
            Err(error) => return unreadable(target, error.to_string()),
        };
        target.path = Some(path.display().to_string());
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => return unreadable(target, format!("read {}: {error}", path.display())),
        };
        let definition = match self.auto_task_show(name) {
            Ok(Some(definition)) => definition,
            Ok(None) => return target,
            Err(error) => return unreadable(target, error.to_string()),
        };
        target.resolution = "resolved";
        target.enabled = Some(definition.enabled);
        target.definition_digest = Some(orbit_automation::delivery::digest(&bytes));
        target.admission_guard = Some(match definition.schedule {
            AutoTaskSchedule::Condition { condition } => ExplainedAdmissionGuard {
                kind: "condition",
                scope: Some(condition.scope.as_str()),
                when: Some(condition.when),
            },
            _ => ExplainedAdmissionGuard {
                kind: "none",
                scope: None,
                when: None,
            },
        });
        target
    }
}

fn explained_reads(reads: &ConditionReads) -> ExplainedWaitReads {
    ExplainedWaitReads {
        steps: reads.steps.iter().cloned().collect(),
        board: reads.board.iter().copied().collect(),
        tasks: reads.task_ids.iter().cloned().collect(),
    }
}
