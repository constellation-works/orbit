//! Resolve a run against one pinned set of host workspaces before reading or changing it.

use super::detail::{job_run_detail_to_json, recorded_run_detail_to_json};
use super::exec::Session;
use super::{Command, Operation};
use crate::application::job::{JobRunCancelResult, JobRunHolder, RecordedJobRun};
use crate::runtime::audit::run_events::{RunEventsPage, RunEventsPageResult};
use crate::runtime::audit::run_logs::RunCliInvocationProfile;
use crate::{Core, JobRun, NotFoundKind, OrbitError, OrbitRuntime, PipelineInvokeResult};
use orbit_common::governance::authorization::{DASHBOARD_AUTO_DRAIN_COMPLETE, DASHBOARD_JOB_RUN};
use orbit_types::workflow::JobRunTrigger;
use serde_json::Value;
use std::sync::Arc;

pub struct RunLookup<'a> {
    pub id: &'a str,
    pub requested: Option<&'a str>,
    /// Whether the starting Core is the requested/default workspace; otherwise it is a host-store reader.
    pub selected: bool,
    /// Open an active workspace from the same registry snapshot.
    pub open: &'a dyn Fn(&str) -> Option<Arc<Core>>,
}

impl RunLookup<'_> {
    fn locate(&self, reader: &Core, settle: bool) -> Result<Located, LookupError> {
        let reader = reader.runtime();
        let read = |rt: &OrbitRuntime| {
            if settle {
                rt.show_job_run(self.id)
            } else {
                rt.show_job_run_without_drain_leaf_settlement(self.id)
            }
        };
        if self.selected {
            match read(reader) {
                Ok(run) => return Ok(Located::Registered(reader.clone(), Box::new(run))),
                Err(OrbitError::NotFound {
                    kind: NotFoundKind::JobRun,
                    ..
                }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let holders = reader.job_run_recording_workspaces(self.id)?;
        let chosen = match holders.as_slice() {
            [] => {
                let holder = reader.job_run_holder(self.id).unwrap_or_else(|error| {
                    tracing::warn!(run_id=self.id,%error,"run holder unreadable; reporting the run as not found");
                    None
                });
                return Err(LookupError::Absent(holder.map(Box::new)));
            }
            [only] => only.as_str(),
            many => self
                .requested
                .filter(|id| many.iter().any(|held| held == id))
                .ok_or_else(|| LookupError::Ambiguous(holders.clone()))?,
        };
        if let Some(core) = (self.open)(chosen) {
            let rt = core.runtime();
            return Ok(Located::Registered(rt.clone(), Box::new(read(rt)?)));
        }
        reader
            .recorded_job_run(chosen, self.id)?
            .map(|record| Located::Recorded(reader.clone(), Box::new(record)))
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, self.id.to_owned()).into())
    }
}

pub enum LookupError {
    Absent(Option<Box<JobRunHolder>>),
    Ambiguous(Vec<String>),
    Unregistered(String),
    Flow(OrbitError),
}

impl From<OrbitError> for LookupError {
    fn from(value: OrbitError) -> Self {
        Self::Flow(value)
    }
}

enum Located {
    Registered(OrbitRuntime, Box<JobRun>),
    Recorded(OrbitRuntime, Box<RecordedJobRun>),
}

impl Located {
    fn writable(self) -> Result<OrbitRuntime, LookupError> {
        match self {
            Self::Registered(rt, _) => Ok(rt),
            Self::Recorded(_, recorded) => Err(LookupError::Unregistered(recorded.workspace_id)),
        }
    }
}

impl<'a> Command<'a, Result<Value, LookupError>> {
    /// Read registered or recorded run detail.
    pub fn dashboard_detail(lookup: RunLookup<'a>) -> Self {
        Self::new(Operation::command("run", "show"), move |core| {
            Ok((|| {
                let located = lookup.locate(core, false)?;
                Ok(match located {
                    Located::Registered(rt, run) => job_run_detail_to_json(&rt, &run),
                    Located::Recorded(rt, record) => recorded_run_detail_to_json(&rt, &record),
                })
            })())
        })
    }
}

