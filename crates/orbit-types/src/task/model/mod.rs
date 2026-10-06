//! Task types: status lifecycle, priority, complexity, and the [`Task`] struct itself.
//!
//! ## Task Status Lifecycle
//!
//! An explicit, authorized edit may move a task directly between any two
//! statuses. Status is an audited classification; workflow admission,
//! completion, and delivery keep their own stricter operational gates.
//!
//! ### Statuses
//! | Status       | Purpose |
//! |--------------|---------|
//! | Proposed     | Awaiting human approval before entering the backlog. |
//! | Backlog      | Approved and queued for work. |
//! | Someday      | Future-scoped — wanted but not yet actionable. Agents skip someday tasks. |
//! | InProgress   | Actively being worked on. |
//! | Review       | Implementation complete; awaiting review/merge. |
//! | Done         | Accepted and closed. May be reopened by an explicit edit. |
//! | Blocked      | Temporarily paused. |
//! | Archived     | Soft-deleted. Restorable to any other status. |
//! | Rejected     | Declined. Can be re-opened. |

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::identity::OrbitId;
use crate::task::TaskError;
use crate::task::artifacts::{
    TaskRelation, TaskRelationType, is_valid_orb_task_id, task_id_prefix,
};

mod status;
mod support;
mod task;

pub use status::{
    CANDIDATE_DISCARDED_EVENT, CANDIDATE_RESUME_EVENT, DEFAULT_TASK_LIST_LIMIT,
    NO_AUTO_APPROVE_TAG, NO_DIFF_EXPECTED_TAG, TASK_REFERENCE_NOT_VERIFIABLE_HERE, TaskComplexity,
    TaskCreateStatus, TaskPriority, TaskStatus, TaskType, UNSET_BUCKET, complexity_bucket,
    complexity_bucket_ord, labeled_or_unset,
};

pub use support::{
    ArtifactPresentation, DependencyDeadEnd, ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM,
    MAX_TASK_ARTIFACT_CONTENT_BYTES, ResolvedTaskDependency, ResolvedTaskRelation, TaskArtifact,
    TaskComment, TaskHistoryEntry, UnsatisfiableTaskDependency, artifact_presentation,
    image_bytes_match_media_type, inline_safe_artifact_media_type, is_inline_image_media_type,
    is_textual_artifact_media_type, media_type_for_artifact_path, normalized_artifact_media_type,
    push_external_ref_if_missing,
};

pub use task::{
    CI_FAILURE_KEY_TAG_PREFIX, DELIVERY_JOB_TAG_PREFIX, ExecutionLocation, Task,
    TaskReferenceIndex, archived_task_completed_before_archive, automatic_dispatch_cmp,
    delivery_job_selection, deserialize_required_tools, is_system_identity_tag,
    normalize_required_tools, normalize_task_dependencies, normalize_task_tags,
    resolve_task_dependencies, resolve_task_dependencies_with_index, resolve_task_relations,
    resolve_task_relations_with_index, satisfy_completed_archived_dependencies,
    task_dependencies_ready, task_dependencies_ready_with_index, task_matches_tags,
    unmet_task_dependencies, unmet_task_dependencies_with_index, unsatisfiable_task_dependencies,
    unsatisfiable_task_dependencies_with_index, validate_task_dependencies,
    validate_task_dependencies_with,
};
