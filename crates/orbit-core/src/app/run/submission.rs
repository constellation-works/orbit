use super::{Command, Operation};
use crate::application::job::{
    AutoDrainSubmissionProfile, AutoDrainSubmissionReceipt, AutoDrainSubmissionSource,
    DrainAdmissionsStopResult, DrainWindowExtensionChange, DrainWorkerLimitChange, JobCatalogEntry,
    JobCatalogFilter, JobRunCancelResult, ShipSubmissionProfile, ShipSubmissionReceipt,
};
use crate::{
    AutoDrainRequest, DrainAdmissionsStopRequest, DrainWindowExtensionRequest,
    DrainWorkerLimitRequest, JobRun, OrbitError, ShipMode, ShipModeResolutionProfile,
    ShipRunRequest, WorkspacePullRequest,
};
use orbit_types::workflow::JobRunTrigger;

impl<'a> Command<'a, Vec<(JobCatalogEntry, Option<JobRun>)>> {
    /// List catalog entries and the most recent run of each.
    pub fn jobs(all: bool, filter: JobCatalogFilter) -> Self {
        Self::new(
            Operation::command("job", "list").target(Some("job"), None),
            move |core| core.runtime().list_job_catalog_with_last_run(all, filter),
        )
    }
}

impl<'a> Command<'a, JobCatalogEntry> {
    /// Load a catalog entry.
    pub fn job(id: &'a str) -> Self {
        Self::new(
            Operation::command("job", "show").target(Some("job"), Some(id.to_owned())),
            move |core| core.runtime().show_job_catalog_entry(id),
        )
    }
}

impl<'a> Command<'a, JobRunCancelResult> {
    /// Cancel a run from the command line.
    pub fn cancel(id: &'a str, reason: Option<&'a str>, force: bool, block: bool) -> Self {
        Self {
            operation: Operation::command("run", "cancel")
                .target(Some("job_run"), Some(id.to_owned())),
            flow: Box::new(move |core| {
                let result = core.runtime().cancel_job_run_with_options_and_policy(
                    id,
                    "cli",
                    "run_cancel",
                    reason,
                    force,
                    block,
                )?;
                let exit_code = i32::from(
                    !result.unstopped_leaves.is_empty() || !result.unstopped_children.is_empty(),
                );
                Ok(super::Outcome {
                    value: result,
                    exit_code,
                })
            }),
        }
    }
}

impl<'a> Command<'a, DrainWorkerLimitChange> {
    /// Change a drain worker ceiling.
    pub fn concurrency(request: DrainWorkerLimitRequest<'a>) -> Self {
        Self::new(
            Operation::command("run", "concurrency")
                .target(Some("job_run"), Some(request.run_id.to_owned())),
            move |core| core.runtime().set_drain_worker_limit(request),
        )
    }
}

impl<'a> Command<'a, DrainWindowExtensionChange> {
    /// Extend a drain window.
    pub fn extend(request: DrainWindowExtensionRequest<'a>) -> Self {
        Self::new(
            Operation::command("run", "extend")
                .target(Some("job_run"), Some(request.run_id.to_owned())),
            move |core| core.runtime().extend_drain_window(request),
        )
    }
}

impl<'a> Command<'a, DrainAdmissionsStopResult> {
    /// Stop admitting work.
    pub fn stop(request: DrainAdmissionsStopRequest<'a>) -> Self {
        Self::new(
            Operation::command("run", "auto").target(Some("workflow"), Some("auto".into())),
            move |core| core.runtime().stop_workspace_auto_admissions(request),
        )
    }
}

impl<'a> Command<'a, AutoDrainSubmissionReceipt> {
    /// Submit a local drain and read its receipt.
    pub fn auto(request: AutoDrainRequest<'a>, profile: AutoDrainSubmissionProfile) -> Self {
        Self::new(
            Operation::command("run", "auto").target(Some("workflow"), Some("auto".into())),
            move |core| submit_auto(core.runtime(), request, profile),
        )
    }
}

impl<'a> Command<'a, AutoDrainSubmissionReceipt> {
    /// Submit a pull drain and read its receipt.
    pub fn pull(
        request: WorkspacePullRequest<'a>,
        trigger: JobRunTrigger,
        profile: AutoDrainSubmissionProfile,
    ) -> Self {
        Self::new(
            Operation::command("run", "auto").target(Some("workflow"), Some("auto".into())),
            move |core| {
                let rt = core.runtime();
                let selector = request.selector;
                let invoke = rt.submit_workspace_pull_run(request, trigger)?;
                rt.auto_drain_submission_receipt(
                    &invoke,
                    AutoDrainSubmissionSource::Pull { selector },
                    profile,
                )
            },
        )
    }
}

impl<'a> Command<'a, ShipSubmissionReceipt> {
    /// Resolve the ship mode, submit selected work, and collect its receipt.
    pub fn ship(
        explicit: Option<ShipMode>,
        request: ShipRunRequest<'a>,
        profile: ShipSubmissionProfile,
    ) -> Self {
        Self::new(
            Operation::command("run", "ship").target(Some("workflow"), Some("ship".into())),
            move |core| {
                let rt = core.runtime();
                let mode_profile = if profile == ShipSubmissionProfile::Cli {
                    ShipModeResolutionProfile::NativeCliLegacyRoot
                } else {
                    ShipModeResolutionProfile::BoundRuntimeOnly
                };
                let mode = rt.resolve_ship_mode(explicit, mode_profile)?;
                if profile == ShipSubmissionProfile::Cli {
                    crate::find_workflow("ship").ok_or_else(|| {
                        OrbitError::InvalidInput("unknown workflow 'ship'".into())
                    })?;
                }
                submit_ship(rt, ShipRunRequest { mode, ..request }, profile)
            },
        )
    }
}

pub(super) fn submit_auto(
    rt: &crate::OrbitRuntime,
    request: AutoDrainRequest<'_>,
    profile: AutoDrainSubmissionProfile,
) -> Result<AutoDrainSubmissionReceipt, OrbitError> {
    let invoke = rt.submit_workspace_auto_run(request)?;
    rt.auto_drain_submission_receipt(&invoke, AutoDrainSubmissionSource::Local, profile)
}

pub(super) fn submit_ship(
    runtime: &crate::OrbitRuntime,
    request: ShipRunRequest<'_>,
    profile: ShipSubmissionProfile,
) -> Result<ShipSubmissionReceipt, OrbitError> {
    let named = !request.task_ids.is_empty();
    let invoke = runtime.submit_ship_run(request)?;
    runtime.ship_submission_receipt(&invoke, named, profile)
}
