//! Principals, classes, verbs and scopes, and the allow-or-deny decision.
//!
//! A token names one principal and its class, the workspaces and verbs it may
//! use, its priority ceiling and its `hard` permission. The decision for a
//! presented token is made here; the listener and the intake socket apply the
//! scope it returns together with the workspace's own class policy.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::validate_registry_identifier;
use serde::{Deserialize, Serialize};

/// Longest principal name, `person` or `person/agent`.
const PRINCIPAL_MAX_BYTES: usize = 128;

/// Who a token's principal is: a person, or an agent acting for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalClass {
    /// A person.
    Human,
    /// An agent, named under its person (`alice/claude-code`).
    Agent,
}

impl PrincipalClass {
    /// The stored and displayed spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }

    /// The ceiling a token of this class gets when none is named, matching
    /// the `lease.max_priority` defaults.
    pub fn default_max_priority(self) -> LeasePriorityCeiling {
        match self {
            Self::Human => LeasePriorityCeiling::High,
            Self::Agent => LeasePriorityCeiling::Normal,
        }
    }
}

impl FromStr for PrincipalClass {
    type Err = OrbitError;
    fn from_str(raw: &str) -> Result<Self, OrbitError> {
        match raw {
            "human" => Ok(Self::Human),
            "agent" => Ok(Self::Agent),
            other => Err(invalid(format!(
                "unknown principal class '{other}' (expected human or agent)"
            ))),
        }
    }
}

/// What a token may do on the listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseTokenVerb {
    /// Show, list, check and read notices.
    Read,
    /// Request, renew, expand and release leases.
    Lease,
}

impl LeaseTokenVerb {
    /// The stored and displayed spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Lease => "lease",
        }
    }

    /// The verbs a `--verbs` grant gives: `lease` includes `read`, since a
    /// holder has to see the leases it takes.
    pub fn granted_by(self) -> Vec<LeaseTokenVerb> {
        match self {
            Self::Read => vec![Self::Read],
            Self::Lease => vec![Self::Read, Self::Lease],
        }
    }
}

impl FromStr for LeaseTokenVerb {
    type Err = OrbitError;
    fn from_str(raw: &str) -> Result<Self, OrbitError> {
        match raw {
            "read" => Ok(Self::Read),
            "lease" => Ok(Self::Lease),
            other => Err(invalid(format!(
                "unknown lease token verb '{other}' (expected read or lease)"
            ))),
        }
    }
}

/// The highest lease priority a token may request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeasePriorityCeiling {
    /// Only `low`.
    Low,
    /// Up to `normal`.
    Normal,
    /// Up to `high`.
    High,
}

impl LeasePriorityCeiling {
    /// The stored and displayed spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
        }
    }
}

impl FromStr for LeasePriorityCeiling {
    type Err = OrbitError;
    fn from_str(raw: &str) -> Result<Self, OrbitError> {
        match raw {
            "low" => Ok(Self::Low),
            "normal" => Ok(Self::Normal),
            "high" => Ok(Self::High),
            other => Err(invalid(format!(
                "unknown lease priority '{other}' (expected low, normal or high)"
            ))),
        }
    }
}

/// Whether a token may hold hard leases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardLeasePermission {
    /// Hard requests are granted as hard, within workspace policy.
    Allow,
    /// Hard requests are granted as advisory.
    Never,
}

impl HardLeasePermission {
    /// The stored and displayed spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Never => "never",
        }
    }
}

impl FromStr for HardLeasePermission {
    type Err = OrbitError;
    fn from_str(raw: &str) -> Result<Self, OrbitError> {
        match raw {
            "allow" => Ok(Self::Allow),
            "never" => Ok(Self::Never),
            other => Err(invalid(format!(
                "unknown hard permission '{other}' (expected allow or never)"
            ))),
        }
    }
}

/// Everything a token authorizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseTokenScope {
    /// `person`, or `person/agent` for an agent's token.
    pub principal: String,
    /// The principal's class.
    pub principal_class: PrincipalClass,
    /// Workspaces the token may address.
    pub workspaces: Vec<String>,
    /// Verbs the token may use.
    pub verbs: Vec<LeaseTokenVerb>,
    /// Highest priority it may request.
    pub max_priority: LeasePriorityCeiling,
    /// Whether it may hold hard leases.
    pub hard: HardLeasePermission,
}

