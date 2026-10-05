//! Durable, per-selector authorization for context targets a task will create.
//!
//! An operator surface that accepts a missing `context_files` selector under
//! `allow_missing_context` records the exact canonical selectors it accepted
//! as one [`CONTEXT_CREATION_AUTHORIZED_EVENT`] history entry, committed in
//! the same task-bundle write as the scope change. The latest entry is the
//! whole grant: a later entry replaces it, so removing a selector from the
//! task's scope revokes its grant and nothing older can revive it.
//!
//! Each grant is bound to the scope and envelope revision it was recorded with
//! ([`ContextCreationGrant::context_files_sha256`] and
//! [`ContextCreationGrant::updated_at`]). A writer that changes a task without
//! carrying the grant forward — an older client, or a path that does not
//! maintain it — leaves the grant [`ContextCreationState::Void`], even if the
//! writer later restores the same scope.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::TaskError;

/// History event whose note is a [`ContextCreationGrant`].
pub const CONTEXT_CREATION_AUTHORIZED_EVENT: &str = "context_creation_authorized";

/// The only grant record version this build reads or writes.
pub const CONTEXT_CREATION_GRANT_VERSION: u32 = 1;

/// Upper bound on selectors one task may hold authorized for creation.
pub const MAX_CONTEXT_CREATION_SELECTORS: usize = 32;

/// The note of one [`CONTEXT_CREATION_AUTHORIZED_EVENT`] history entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCreationGrant {
    /// [`CONTEXT_CREATION_GRANT_VERSION`].
    pub version: u32,
    /// The task the grant belongs to; a record copied to another task is void.
    pub task_id: String,
    /// Exact canonical selectors authorized for creation, sorted and unique.
    /// Empty records an explicit revocation of every earlier grant.
    pub selectors: Vec<String>,
    /// [`context_files_sha256`] of the scope this grant was recorded with.
    pub context_files_sha256: String,
    /// Unique id of the history event that recorded this grant. Older notes
    /// omit it; maintained writers assign it before persisting a new grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    /// Task envelope revision this grant describes. A writer that changes the
    /// envelope without maintaining grants makes the old grant stale even if
    /// it later restores the same selector list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

impl ContextCreationGrant {
    /// A grant for `selectors`, bound to `context_files`.
    pub fn new(
        task_id: &str,
        selectors: impl IntoIterator<Item = String>,
        context_files: &[String],
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            version: CONTEXT_CREATION_GRANT_VERSION,
            task_id: task_id.to_string(),
            selectors: selectors
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            context_files_sha256: context_files_sha256(context_files),
            generation: None,
            updated_at: Some(updated_at),
        }
    }

    /// Parse a history note; `None` for a note of another shape.
    pub fn from_note(note: &str) -> Option<Self> {
        serde_json::from_str(note).ok()
    }

    /// The history note recording this grant.
    pub fn to_note(&self) -> String {
        // A struct of strings and a number always serializes.
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Stable identity of this exact record, for compare-and-set checks.
    pub fn identity(&self) -> String {
        // `updated_at` is a compatibility seal for older writers. Refreshing
        // that seal must not make an unchanged authorization look like a new
        // operator decision.
        let identity = (
            self.version,
            &self.task_id,
            &self.selectors,
            &self.context_files_sha256,
            &self.generation,
        );
        let encoded = serde_json::to_string(&identity).unwrap_or_default();
        format!("{:x}", Sha256::digest(encoded.as_bytes()))
    }
}

/// Order-insensitive digest of a task's `context_files`.
pub fn context_files_sha256(context_files: &[String]) -> String {
    let set = context_files.iter().collect::<BTreeSet<_>>();
    let encoded = serde_json::to_string(&set).unwrap_or_default();
    format!("{:x}", Sha256::digest(encoded.as_bytes()))
}

/// The creation authorization a task's history grants its current scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextCreationState {
    /// No grant was ever recorded.
    Absent,
    /// The latest grant is well formed, belongs to this task, and is bound to
    /// the task's current `context_files` and envelope revision.
    Current(ContextCreationGrant),
    /// The latest grant cannot be trusted for the current scope: it is
    /// malformed, names another task, or the scope changed without it.
    Void,
}

