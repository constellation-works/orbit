//! Read-only status projection for `orbit routine list` / `show` [ORB-10021]:
//! every routine with both toggle layers (versioned `enabled` and host-local
//! pause), the computed next-due slot, and the last recorded fire — so "why
//! didn't this fire?" is one command. The row, fire and report projections
//! and the toggle selection here serve the CLI, dashboard and MCP alike.

// Bound status reads independently of the compact ten-fire dashboard strip.
mod checkout;
mod display;
mod fires;
mod report;
mod toggle;

use super::RoutineMachineIdentity;
use super::loader::{
    DiscoveredWorkspaces, LoadedRoutine, OwnerOnlyRoutine, RetiredRoutine, RoutineLoadError,
    RoutineWorkspaceProvider, apply_peer_name_collisions, collect_host_routines,
};
use crate::OrbitRuntime;
use crate::application::contracts::ScheduleDisplayState;
use crate::application::store_opener::StoreOpener;
pub(crate) use checkout::{
    checkout_edit_report, checkout_routine_statuses, toggle_checkout_routine,
};
use chrono::{DateTime, Local, Utc};
use display::RoutineFailureStreak;
pub use display::{FireField, RoutineField, RoutineStatus, project_fire};
pub use fires::recent_fires;
pub use orbit_automation::routines::breaker::{BreakerTrialPhase, breaker_trial_status};
use orbit_automation::routines::due::{next_occurrence, parse_cron};
use orbit_automation::routines::sweep::duplicate_ownership_skipped;
use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_store::contract::{
    JobRunQuery, RoutineBreakerRecord, RoutineBreakerState, RoutineFireRecord, RoutineFireState,
};
use orbit_types::workflow::{JobRun, JobRunState};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use report::routine_statuses_from_store;
pub use report::{RoutineStatusReport, routine_statuses_with_providers};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::path::Path;
pub use toggle::{
    RoutineBreakerAction, RoutineBreakerOutcome, control_listed_routine_breaker, pause_routine,
    resume_routine, routine_breaker_events, set_routine_enabled, toggle_listed_routine,
};
pub(crate) use toggle::{
    next_scheduled_occurrence, rewrite_enabled_line, select_listed_routine_for_target,
};
