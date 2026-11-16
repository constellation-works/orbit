//! Task-pilot receipts and the evidence the Agents › Pilots scorecard grades:
//! each done task a pilot assessed, with its landed diff, the pilot's and the
//! filer's selectors and the paths its run was contested on, and the lock
//! waits other tasks spent on paths each such task held.
//!
//! A task's pilot selectors are the newest task-pilot receipt's
//! `context_files_after`; the filer's are the oldest receipt's
//! `context_files_before`, what the task declared before any pilot ran. The
//! receipt names its pilot job run (`pilot_run_id`), whose record holds the
//! frozen activity crew draw; invocation evidence holds the pilot model. A
//! receipt written before that field is matched to the one task-pilot run
//! active when it was written, or among several, the one whose apply report
//! carries its exact operation id. Models come from the producing worker step
//! when its apply report is available. Without a report, only a single
//! recorded worker can identify the round (including a repair-only run).
//! Task links distinguish fan-out invocations; ambiguous evidence stays unknown.

mod evidence;
mod inputs;
mod runs;
mod wire;

pub(in crate::application) use evidence::{
    Assessment, Contest, HeldWait, PilotEvidence, pilot_evidence,
};
pub(super) use inputs::{PilotInputs, inputs};
pub(in crate::application) use runs::{PilotRun, PilotRuns};

/// The actor that writes task-pilot receipts.
const PILOT_ACTOR: &str = "task-pilot";