impl ContextCreationState {
    /// Resolve the state from a task's history, oldest first, as
    /// `(event, note)` pairs.
    pub fn resolve<'a>(
        task_id: &str,
        context_files: &[String],
        updated_at: DateTime<Utc>,
        events: impl DoubleEndedIterator<Item = (&'a str, Option<&'a str>)>,
    ) -> Self {
        let mut events = events.rev();
        let Some((_, note)) = events.find(|(event, _)| *event == CONTEXT_CREATION_AUTHORIZED_EVENT)
        else {
            return Self::Absent;
        };
        let Some(grant) = note.and_then(ContextCreationGrant::from_note) else {
            return Self::Void;
        };
        let scope = context_files.iter().collect::<BTreeSet<_>>();
        let canonical_order = grant.selectors.windows(2).all(|pair| pair[0] < pair[1]);
        if grant.version != CONTEXT_CREATION_GRANT_VERSION
            || grant.task_id != task_id
            || grant.context_files_sha256 != context_files_sha256(context_files)
            || grant.updated_at != Some(updated_at)
            || grant.selectors.len() > MAX_CONTEXT_CREATION_SELECTORS
            || !canonical_order
            || !grant
                .selectors
                .iter()
                .all(|selector| scope.contains(selector))
        {
            return Self::Void;
        }
        Self::Current(grant)
    }

    /// The selectors currently authorized for creation.
    pub fn selectors(&self) -> &[String] {
        match self {
            Self::Current(grant) => &grant.selectors,
            Self::Absent | Self::Void => &[],
        }
    }

    /// Identity of the current grant record, `None` when none applies.
    pub fn identity(&self) -> Option<String> {
        match self {
            Self::Current(grant) => Some(grant.identity()),
            Self::Absent | Self::Void => None,
        }
    }

    /// The grant a write replacing the scope with `next_context_files` must
    /// record, or `None` when the current record already says it.
    ///
    /// Grants for selectors the write keeps are retained, `authorize` adds
    /// new ones, and every other grant is revoked. Once a task has any grant
    /// record, every scope write re-binds it to the new scope, so a void grant
    /// is replaced by an explicit revocation and cannot revive when a later
    /// write restores the scope it was bound to. Errors name a selector
    /// authorized outside the written scope or a grant over
    /// [`MAX_CONTEXT_CREATION_SELECTORS`].
    pub fn next_grant(
        &self,
        task_id: &str,
        next_context_files: &[String],
        authorize: &[String],
        updated_at: DateTime<Utc>,
    ) -> Result<Option<ContextCreationGrant>, TaskError> {
        let scope = next_context_files.iter().collect::<BTreeSet<_>>();
        if let Some(outside) = authorize.iter().find(|selector| !scope.contains(selector)) {
            return Err(TaskError::Invalid(format!(
                "creation authorization for `{outside}` must name a selector the write stores in context_files"
            )));
        }
        let granted = self
            .selectors()
            .iter()
            .filter(|selector| scope.contains(selector))
            .chain(authorize)
            .cloned()
            .collect::<BTreeSet<_>>();
        if granted.len() > MAX_CONTEXT_CREATION_SELECTORS {
            return Err(TaskError::Invalid(format!(
                "a task may hold at most {MAX_CONTEXT_CREATION_SELECTORS} selectors authorized for creation; this write would hold {}",
                granted.len()
            )));
        }
        if granted.is_empty() && matches!(self, Self::Absent) {
            return Ok(None);
        }
        let mut next = ContextCreationGrant::new(task_id, granted, next_context_files, updated_at);
        match self {
            Self::Current(current)
                if current.version == next.version
                    && current.task_id == next.task_id
                    && current.selectors == next.selectors
                    && current.context_files_sha256 == next.context_files_sha256 =>
            {
                next.generation = current.generation.clone();
                if current.updated_at == next.updated_at {
                    Ok(None)
                } else {
                    Ok(Some(next))
                }
            }
            _ => Ok(Some(next)),
        }
    }
}
