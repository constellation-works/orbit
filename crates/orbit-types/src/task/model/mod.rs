//! Task types: status lifecycle, priority, complexity, and the [`Task`] struct itself.
//!
//! ## Task Status Lifecycle
//!
//! Ordinary status edits follow the lifecycle transition rules: `done` and
//! `archived` are terminal, while `rejected` may return only to `backlog` or
//! `in-progress`. A human operator can override the table with
//! `task update --force`; forced changes are recorded in task history.
//!
//! ### Statuses
//! | Status       | Purpose |
//! |--------------|---------|
//! | Proposed     | Awaiting human approval before entering the backlog. |
//! | Backlog      | Approved and queued for work. |
//! | Someday      | Future-scoped — wanted but not yet actionable. Agents skip someday tasks. |
//! | InProgress   | Actively being worked on. |
//! | Review       | Implementation complete; awaiting review/merge. |
//! | Done         | Accepted and closed. Terminal. |
//! | Blocked      | Temporarily paused. |
//! | Archived     | Soft-deleted. Terminal. |
//! | Rejected     | Declined. Can return to backlog or in-progress. |

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::identity::OrbitId;
use crate::task::artifacts::{
    TaskRelation, TaskRelationType, is_valid_orb_task_id, task_id_prefix,
};
use crate::task::{RegressionClassification, TaskDecision, TaskError};

mod status;
mod support;
mod task;

pub use status::*;

pub use support::*;

pub use task::*;
pub(crate) use task::{
    LazyTaskReferenceIndex, resolve_task_dependencies_lazily, resolve_task_relations_lazily,
};
