//! Durable operation-mode authority and recovery accounting [ORB-11332].
//!
//! A grant is the one record that lets automation prepare, promote, or
//! complete work without asking again for each eligible task. It is separate
//! from preferences: a configured `autonomous` preset only describes desired
//! defaults, while a grant names the exact workspace, finite task scope,
//! rights, absolute expiry, limits, and the versioned effective policy that
//! was captured when it was enabled. Every privileged action rechecks the
//! grant at its authoritative boundary; nothing here caches a decision.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The reserved run-input key that carries a captured admission snapshot.
/// Only the trusted coordinator submission and the parent-authorized child
/// admission path may write it; ordinary submissions that set it are refused.
pub const OPERATION_ADMISSION_KEY: &str = "operation";

/// Longest admission window a grant may request, in seconds (24h). The same
/// ceiling the drain window enforces, so a grant cannot outlive the run it
/// authorizes by construction.
pub const MAX_GRANT_WINDOW_SECONDS: u64 = 86_400;

/// Largest finite task set one grant may name. V1 deliberately has no
/// dynamic or standing scope; a larger rollout is a second, explicit grant.
pub const MAX_GRANT_SCOPE_TASKS: usize = 50;

/// Which privileged actions a grant permits. Each right is separate: a grant
/// that prepares may not promote, and one that promotes may not complete.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRights {
    /// Schedule missing/stale preparation for in-scope tasks promptly.
    #[serde(default)]
    pub prepare: bool,
    /// Move fresh positively assessed proposed tasks in scope to backlog.
    #[serde(default)]
    pub promote: bool,
    /// Request the guarded `review -> done` transition for in-scope delivery.
    #[serde(default)]
    pub complete: bool,
}

impl GrantRights {
    /// Parse the CLI/MCP right names; unknown names are refused.
    pub fn parse(names: &[String]) -> Result<Self, String> {
        let mut rights = Self::default();
        for name in names {
            match name.trim() {
                "prepare" => rights.prepare = true,
                "promote" => rights.promote = true,
                "complete" => rights.complete = true,
                other => {
                    return Err(format!(
                        "unknown grant right '{other}'; expected prepare, promote, or complete"
                    ));
                }
            }
        }
        Ok(rights)
    }

    /// The granted right names, in canonical order.
    pub fn names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.prepare {
            names.push("prepare");
        }
        if self.promote {
            names.push("promote");
        }
        if self.complete {
            names.push("complete");
        }
        names
    }
}

/// The numeric bounds captured with the grant. They come from the effective
/// policy at enablement and never change afterwards; retuning preferences
/// affects future grants only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantLimits {
    /// Ceiling on concurrently live leaf runs under this grant.
    pub leaf_ceiling: u32,
    /// Due interval for automatic preparation of in-scope tasks, in seconds.
    pub preparation_due_seconds: u64,
    /// Recovery episodes allowed per task across step hooks and triage.
    pub recovery_episodes_per_task: u32,
    /// Recovery wall time allowed per task across step hooks and triage.
    pub recovery_minutes_per_task: u32,
}

/// Why a grant stopped admitting, or lost its privileges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantTransition {
    /// Who requested it.
    pub actor: String,
    /// The recorded reason, when one was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// When it was recorded.
    pub at: DateTime<Utc>,
}

/// The durable status of a grant. Expiry is not a status: it is derived from
/// `expires_at` at every check so a clock cannot be reset by a rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantStatus {
    /// Admits new work while `expires_at` is in the future.
    Active,
    /// Ordinary stop: no new admissions or promotion; admitted work keeps
    /// its captured bounds, including completion.
    Stopped,
    /// Hard revocation: no further privileged actions, including completion
    /// of admitted work.
    Revoked,
}

impl GrantStatus {
    /// Stable label for projections.
    pub fn as_str(self) -> &'static str {
        match self {
            GrantStatus::Active => "active",
            GrantStatus::Stopped => "stopped",
            GrantStatus::Revoked => "revoked",
        }
    }
}

/// Whether a grant currently admits *new* work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantAdmission {
    /// New admissions and promotion are allowed.
    Open,
    /// The absolute deadline passed. Admitted work retains its bounds.
    Expired,
    /// An operator stopped new admissions. Admitted work retains its bounds.
    Stopped,
    /// An operator revoked the grant. Admitted work loses privileged actions.
    Revoked,
}

impl GrantAdmission {
    /// The reason vocabulary shared by admission refusals and diagnostics.
    pub fn reason(self) -> &'static str {
        match self {
            GrantAdmission::Open => "open",
            GrantAdmission::Expired => "grant_expired",
            GrantAdmission::Stopped => "grant_stopped",
            GrantAdmission::Revoked => "grant_revoked",
        }
    }

    /// Whether new work may be admitted.
    pub fn admits(self) -> bool {
        matches!(self, GrantAdmission::Open)
    }
}

/// A durable, attributable authorization for scoped automation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationGrant {
    /// Stable identifier (`ogrant-...`).
    pub id: String,
    /// The workspace this grant covers. There is no federation-wide scope.
    pub workspace_id: String,
    /// Who enabled it.
    pub actor: String,
    /// The surface it came from (`cli`, `tool`, `dashboard`).
    pub source: String,
    /// When it was enabled.
    pub created_at: DateTime<Utc>,
    /// Absolute admission deadline. Never extended in place.
    pub expires_at: DateTime<Utc>,
    /// Compare-and-set handle; every accepted transition increments it.
    pub revision: u32,
    /// The finite task set this grant covers, sorted and deduplicated.
    pub task_ids: Vec<String>,
    /// Separate prepare/promote/complete rights.
    pub rights: GrantRights,
    /// Numeric bounds captured at enablement.
    pub limits: GrantLimits,
    /// The resolved effective policy captured once at enablement, including
    /// per-field provenance, so later diagnostics can explain what applied.
    pub policy: Value,
    /// Version of the captured policy shape.
    pub policy_version: u32,
    /// Durable status.
    pub status: GrantStatus,
    /// The stop transition, when one was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<GrantTransition>,
    /// The revocation, when one was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<GrantTransition>,
}