impl LeaseTokenScope {
    /// Validate a scope before it is minted.
    ///
    /// A human principal is one name; an agent's is `person/agent`, so its
    /// notices and audit rows say whose session it is.
    pub fn validated(mut self) -> Result<Self, OrbitError> {
        validate_principal(&self.principal, self.principal_class)?;
        if self.workspaces.is_empty() {
            return Err(invalid(
                "a lease token needs at least one workspace".to_string(),
            ));
        }
        for workspace in &self.workspaces {
            validate_registry_identifier("workspace", workspace)?;
            if workspace.contains([',', ' ']) {
                return Err(invalid(format!(
                    "workspace '{workspace}' must not contain commas or spaces"
                )));
            }
        }
        self.workspaces.sort();
        self.workspaces.dedup();
        if self.verbs.is_empty() {
            return Err(invalid("a lease token needs at least one verb".to_string()));
        }
        self.verbs.sort();
        self.verbs.dedup();
        Ok(self)
    }

    /// Whether this scope covers `verb` on `workspace`.
    pub fn permits(&self, workspace: &str, verb: LeaseTokenVerb) -> Result<(), LeaseTokenRefusal> {
        if !self.workspaces.iter().any(|allowed| allowed == workspace) {
            return Err(LeaseTokenRefusal::WrongWorkspace);
        }
        if !self.verbs.contains(&verb) {
            return Err(LeaseTokenRefusal::VerbNotGranted);
        }
        Ok(())
    }
}

/// Why a presented token was refused. Every variant is the same to the
/// caller on the wire; the distinction is for the audit row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseTokenRefusal {
    /// No token has this secret, or it is not a token at all.
    Unknown,
    /// The token was revoked.
    Revoked,
    /// The token's expiry has passed.
    Expired,
    /// The token does not name the requested workspace.
    WrongWorkspace,
    /// The token does not grant the requested verb.
    VerbNotGranted,
}

impl LeaseTokenRefusal {
    /// The stored and displayed spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::WrongWorkspace => "wrong_workspace",
            Self::VerbNotGranted => "verb_not_granted",
        }
    }
}

impl fmt::Display for LeaseTokenRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A stored token, without its secret or hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseTokenRecord {
    /// Public identifier, used to list and revoke.
    pub token_id: String,
    /// What the token authorizes.
    #[serde(flatten)]
    pub scope: LeaseTokenScope,
    /// When it was minted.
    pub created_at: DateTime<Utc>,
    /// When it stops verifying.
    pub expires_at: DateTime<Utc>,
    /// The last successful verification, if any.
    pub last_used_at: Option<DateTime<Utc>>,
    /// When it was revoked, if it was.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl LeaseTokenRecord {
    /// The allow-or-deny decision for this token on `verb` in `workspace`.
    pub fn decide(
        &self,
        workspace: &str,
        verb: LeaseTokenVerb,
        now: DateTime<Utc>,
    ) -> Result<(), LeaseTokenRefusal> {
        if self.revoked_at.is_some() {
            return Err(LeaseTokenRefusal::Revoked);
        }
        if self.expires_at <= now {
            return Err(LeaseTokenRefusal::Expired);
        }
        self.scope.permits(workspace, verb)
    }
}

fn validate_principal(principal: &str, class: PrincipalClass) -> Result<(), OrbitError> {
    if principal.is_empty() || principal.len() > PRINCIPAL_MAX_BYTES {
        return Err(invalid(format!(
            "principal must be 1 to {PRINCIPAL_MAX_BYTES} bytes"
        )));
    }
    let segments: Vec<&str> = principal.split('/').collect();
    let valid_segment = |segment: &&str| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    };
    if !segments.iter().all(valid_segment) {
        return Err(invalid(format!(
            "principal '{principal}' may use only letters, digits, '.', '_' and '-', with '/' \
             between a person and their agent"
        )));
    }
    match (class, segments.len()) {
        (PrincipalClass::Human, 1) | (PrincipalClass::Agent, 2) => Ok(()),
        (PrincipalClass::Human, _) => Err(invalid(format!(
            "a human principal is one name, not '{principal}'; name an agent's token \
             `person/agent` with --class agent"
        ))),
        (PrincipalClass::Agent, _) => Err(invalid(format!(
            "an agent principal is named under its person, as `person/agent` \
             (for example alice/claude-code), not '{principal}'"
        ))),
    }
}

fn invalid(message: String) -> OrbitError {
    OrbitError::InvalidInput(message)
}
