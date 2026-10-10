//! Delivery ownership and direct-landing facts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// How the effective owner of a delivery consumer was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerAuthority {
    /// The definition names `owner_machine` explicitly.
    Definition,
    /// Inherited from the registered owner of this workspace, which is
    /// authoritative whenever the definition omits an owner.
    Workspace,
    /// Nothing names an owner: the workspace record predates host identity,
    /// or this checkout is not registered.
    Missing,
    /// The workspace record and this checkout's replica role name different
    /// owners, so neither may be trusted.
    Conflicting,
}

/// Effective ownership of one delivery consumer on this host. Preview,
/// inspection and real evaluation all report it, so "no admission here" is
/// never indistinguishable from a definition the operator disabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryOwnership {
    /// The machine allowed to admit work, when one could be resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_machine: Option<String>,
    pub authority: OwnerAuthority,
    /// True only when this host is the resolved owner. Admission is
    /// impossible otherwise, whatever the definition's `enabled` says.
    pub owned_here: bool,
}

impl DeliveryOwnership {
    /// Scheduling reason for an enabled definition this host may not admit
    /// work for; `None` when this host is the owner.
    pub fn refusal(&self) -> Option<&'static str> {
        if self.owned_here {
            return None;
        }

        Some(match self.authority {
            OwnerAuthority::Definition | OwnerAuthority::Workspace => "owned_elsewhere",
            OwnerAuthority::Missing | OwnerAuthority::Conflicting => "ownership_unresolved",
        })
    }
}

/// Deterministic delivery-owner facts captured before attempting a direct landing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectLandingRequest {
    pub run_id: String,
    pub branch: String,
    pub before_commit: String,
    pub after_commit: String,
    /// The tasks this landing delivers, when the landing step reads them from
    /// owner state: a handoff landing names its accepted handoff's task. Empty
    /// means the run's own input names them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ids: Vec<String>,
    /// The accepted handoff a landing job lands. It identifies the delivery in
    /// place of the run, so a retried attempt re-records the same intent
    /// rather than a second one over the same commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_id: Option<String>,
}

/// Provider association retained while a complete PR landing span is unresolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryAssociation {
    pub key: String,
    pub anchor: String,
    pub reference: String,
    pub landed_at: DateTime<Utc>,
    /// The head commit the provider reports the merged pull request landed.
    /// Absent on associations recorded before it was kept and on identities
    /// rebuilt from a pending delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}
