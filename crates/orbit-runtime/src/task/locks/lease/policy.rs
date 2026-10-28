//! `lease.*` policy as admitted, the caller's principal class, and the mode
//! and priority values the policy and requests are parsed into.

use orbit_common::OrbitError;
use orbit_store::contracts::{LeaseMode, LeasePrincipalClass, LeasePriority};
use serde_json::json;

/// `lease.*` as admitted: whether leases are accepted, the mode a request
/// that names none gets, and the per-principal-class guards.
#[derive(Clone, Debug)]
pub(crate) struct LeasePolicy {
    enabled: bool,
    pub(super) default_mode: LeaseMode,
    hard_allowed_human: bool,
    hard_allowed_agent: bool,
    max_priority_human: LeasePriority,
    max_priority_agent: LeasePriority,
    pub(super) aging_minutes: u32,
}

impl LeasePolicy {
    /// Read the policy from admitted settings. Config admission already
    /// refused any value outside each key's choices.
    pub(crate) fn admit(snapshot: &orbit_config::ConfigSnapshot) -> Result<Self, OrbitError> {
        Ok(Self {
            enabled: snapshot.lease_enabled,
            default_mode: parse_mode(&snapshot.lease_default_mode, "lease.default_mode")?,
            hard_allowed_human: hard_allowed(&snapshot.lease_hard_human, "lease.hard.human")?,
            hard_allowed_agent: hard_allowed(&snapshot.lease_hard_agent, "lease.hard.agent")?,
            max_priority_human: parse_priority(
                &snapshot.lease_max_priority_human,
                "lease.max_priority.human",
            )?,
            max_priority_agent: parse_priority(
                &snapshot.lease_max_priority_agent,
                "lease.max_priority.agent",
            )?,
            aging_minutes: snapshot.lease_aging_minutes,
        })
    }

    pub(super) fn ensure_enabled(&self) -> Result<(), OrbitError> {
        if self.enabled {
            return Ok(());
        }
        Err(OrbitError::InvalidInput(
            "path leases are disabled in this workspace; set `lease.enabled = true` to accept \
             lease operations"
                .to_string(),
        ))
    }

    pub(super) fn hard_allowed(&self, class: LeasePrincipalClass) -> bool {
        match class {
            LeasePrincipalClass::Human => self.hard_allowed_human,
            LeasePrincipalClass::Agent => self.hard_allowed_agent,
        }
    }

    pub(super) fn max_priority(&self, class: LeasePrincipalClass) -> LeasePriority {
        match class {
            LeasePrincipalClass::Human => self.max_priority_human,
            LeasePrincipalClass::Agent => self.max_priority_agent,
        }
    }
}

/// The principal class of the calling context: `agent` when the process
/// declares an agent or a managed run (the signal
/// `agent_context_declared` reads), otherwise `human`.
pub fn lease_principal_class() -> LeasePrincipalClass {
    if orbit_common::governance::authorization::agent_context_declared() {
        LeasePrincipalClass::Agent
    } else {
        LeasePrincipalClass::Human
    }
}

pub(super) fn parse_mode(raw: &str, field: &str) -> Result<LeaseMode, OrbitError> {
    serde_json::from_value(json!(raw.trim())).map_err(|_| {
        OrbitError::InvalidInput(format!(
            "`{field}` must be `advisory` or `hard`; got `{raw}`"
        ))
    })
}

pub(super) fn parse_priority(raw: &str, field: &str) -> Result<LeasePriority, OrbitError> {
    serde_json::from_value(json!(raw.trim())).map_err(|_| {
        OrbitError::InvalidInput(format!(
            "`{field}` must be `low`, `normal` or `high`; got `{raw}`"
        ))
    })
}

fn hard_allowed(raw: &str, key: &str) -> Result<bool, OrbitError> {
    match raw {
        "allow" => Ok(true),
        "never" => Ok(false),
        other => Err(OrbitError::InvalidInput(format!(
            "`{key}` must be `allow` or `never`; got `{other}`"
        ))),
    }
}

pub(super) fn priority_rank(priority: LeasePriority) -> u8 {
    match priority {
        LeasePriority::Low => 0,
        LeasePriority::Normal => 1,
        LeasePriority::High => 2,
    }
}

pub(super) fn lower_priority(left: LeasePriority, right: LeasePriority) -> LeasePriority {
    if priority_rank(left) <= priority_rank(right) {
        left
    } else {
        right
    }
}