impl<'a> Command<'a, Result<RunEventsPageResult, LookupError>> {
    /// Read the event page under the workspace that holds the run.
    pub fn dashboard_events(lookup: RunLookup<'a>, page: RunEventsPage<'a>) -> Self {
        Self::new(Operation::command("run", "events"), move |core| {
            Ok((|| {
                let located = lookup.locate(core, true)?;
                Ok(match located {
                    Located::Registered(rt, run) => rt.run_events_page(&run, page)?,
                    Located::Recorded(rt, record) => rt.run_events_page_in_workspace(
                        &record.workspace_id,
                        &record.run.run_id,
                        page,
                    )?,
                })
            })())
        })
    }
}

impl<'a> Command<'a, Result<Vec<Value>, LookupError>> {
    /// Read bounded log previews for the located run.
    pub fn dashboard_logs(lookup: RunLookup<'a>, limit: usize) -> Self {
        Self::new(Operation::command("run", "logs"), move |core| {
            Ok((|| {
                let located = lookup.locate(core, true)?;
                Ok(match located {
                    Located::Registered(rt, run) => rt
                        .collect_run_cli_invocations_bounded(
                            &run.run_id,
                            Some(limit),
                            RunCliInvocationProfile::HttpPreview.blob_read_max_bytes(),
                        )?
                        .iter()
                        .map(|row| row.to_profile_json(RunCliInvocationProfile::HttpPreview))
                        .collect(),
                    Located::Recorded(..) => Vec::new(),
                })
            })())
        })
    }
}

impl<'a> Command<'a, Result<Result<JobRunCancelResult, OrbitError>, LookupError>> {
    /// Cancel the run in the registered workspace that holds it.
    pub fn dashboard_cancel(
        lookup: RunLookup<'a>,
        queued_only: bool,
        reason: Option<&'a str>,
        force: bool,
        block: bool,
    ) -> Self {
        Self::new(Operation::command("run", "cancel"), move |core| {
            Ok((|| {
                let located = lookup.locate(core, false)?;
                let rt = located.writable()?;
                Ok(if queued_only {
                    rt.cancel_queued_job_run(lookup.id, "dashboard", "web", reason, block)
                } else {
                    rt.cancel_job_run_with_options_and_policy(
                        lookup.id,
                        "dashboard",
                        "web",
                        reason,
                        force,
                        block,
                    )
                })
            })())
        })
    }
}

impl<'a> Command<'a, Result<PipelineInvokeResult, LookupError>> {
    /// Submit a replay in the holding workspace. A source run that asked for
    /// completion needs completion authority, which the flow asks for once it
    /// has read the run.
    pub fn dashboard_replay(lookup: RunLookup<'a>, claim: Option<&'a str>) -> Self {
        Self::new(replay_operation(), move |core| {
            Ok((|| {
                let located = lookup.locate(core, false)?;
                let rt = located.writable()?;
                let source = rt.show_job_run(lookup.id)?;
                if source
                    .input
                    .as_ref()
                    .and_then(|input| input.get("completion"))
                    .and_then(Value::as_str)
                    == Some("done")
                {
                    super::exec::require(core, &DASHBOARD_AUTO_DRAIN_COMPLETE)?;
                }
                Ok(rt.submit_replay_run(
                    lookup.id,
                    Some("dashboard"),
                    claim,
                    JobRunTrigger::dashboard(),
                )?)
            })())
        })
    }
}

/// A dashboard replay's operation: it requires job-run authority whatever the
/// source run is.
fn replay_operation() -> Operation {
    Operation::command("run", "replay").requires(&DASHBOARD_JOB_RUN)
}

/// Decide a dashboard replay for `session` before the workspace that holds
/// the run is discovered. `exec` decides the same requirement again.
pub fn admit_dashboard_replay(session: Session) -> Result<(), OrbitError> {
    super::exec::admit_operation(session, &replay_operation())
}
