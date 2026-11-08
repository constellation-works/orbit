//! The path-lease policy, the caller's principal class, and the mode and
//! priority values the policy and requests are parsed into.
//!
//! Path leases are not configurable [ORB-22808]: the `lease.*` keys were
//! withdrawn, so every runtime holds the fixed policy below, which refuses
//! every lease operation. The machinery stays for a later revival; its tests
//! reach it through `install_test_lease_policy` under `test-support`.

use orbit_common::OrbitError;
use orbit_store::contracts::{LeaseMode, LeasePrincipalClass, LeasePriority};
use serde_json::json;

/// The former `lease.enabled` default, fixed when the key was removed [ORB-22808].
pub const PATH_LEASES_ENABLED: bool = false;
/// The former `lease.default_mode` default, fixed when the key was removed [ORB-22808].
pub const LEASE_DEFAULT_MODE: LeaseMode = LeaseMode::Advisory;
/// The former `lease.hard.human` default (`allow`), fixed when the key was removed [ORB-22808].
pub const LEASE_HARD_ALLOWED_HUMAN: bool = true;
/// The former `lease.hard.agent` default (`allow`), fixed when the key was removed [ORB-22808].
pub const LEASE_HARD_ALLOWED_AGENT: bool = true;
/// The former `lease.max_priority.human` default, fixed when the key was removed [ORB-22808].
pub const LEASE_MAX_PRIORITY_HUMAN: LeasePriority = LeasePriority::High;
/// The former `lease.max_priority.agent` default, fixed when the key was removed [ORB-22808].
pub const LEASE_MAX_PRIORITY_AGENT: LeasePriority = LeasePriority::Normal;
/// The former `lease.aging_minutes` default, fixed when the key was removed [ORB-22808].
pub const LEASE_AGING_MINUTES: u32 = 30;

/// The lease policy a runtime holds: whether leases are accepted, the mode a
/// request that names none gets, and the per-principal-class guards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeasePolicy {
    /// Whether lease operations are accepted at all.
    pub enabled: bool,
    /// The mode of a request that names none.
    pub default_mode: LeaseMode,
    /// Whether a human principal may hold hard leases; `false` grants them
    /// as advisory leases.
    pub hard_allowed_human: bool,
    /// Whether an agent principal may hold hard leases.
    pub hard_allowed_agent: bool,
    /// Highest priority a human principal may request.
    pub max_priority_human: LeasePriority,
    /// Highest priority an agent principal may request.
    pub max_priority_agent: LeasePriority,
    /// Minutes a queued hard-lease request or expansion waits before it
    /// moves up one priority level.
    pub aging_minutes: u32,
}

impl LeasePolicy {
    /// The fixed policy: path leases disabled, the guards at their former
    /// defaults.
    pub const FIXED: Self = Self {
        enabled: PATH_LEASES_ENABLED,
        default_mode: LEASE_DEFAULT_MODE,
        hard_allowed_human: LEASE_HARD_ALLOWED_HUMAN,
        hard_allowed_agent: LEASE_HARD_ALLOWED_AGENT,
        max_priority_human: LEASE_MAX_PRIORITY_HUMAN,
        max_priority_agent: LEASE_MAX_PRIORITY_AGENT,
        aging_minutes: LEASE_AGING_MINUTES,
    };

    /// The policy a runtime over `orbit_dir` opens with: [`Self::FIXED`], or
    /// under `test-support` the policy `install_test_lease_policy` set for
    /// that directory.
    pub(crate) fn admit(orbit_dir: &std::path::Path) -> Self {
        #[cfg(feature = "test-support")]
        if let Some(policy) = test_lease_policy(orbit_dir) {
            return policy;
        }
        let _ = orbit_dir;
        Self::FIXED
    }

    /// Whether this runtime accepts lease operations.
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(super) fn ensure_enabled(&self) -> Result<(), OrbitError> {
        if self.enabled {
            return Ok(());
        }
        Err(OrbitError::InvalidInput(
            "path leases are disabled: they are not configurable on this Orbit".to_string(),
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

#[cfg(feature = "test-support")]
type TestLeasePolicies = std::collections::HashMap<std::path::PathBuf, LeasePolicy>;

#[cfg(feature = "test-support")]
static TEST_LEASE_POLICIES: std::sync::Mutex<Option<TestLeasePolicies>> =
    std::sync::Mutex::new(None);

#[cfg(feature = "test-support")]
fn test_lease_policy_key(orbit_dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(orbit_dir).unwrap_or_else(|_| orbit_dir.to_path_buf())
}

#[cfg(feature = "test-support")]
fn test_lease_policy(orbit_dir: &std::path::Path) -> Option<LeasePolicy> {
    TEST_LEASE_POLICIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()?
        .get(&test_lease_policy_key(orbit_dir))
        .cloned()
}

/// Make every runtime this process opens afterwards over the workspace at
/// `orbit_dir` (its `.orbit` directory) hold `policy` instead of
/// [`LeasePolicy::FIXED`]. A runtime keeps the policy it opened with;
/// installing again for the same directory replaces the policy.
///
/// For lease machinery tests only; no operator surface reaches it. Keying
/// by directory keeps concurrent tests in one process apart.
#[cfg(feature = "test-support")]
pub fn install_test_lease_policy(orbit_dir: &std::path::Path, policy: LeasePolicy) {
    TEST_LEASE_POLICIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(Default::default)
        .insert(test_lease_policy_key(orbit_dir), policy);
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
