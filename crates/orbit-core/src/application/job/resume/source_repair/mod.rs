//! Authenticate and admit owner-local published-candidate recovery.

mod admission;
mod adoption;
mod completion;
mod handoff;
mod identity;
mod observation;
mod publication;
mod published_owner;
mod resumed_recovery;
mod successor;

use std::collections::BTreeSet;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::git::run_git;
use orbit_common::security::release::sha256_hex;
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    ActivityV2Spec, CommitObservationStatus, JobRun, JobRunState, JobV2, JobV2StepBody,
    PipelineState, SOURCE_REPAIR_KEY, SOURCE_REPAIR_LINKED, SOURCE_REPAIR_RESERVED,
    SourceRepairAdmission, SourceRepairBinding,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};
use crate::runtime::run_input::carried_task_ids;
use admission::{RepairLink, digest, field, refuse, task_digest, task_pr_association};
pub(crate) use published_owner::PublishedResume;
