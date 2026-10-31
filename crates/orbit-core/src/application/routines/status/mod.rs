//! Read-only status projection for `orbit routine list` / `show` [ORB-10021]:
//! every routine with both toggle layers (versioned `enabled` and host-local
//! pause), the computed next-due slot, and the last recorded fire — so "why
//! didn't this fire?" is one command. The row, fire and report projections
//! and the toggle selection here serve the CLI, dashboard and MCP alike.

use super::RoutineMachineIdentity;
use super::due::{next_occurrence, parse_cron};
use super::loader::{
    DiscoveredWorkspaces, LoadedRoutine, OwnerOnlyRoutine, RetiredRoutine, RoutineLoadError,
    RoutineWorkspaceProvider, collect_host_routines,
};
use crate::OrbitRuntime;
use chrono::{DateTime, Local, Utc};
use orbit_automation::routines::sweep::duplicate_ownership_skipped;
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_store::contracts::{JobRunQuery, RoutineFireRecord, RoutineFireState};
use orbit_types::workflow::{JobRun, JobRunState, RoutineTarget};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::path::Path;

// Bound status reads independently of the compact ten-fire dashboard strip.

mod checkout;
mod display;
mod fires;
mod report;
mod toggle;

pub(crate) use checkout::{
    checkout_edit_report, checkout_refusal, checkout_routine_statuses, toggle_checkout_routine,
};
pub use display::{
    FireField, RoutineFailureStreak, RoutineField, RoutineStatus, ScheduleDisplayState,
    project_fire,
};
pub use fires::recent_fires;
pub use report::{RoutineStatusReport, routine_statuses_with_providers};
pub use toggle::{
    RoutineToggleOutcome, RoutineToggleRefusal, pause_routine, resume_routine,
    select_listed_routine, set_routine_enabled, toggle_listed_routine,
};
pub(crate) use toggle::{next_scheduled_occurrence, rewrite_enabled_line};