impl OperationGrant {
    /// Whether the grant admits new work at `now`.
    pub fn admission(&self, now: DateTime<Utc>) -> GrantAdmission {
        match self.status {
            GrantStatus::Revoked => GrantAdmission::Revoked,
            GrantStatus::Stopped => GrantAdmission::Stopped,
            GrantStatus::Active if now >= self.expires_at => GrantAdmission::Expired,
            GrantStatus::Active => GrantAdmission::Open,
        }
    }

    /// Whether already admitted work may still perform privileged actions.
    /// Only hard revocation withdraws them; stop and expiry do not.
    pub fn privileged_actions_allowed(&self) -> bool {
        self.status != GrantStatus::Revoked
    }

    /// Whether `task_id` is inside the finite scope.
    pub fn covers(&self, task_id: &str) -> bool {
        self.task_ids
            .binary_search_by(|id| id.as_str().cmp(task_id))
            .is_ok()
    }

    /// Seconds left before expiry at `now`, saturating at zero.
    pub fn remaining_seconds(&self, now: DateTime<Utc>) -> u64 {
        (self.expires_at - now)
            .num_seconds()
            .try_into()
            .unwrap_or(0)
    }

    /// Whether the grant currently allows the `review -> done` transition
    /// for work it admitted: it must carry the right and not be revoked.
    pub fn completion_allowed(&self) -> bool {
        self.rights.complete && self.privileged_actions_allowed()
    }
}

/// The subset of a grant a run carries in its immutable input under
/// [`OPERATION_ADMISSION_KEY`]. A child inherits exactly this from its parent
/// at the trusted admission path; it cannot acquire more by reading newer
/// configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationAdmission {
    /// The grant this run was admitted under.
    pub grant_id: String,
    /// The grant revision observed at admission.
    pub grant_revision: u32,
    /// Version of the captured policy shape. An unknown version fails closed
    /// for privileged actions.
    pub policy_version: u32,
    /// The absolute deadline captured at admission.
    pub expires_at: DateTime<Utc>,
    /// The effective completion captured for this admission after the
    /// delivery cap and rights applied (`review` or `done`).
    pub completion: String,
    /// Captured limits, inherited unchanged by children.
    pub limits: GrantLimits,
}

impl OperationAdmission {
    /// Read the snapshot carried by a run input, if any. A present but
    /// malformed snapshot is an error, never silently ignored.
    pub fn from_run_input(input: &Value) -> Result<Option<Self>, String> {
        let Some(raw) = input.get(OPERATION_ADMISSION_KEY) else {
            return Ok(None);
        };
        if raw.is_null() {
            return Ok(None);
        }
        serde_json::from_value(raw.clone())
            .map(Some)
            .map_err(|error| format!("invalid `{OPERATION_ADMISSION_KEY}` run input: {error}"))
    }
}

/// Which path consumed a recovery episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryEpisodeKind {
    /// An engine step recovery hook.
    StepRecovery,
    /// A terminal-run triage diagnosis and its resulting requeue.
    Triage,
}

/// One consumed recovery episode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryEpisode {
    /// One-based index within the task's lineage.
    pub index: u32,
    /// Which path consumed it.
    pub kind: RecoveryEpisodeKind,
    /// The run that triggered it.
    pub run_id: String,
    /// The failed step, for step recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    /// When it was reserved.
    pub reserved_at: DateTime<Utc>,
    /// Wall seconds recorded when the episode settled; `None` while open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
}

/// Aggregate recovery consumption for one task across step hooks, resumed
/// runs, and triage. Nesting or requeueing never resets it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryLedger {
    /// The task whose lineage this ledger covers.
    pub task_id: String,
    /// Every consumed episode, oldest first.
    pub episodes: Vec<RecoveryEpisode>,
    /// Settled wall seconds across all episodes.
    pub consumed_seconds: u64,
    /// Last write.
    pub updated_at: DateTime<Utc>,
}

impl RecoveryLedger {
    /// A fresh ledger for `task_id`.
    pub fn new(task_id: &str, now: DateTime<Utc>) -> Self {
        Self {
            task_id: task_id.to_string(),
            episodes: Vec::new(),
            consumed_seconds: 0,
            updated_at: now,
        }
    }

    /// Episodes consumed so far.
    pub fn episodes_consumed(&self) -> u32 {
        u32::try_from(self.episodes.len()).unwrap_or(u32::MAX)
    }
}

/// The outcome of asking for one more recovery episode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RecoveryReservation {
    /// An episode was reserved; the ledger reflects it durably.
    Reserved {
        /// The reserved episode index.
        episode: u32,
        /// Episodes still available after this one.
        remaining_episodes: u32,
        /// Wall seconds still available.
        remaining_seconds: u64,
    },
    /// The aggregate allowance is spent; the caller must escalate.
    Exhausted {
        /// `recovery_episodes_exhausted` or `recovery_minutes_exhausted`.
        reason: &'static str,
        /// Episodes consumed so far.
        episodes_consumed: u32,
        /// Wall seconds consumed so far.
        consumed_seconds: u64,
    },
}
